use std::path::Path;

use color_eyre::eyre::{OptionExt, Result, bail};
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::encryption::{
    BackupDownloadStrategy, CrossSigningResetAuthType, EncryptionSettings,
};
use matrix_sdk::ruma::api::client::uiaa;
use matrix_sdk::{AuthSession, Client};
use rand::Rng;
use rusqlite::{OpenFlags, OptionalExtension};
use scopeguard::guard;
use tracing::{info, instrument};

use super::connect_sqlite;

/// Information to set up a Matrix bot using [`setup`].
#[derive(Clone)]
pub struct SetupConfig<'a, F1, F2, F3> {
    /// A directory to store the bot's state database.
    ///
    /// Later [`login`] calls need to use the same directory.
    ///
    /// One directory can only store one session.
    pub data_dir: &'a Path,
    /// The Matrix homeserver.
    ///
    /// Supports server name (`matrix.org`), or base URL (`https://matrix-client.matrix.org`).
    pub homeserver: &'a str,
    /// The user name.
    ///
    /// Supports localpart `example` or full ID (`@example:matrix.org`).
    pub username: &'a str,
    /// The password.
    ///
    /// matrixbot-ezlogin does not support multi-factor authentication or single sign-on, as bots are designed to run unattended.
    pub password: &'a str,
    /// Any descriptive text to distinguish this session with other sessions logged in at different locations.
    pub device_name: &'a str,
    /// An `async` block that asks the user to supply a recovery key and returns [`Result<String, Report>`](Result).
    ///
    /// Alternatively, you can use [`setup_interactive`](crate::setup_interactive), which provides a built-in implementation.
    pub ask_recovery_key: F1,
    /// An `async` block that asks the user to confirm before creating a backup and returns [`Result<(), Report>`](Result).
    ///
    /// Creating the initial backup also resets the account's cryptographic identity.
    ///
    /// If it returns [`Result::Err`], the setup process will be aborted and no backups will be created.
    ///
    /// Alternatively, you can use [`setup_interactive`](crate::setup_interactive), which provides a built-in implementation.
    pub before_create_backup: F2,
    /// An `async fn(recovery_key: String) -> Result<(), Report>` that asks the user to keep the recovery key in a safe place.
    ///
    /// Currently, matrixbot-ezlogin also saves a copy of the recovery key into the `matrixbot-ezlogin.sqlite` database, but it's subject to change.
    ///
    /// If you lost your recovery key, you may not be able to set up a new session without resetting the cryptographic identity.
    ///
    /// Alternatively, you can use [`setup_interactive`](crate::setup_interactive), which provides a built-in implementation.
    pub print_recovery_key: F3,
}

async fn build_client(data_dir: &Path, homeserver: &str, passphrase: &str) -> Result<Client> {
    let mut client_builder = Client::builder()
        .server_name_or_homeserver_url(homeserver)
        .sqlite_store(data_dir, Some(passphrase))
        .with_encryption_settings(EncryptionSettings {
            auto_enable_cross_signing: true,
            backup_download_strategy: BackupDownloadStrategy::AfterDecryptionFailure,
            auto_enable_backups: true,
        });

    if let Some((_, proxy)) =
        std::env::vars_os().find(|(k, _)| k.eq_ignore_ascii_case("https_proxy"))
    {
        client_builder = client_builder.proxy(
            proxy
                .to_str()
                .expect("invalid environment variable https_proxy"),
        );
    }
    Ok(client_builder.build().await?)
}

