use std::path::Path;
use std::sync::Arc;
use std::sync::RwLock;

use color_eyre::eyre::Result;
use matrix_sdk::Client;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::sync::SyncResponse;
use rusqlite::{OpenFlags, OptionalExtension};
use tokio_stream::StreamExt;
use tracing::instrument;

use super::connect_sqlite;

#[derive(Clone, Debug)]
pub struct SyncHelper {
    inner: Arc<RwLock<SyncHelperInner>>,
}

#[derive(Debug)]
struct SyncHelperInner {
    session_db: rusqlite::Connection,
    sync_token: Option<String>,
}

impl SyncHelper {
    #[instrument(name = "SyncHelper", skip_all)]
    pub fn new(data_dir: &Path) -> Result<Self> {
        let mut result = SyncHelperInner {
            session_db: connect_sqlite(
                &data_dir.join("matrixbot-ezlogin.sqlite3"),
                OpenFlags::SQLITE_OPEN_READ_WRITE
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_URI,
            )?,
            sync_token: None,
        };
        result
            .session_db
            .execute_batch("PRAGMA locking_mode = EXCLUSIVE;")?;
        result.sync_token = result
            .session_db
            .query_row("SELECT token FROM sync_token WHERE id = 0;", (), |row| {
                row.get(0)
            })
            .optional()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(result)),
        })
    }

    pub fn get_sync_token(&self) -> Option<String> {
        self.inner.read().unwrap().sync_token.clone()
    }

    pub fn set_sync_token(&self, token: String) -> Result<()> {
        let mut inner = self.inner.write().unwrap();
        inner.session_db.execute(
            "INSERT OR REPLACE INTO sync_token (id, token) VALUES (0, ?);",
            (&token,),
        )?;
        inner.sync_token = Some(token);
        Ok(())
    }

    pub fn process_sync_settings(&self, mut sync_settings: SyncSettings) -> SyncSettings {
        if let Some(token) = self.get_sync_token() {
            sync_settings = sync_settings.token(token);
        }
        sync_settings
    }

    pub fn process_sync_response(&self, next_batch: &str) -> Result<(), matrix_sdk::Error> {
        self.set_sync_token(next_batch.to_owned())
            .map_err(|err| matrix_sdk::Error::UnknownError(err.into()))
    }

    pub async fn sync_once(
        &self,
        client: &Client,
        sync_settings: SyncSettings,
    ) -> Result<SyncResponse, matrix_sdk::Error> {
        let sync_stream = client
            .sync_stream(self.process_sync_settings(sync_settings))
            .await;
        tokio::pin!(sync_stream);
        let response = sync_stream.next().await.unwrap()?;
        self.process_sync_response(&response.next_batch)?;
        Ok(response)
    }

    pub async fn sync(
        &self,
        client: &Client,
        sync_settings: SyncSettings,
    ) -> Result<(), matrix_sdk::Error> {
        let sync_stream = client
            .sync_stream(self.process_sync_settings(sync_settings))
            .await;
        tokio::pin!(sync_stream);
        loop {
            let response = sync_stream.next().await.unwrap()?;
            self.process_sync_response(&response.next_batch)?;
        }
    }
}

impl Drop for SyncHelperInner {
    fn drop(&mut self) {
        _ = self.session_db.execute("PRAGMA optimize;", ());
    }
}
