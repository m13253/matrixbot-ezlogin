use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use async_stream::try_stream;
use eyre::Result;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::sync::SyncResponse;
use matrix_sdk::{Client, LoopCtrl};
use rusqlite::OptionalExtension;
use tokio_stream::{Stream, StreamExt};
use tracing::{debug, instrument, trace};

use crate::db::SQLiteHelper;

/// Helps you maintain sync positions between process restarts.
///
/// This allows you to distinguish events that occurred while the bot was offline from those that happened after it restarted.
///
/// It maintains a `sync_token` in the state database.
///
/// # Important
///
/// The state database is exclusively locked for the entire lifetime of this [`SyncHelper`], in order to prevent multiple processes from accessing the same Matrix session.
///
/// # Example
///
/// You can:
/// * Either manually pass the token between [`SyncHelper`] and [`Client::sync`], like this:
///
///    ```
///    use std::path::Path;
///
///    use color_eyre::eyre::Result;
///    use matrix_sdk::config::SyncSettings;
///    use matrix_sdk::ruma::api::client::filter::FilterDefinition;
///    use matrix_sdk::{Client, LoopCtrl};
///    use matrixbot_ezlogin::SyncHelper;
///
///    #[tokio::main]
///    async fn main() -> Result<()> {
///        let (client, sync_helper) = matrixbot_ezlogin::login(Path::new("./TODO")).await?;
///        // SyncHelper can also be used independently
///        let sync_helper = SyncHelper::new(Path::new("./TODO"))?;
///
///        // Install your bot logic handlers
///        todo!();
///
///        // Loading sync_token
///        let sync_token = sync_helper.get_sync_token();
///        let mut sync_settings = SyncSettings::default().filter(
///            FilterDefinition::with_lazy_loading().into()
///        );
///        if let Some(token) = sync_token {
///            sync_settings.token(token);
///        }
///        // Alternatively:
///        let sync_settings = sync_helper.process_sync_settings(
///            SyncSettings::default().filter(FilterDefinition::with_lazy_loading().into())
///        );
///
///        // Saving sync_token
///        client.sync_with_result_callback(sync_settings.clone(), |response| {
///            let sync_helper_clone = sync_helper.clone();
///            async move {
///                sync_helper_clone.set_sync_token(response?.next_batch)
///                    .map_err(|err| matrix_sdk::Error::UnknownError(err.into()))?;
///                Ok(LoopCtrl::Continue)
///            }
///        });
///        // Alternatively:
///        client.sync_with_result_callback(sync_settings, |response| {
///            let sync_helper_clone = sync_helper.clone();
///            async move {
///                sync_helper_clone.process_sync_response(&response?)
///            }
///        });
///
///        Ok(())
///    }
///    ```
///
/// * Or, you can call the convenience methods [`SyncHelper::sync`], [`SyncHelper::sync_once`], or [`SyncHelper::sync_stream`], that automatically loads and saves `sync_token` for you.
///
/// * Or, you can also mix and match the easy and hard ways in an application.
#[derive(Clone, Debug)]
pub struct SyncHelper {
    inner: Arc<Mutex<SyncHelperInner>>,
}

#[derive(Debug)]
struct SyncHelperInner {
    session_db: SQLiteHelper,
    sync_token: Option<String>,
}

impl SyncHelper {
    /// Creates a new [`SyncHelper`] to use it independently from [`login`](crate::login).
    ///
    /// # Arguments
    ///
    /// * `data_dir`: The directory containing the bot's state database.
    ///
    ///   It must be the same as specified in [`login`](crate::login).
    #[instrument(name = "SyncHelper", skip_all)]
    pub fn new(data_dir: &Path) -> Result<Self> {
        Self::from_opened_db(SQLiteHelper::open(
            &data_dir.join("matrixbot-ezlogin.sqlite3"),
            false,
        )?)
    }

