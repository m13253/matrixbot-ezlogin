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

#[derive(Clone)]
pub struct SetupConfig<'a, F1, F2, F3> {
    pub data_dir: &'a Path,
    pub homeserver: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub device_name: &'a str,
    pub ask_recovery_key: F1,
    pub before_create_backup: F2,
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
        .ok_or_eyre("Matrix SDK did not return a session")?;
    let AuthSession::Matrix(matrix_session) = session else {
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
        info!("Recovered from the server backup.");
    } else {
        // What if at this specific moment, another client also wants to create a backup?
        // This is rarely an issue with human users, but can be problematic for bots with sharded backends.
        // As the code in the SDK doesn't deal with this race condition, we can do nothing here.
        // If that happens, maybe we will have to forcefully reset the backup.

        info!("No backup exists on the server, creating a new one.");
        config.before_create_backup.await?;

        info!("Resetting cryptography identity.");
        if let Some(reset_handle) = recovery.reset_identity().await? {
            match reset_handle.auth_type() {
                CrossSigningResetAuthType::Uiaa(uiaa) => {
                    info!("Setting up cross-signing. (Stage 2: UIAA)");
                    let mut auth_data = uiaa::Password::new(
                        client
                            .user_id()
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
    session_db.execute(
        "INSERT INTO recovery_key (id, key) VALUES (0, ?);",
        (&recovery_key,),
    )?;
    if !has_backup {
        (config.print_recovery_key)(recovery_key).await?;
    }

    Ok(())
}

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
        .ok_or_eyre("no session found, run setup first")?;
    let recovery_key: String = session_db
        .query_row("SELECT key FROM recovery_key WHERE id = 0;", (), |row| {
            row.get(0)
        })
        .optional()?
        .ok_or_eyre("no recovery key stored, reset and run setup first")?;
    let matrix_session = serde_json::from_str::<MatrixSession>(&session)?;

    info!("Logging into Matrix.");
    let client = build_client(data_dir, &homeserver, &passphrase).await?;
    client
        .restore_session(AuthSession::Matrix(matrix_session))
        .await?;

    info!("Recovering from the backup.");
    client
        .encryption()
        .recovery()
        .recover(&recovery_key)
        .await?;

    info!("Login finished.");
    Ok(client)
}
