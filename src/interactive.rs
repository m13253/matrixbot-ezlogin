use std::path::Path;

use eyre::{Result, bail};
use matrix_sdk::Client;
use tracing::instrument;

use crate::{DuplexLog, SetupConfig, setup};

/// Set up a Matrix bot account by asking credentials through the terminal interactively.
///
/// It creates a new session, saves it for later [`login`](crate::login) use, then exits.
///
/// # Arguments
///
/// * `data_dir`: A directory to store the bot's state database.
///
///   Later [`login`](crate::login) calls need to use the same directory.
///
///   One directory can only store one session.
///
/// * `device_name`: Any descriptive text to distinguish this session with other sessions logged in at different locations.
#[instrument(skip_all)]
pub async fn setup_interactive(data_dir: &Path, device_name: &str) -> Result<Client> {
    let homeserver = DuplexLog::readline("Matrix homeserver: ").await?;
    let username = DuplexLog::readline("User name: ").await?;
    let password = DuplexLog::readline("Password: ").await?;
    let config = SetupConfig {
        data_dir,
        homeserver: &homeserver,
        username: &username,
        password: &password,
        device_name,
        ask_recovery_key: async { Ok(DuplexLog::readline("Backup recovery key: ").await?) },
        before_create_backup: async {
            if DuplexLog::readline("Are you ready to reset the cryptographic identity to enable server-side backup (y/n)? ")
                .await
                .map(|resp| resp.eq_ignore_ascii_case("y"))
                .unwrap_or(false)
            {
                Ok(())
            } else {
                bail!("backup canceled by user")
            }
        },
        print_recovery_key: async |mut recovery_key: String, _new_backup: bool| {
            let recovery_key_path = data_dir.join("recovery-key.txt");
            recovery_key.push('\n');
            tokio::fs::write(&recovery_key_path, &recovery_key).await?;
            _ = DuplexLog::readline(format!(
                "Please move {} to a safe place, then press ENTER to continue: ",
                recovery_key_path.as_os_str().to_string_lossy()
            ))
            .await;
            Ok(())
        },
    };
    let client = setup(config).await?;
    Ok(client)
}