    pub(crate) fn from_opened_db(session_db: SQLiteHelper) -> Result<Self> {
        let sync_token = session_db
            .query_row("SELECT token FROM sync_token WHERE id = 0;", (), |row| {
                row.get(0)
            })
            .optional()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(SyncHelperInner {
                session_db,
                sync_token,
            })),
        })
    }

    /// Retrieves the saved `sync_token`.
    pub fn get_sync_token(&self) -> Option<String> {
        let token = self
            .inner
            .lock()
            // lock() will only return an error after some other task panicked
            .unwrap()
            .sync_token
            .clone();
        debug!("Current sync token: {}", token.as_deref().unwrap_or("None"));
        token
    }

    /// Stores a new `sync_token` that the Matrix server provides as [`SyncResponse::next_batch`].
    pub fn set_sync_token(&self, token: String) -> Result<()> {
        debug!("Next sync token: {}", token);
        let mut inner = self
            .inner
            .lock()
            // lock() will only return an error after some other task panicked
            .unwrap();
        inner
            .session_db
            .prepare_cached("INSERT OR REPLACE INTO sync_token (id, token) VALUES (0, ?);")?
            .execute((&token,))?;
        inner.sync_token = Some(token);
        Ok(())
    }

    /// Convenience method that calls [`SyncHelper::get_sync_token`] to populate a [`SyncSettings`].
    pub fn process_sync_settings(&self, mut sync_settings: SyncSettings) -> SyncSettings {
        if let Some(token) = self.get_sync_token() {
            sync_settings = sync_settings.token(token);
        }
        sync_settings
    }

    /// Convenience method that calls [`SyncHelper::set_sync_token`] using a [`SyncResponse`].
    ///
    /// On success, it returns [`Ok(LoopCtrl::Continue)`](LoopCtrl::Continue) for your convenience.
    pub fn process_sync_response(
        &self,
        sync_response: &SyncResponse,
    ) -> Result<LoopCtrl, matrix_sdk::Error> {
        self.set_sync_token(sync_response.next_batch.clone())
            .map_err(|err| matrix_sdk::Error::UnknownError(err.into()))?;
        Ok(LoopCtrl::Continue)
    }

    /// Convenience method that calls [`SyncHelper::process_sync_settings`], [`matrix_sdk::Client::sync_once`], then [`SyncHelper::process_sync_response`].
    ///
    /// The first [`sync_once`](SyncHelper::sync_once) call immediately after [`login`](crate::login) returns events that occurred while the bot was offline (i.e., old events).
    ///
    /// Therefore, if your bot logic wants to ignore such old events, install event handlers *after* [`sync_once`](SyncHelper::sync_once).
    ///
    /// Internally, it actually calls [`matrix_sdk::Client::sync_stream`] to let it manage retry logic.
    pub async fn sync_once(
        &self,
        client: &Client,
        sync_settings: SyncSettings,
    ) -> Result<SyncResponse, matrix_sdk::Error> {
        let sync_stream = client
            .sync_stream(self.process_sync_settings(sync_settings))
            .await;
        tokio::pin!(sync_stream);
        let response = sync_stream
            .next()
            .await
            // sync_stream is infinite
            .unwrap()?;
        trace!("Sync response: {:?}", response);
        self.process_sync_response(&response)?;
        Ok(response)
    }

    /// Convenience method that returns a [`Stream`], which calls [`SyncHelper::process_sync_settings`], [`matrix_sdk::Client::sync_once`], then [`SyncHelper::process_sync_response`] whenever being polled.
    ///
    /// Internally, it actually calls [`matrix_sdk::Client::sync_stream`] to let it manage retry logic.
    pub async fn sync_stream(
        &self,
        client: &Client,
        sync_settings: SyncSettings,
    ) -> impl Stream<Item = Result<SyncResponse, matrix_sdk::Error>> {
        let sync_stream = client
            .sync_stream(self.process_sync_settings(sync_settings))
            .await;
        try_stream! {
            tokio::pin!(sync_stream);
            loop {
                let response = sync_stream
                    .next()
                    .await
                    // sync_stream is infinite
                    .unwrap()?;
                trace!("Sync response: {:?}", response);
                self.process_sync_response(&response)?;
                yield response;
            }
        }
    }

    /// Convenience method that calls [`SyncHelper::process_sync_settings`], [`matrix_sdk::Client::sync_once`], then [`SyncHelper::process_sync_response`] in an infinite loop.
    ///
    /// Internally, it actually calls [`matrix_sdk::Client::sync_stream`] to let it manage retry logic.
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
            let response = sync_stream
                .next()
                .await
                // sync_stream is infinite
                .unwrap()?;
            trace!("Sync response: {:?}", response);
            self.process_sync_response(&response)?;
        }
    }
}