/// Set up a Matrix bot account by providing credentials through a `SetupConfig`.
///
/// It creates a new session, saves it for later [`login`] use, then exits.
///
/// Alternatively, [`setup_interactive`](crate::setup_interactive) provides an interactive version.
#[instrument(skip_all)]
pub async fn setup<F1, F2, F3, R3>(config: SetupConfig<'_, F1, F2, F3>) -> Result<Client>
where
    F1: Future<Output = Result<String>>,
    F2: Future<Output = Result<()>>,
    F3: FnOnce(String) -> R3,
    R3: Future<Output = Result<()>>,
{
    tokio::fs::create_dir_all(&config.data_dir).await?;

    let session_db = guard(
        connect_sqlite(
            &config.data_dir.join("matrixbot-ezlogin.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?,
        |session_db| {
            _ = session_db.execute("PRAGMA optimize;", ());
        },
    );
    session_db.execute_batch(
        "BEGIN TRANSACTION;
DROP TABLE IF EXISTS matrix_session;
DROP TABLE IF EXISTS recovery_key;
DROP TABLE IF EXISTS sync_token;
CREATE TABLE matrix_session (id INTEGER PRIMARY KEY CHECK (id = 0), homeserver TEXT NOT NULL, passphrase TEXT NOT NULL, session BLOB NOT NULL);
CREATE TABLE recovery_key (id INTEGER PRIMARY KEY CHECK (id = 0), key TEXT NOT NULL);
CREATE TABLE sync_token (id INTEGER PRIMARY KEY CHECK (id = 0), token TEXT NOT NULL);
COMMIT;",
    )?;

    let rng = rand::rng();
    let db_passphrase = rng
        .sample_iter(rand::distr::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect::<String>();
    for file_name in [
        "matrix-sdk-crypto.sqlite3",
        "matrix-sdk-event-cache.sqlite3",
        "matrix-sdk-state.sqlite3",
    ] {
        _ = tokio::fs::remove_file(&config.data_dir.join(file_name)).await;
    }

    info!("Logging into Matrix.");
    let client: Client = build_client(config.data_dir, config.homeserver, &db_passphrase).await?;
    client
        .matrix_auth()
        .login_username(config.username, config.password)
        .initial_device_display_name(config.device_name)
        .await?;

    match save_session(config, &session_db, db_passphrase, &client).await {
        Ok(_) => {
            info!("Setup finished.");
            Ok(client)
        }
        Err(err) => {
            info!("Logging out of Matrix.");
            client.logout().await?;
            Err(err)?
        }
    }
}

async fn save_session<F1, F2, F3, R3>(
    config: SetupConfig<'_, F1, F2, F3>,
    session_db: &rusqlite::Connection,
    db_passphrase: String,
    client: &Client,
) -> Result<()>
where
    F1: Future<Output = Result<String>>,
    F2: Future<Output = Result<()>>,
    F3: FnOnce(String) -> R3,
    R3: Future<Output = Result<()>>,
{
    info!("Saving the Matrix session.");
    let session = client
        .session()
        // TODO: If anyone needs, transform these ad-hoc errors into named error types.
        .ok_or_eyre("Matrix SDK did not return a session")?;
    let AuthSession::Matrix(matrix_session) = session else {
        // TODO: If anyone needs, transform these ad-hoc errors into named error types.
        bail!("Matrix SDK returned an unsupported session type");
    };
    let session_json = serde_json::to_string(&matrix_session)?;
    session_db.execute(
        "INSERT INTO matrix_session (id, homeserver, passphrase, session) VALUES (0, ?, ?, jsonb(?));",
        (client.homeserver().as_str(), db_passphrase, &session_json),
    )?;

    info!("Setting up encryption.");
    let encryption = client.encryption();
    let has_backup = encryption.backups().fetch_exists_on_server().await?;
    let recovery = encryption.recovery();
    encryption.wait_for_e2ee_initialization_tasks().await;

    let recovery_key: String;
    if has_backup {
        info!("A backup exists on the server, recovering from it.");
        recovery_key = config.ask_recovery_key.await?;
        recovery.recover(&recovery_key).await?;
        encryption.wait_for_e2ee_initialization_tasks().await;
        info!("Recovered from the server backup.");
    } else {
        // What if at this specific moment, another client also wants to create a backup?
        // This is rarely an issue with human users, but can be problematic for bots with sharded backends.
        // As the code in the SDK doesn't deal with this race condition, we can do nothing here.
        // If that happens, maybe the user just needs to forcefully reset the cryptographic identity and rerun the setup.

        info!("No backup exists on the server, creating a new one.");
        config.before_create_backup.await?;

        info!("Resetting cryptography identity.");
        if let Some(reset_handle) = recovery.reset_identity().await? {
            match reset_handle.auth_type() {
                CrossSigningResetAuthType::Uiaa(uiaa) => {
                    info!("Resetting cryptography identity. (Stage 2: UIAA)");
                    let mut auth_data = uiaa::Password::new(
                        client
                            .user_id()
                            // TODO: If anyone needs, transform these ad-hoc errors into named error types.
                            .ok_or_eyre("failed to get user ID")?
                            .to_owned()
                            .into(),
                        config.password.to_owned(),
                    );
                    auth_data.session = uiaa.session.clone();
                    reset_handle
                        .reset(Some(uiaa::AuthData::Password(auth_data)))
                        .await?;
                }
                CrossSigningResetAuthType::OAuth(oauth) => {
                    eprintln!(
                        "To reset your end-to-end encryption cross-signing identity, you first need to approve it at: {}",
                        oauth.approval_url
                    );
                    reset_handle.reset(None).await?;
                }
            }
        }
        encryption.wait_for_e2ee_initialization_tasks().await;

        info!("Creating a server backup.");
        recovery_key = recovery.enable().wait_for_backups_to_upload().await?;
        info!("Finished initial backup.");
    }

    info!("Saving the recovery key.");
    // Currently, matrixbot-ezlogin also saves a copy of the recovery key into the `matrixbot-ezlogin.sqlite` database, but it's subject to change.
    // If you lost your recovery key, you may not be able to set up a new session without resetting the cryptographic identity.
    session_db.execute(
        "INSERT INTO recovery_key (id, key) VALUES (0, ?);",
        (&recovery_key,),
    )?;
    if !has_backup {
        (config.print_recovery_key)(recovery_key).await?;
    }

    Ok(())
}

/// Log in and restore a Matrix session from a state database saved by [`setup`] or [`setup_interactive`](crate::setup_interactive).
///
/// # Arguments
///
/// * `data_dir`, The directory containing the bot's state database.
///
///   It must be already initialized by a successful [`setup`] or [`setup_interactive`](crate::setup_interactive) call.
///
///   Only one process can use a directory at the same time.
///
///   If you need to connect two processes to the same Matrix account, run [`setup`] or [`setup_interactive`](crate::setup_interactive) using two different `data_dir`.
#[instrument(skip_all)]
pub async fn login(data_dir: &Path) -> Result<Client> {
    let session_db = connect_sqlite(
        &data_dir.join("matrixbot-ezlogin.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;

    let (homeserver, passphrase, session): (String, String, String) = session_db
        .query_row(
            "SELECT homeserver, passphrase, json(session) FROM matrix_session WHERE id = 0;",
            (),
            |row| row.try_into(),
        )
        .optional()?
        // TODO: If anyone needs, transform these ad-hoc errors into named error types.
        .ok_or_eyre("no session found, run setup first")?;
    let matrix_session = serde_json::from_str::<MatrixSession>(&session)?;

    info!("Logging into Matrix.");
    let client = build_client(data_dir, &homeserver, &passphrase).await?;
    client
        .restore_session(AuthSession::Matrix(matrix_session))
        .await?;

    info!("Login finished.");
    Ok(client)
}
