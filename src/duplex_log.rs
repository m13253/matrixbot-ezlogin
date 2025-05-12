use std::borrow::Cow;
use std::io::{IsTerminal, Write};
use std::sync::LazyLock;

use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use scopeguard::guard;
use tokio::select;
use tokio::sync::{mpsc, oneshot};

static DUPLEX_LOG: LazyLock<Option<DuplexLog>> = LazyLock::new(DuplexLog::init_global);

/// Provides a way to handle terminal input while also allowing other parts of the application to log messages.
///
/// Internally, it starts a background task that uses [`rustyline_async`] to handle all the input/output.
///
/// # Example
///
/// ```
/// use matrixbot_ezlogin::DuplexLog;
/// use tracing_subscriber::{EnvFilter, prelude::*};
///
/// #[tokio::main]
/// async fn main() {
///     DuplexLog::init();
///     tracing_subscriber::registry()
///         .with(tracing_error::ErrorLayer::default())
///         .with({
///             let mut filter = EnvFilter::new("warn,matrixbot_ezlogin=debug");
///             if let Some(env) = std::env::var_os(EnvFilter::DEFAULT_ENV) {
///                 for segment in env.to_string_lossy().split(',') {
///                     if let Ok(directive) = segment.parse() {
///                         filter = filter.add_directive(directive);
///                     }
///                 }
///             }
///             filter
///         })
///         .with(tracing_subscriber::fmt::layer().with_writer(DuplexLog::get_writer))
///         .init();
///
///     todo!()
/// }
/// ```
pub struct DuplexLog {
    request_tx: mpsc::Sender<(
        Cow<'static, str>,
        oneshot::Sender<Result<String, std::io::Error>>,
    )>,
    shared_writer: SharedWriter,
}

impl DuplexLog {
    fn init_global() -> Option<DuplexLog> {
        if !std::io::stdin().is_terminal() {
            return None;
        }
        let Ok((readline, shared_writer)) = Readline::new(String::new()) else {
            return None;
        };
        let (request_tx, request_rx) = mpsc::channel(1);
        tokio::spawn(Self::run_background_task(request_rx, readline));
        Some(DuplexLog {
            request_tx,
            shared_writer,
        })
    }

    /// Initializes the global instance.
    ///
    /// This function should be called early in the application's lifecycle before `tracing_subscriber` is set up.
    /// This is to prevent deadlock, because third-party libraries that [`DuplexLog::init`] depends on can generate traces.
    pub fn init() {
        LazyLock::force(&DUPLEX_LOG);
    }

    /// Asynchronously reads a line of input from the terminal with the given prompt.
    ///
    /// It returns [`UnexpectedEof`](std::io::ErrorKind::UnexpectedEof) if [`stdin`](std::io::stdin) is not a TTY.
    pub async fn readline<S>(prompt: S) -> Result<String, std::io::Error>
    where
        S: Into<Cow<'static, str>>,
    {
        let Some(inst) = DUPLEX_LOG.as_ref() else {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        };
        let (response_tx, response_rx) = oneshot::channel();
        inst.request_tx
            .send((prompt.into(), response_tx))
            .await
            // run_background_task should run forever
            .unwrap();
        response_rx
            .await
            // run_background_task always sends a response
            .unwrap()
    }

    /// Gets a writer that can be used to print messages to the terminal without interfering with the [`DuplexLog::readline`] prompt.
    pub fn get_writer() -> Box<dyn Write> {
        let Some(inst) = DUPLEX_LOG.as_ref() else {
            return Box::new(std::io::stdout());
        };
        Box::new(inst.shared_writer.clone())
    }

    async fn run_background_task(
        mut request_rx: mpsc::Receiver<(
            Cow<'static, str>,
            oneshot::Sender<Result<String, std::io::Error>>,
        )>,
        readline: Readline,
    ) {
        let mut readline = guard(readline, |mut readline| {
            _ = readline.flush();
        });
        let mut pending_response_tx = None;
        let mut running = true;
        while running {
            select! {
                req = request_rx.recv() => {
                    let Some((prompt, response_tx)) = req else {
                        continue;
                    };
                    _ = readline.update_prompt(&prompt);
                    pending_response_tx = Some(response_tx);
                }
                line = readline.readline() => {
                    let resp = match line {
                        Ok(ReadlineEvent::Line(s)) => Ok(s),
                        Ok(ReadlineEvent::Eof) => Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
                        Ok(ReadlineEvent::Interrupted) => {
                            running = false;
                            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                        },
                        Err(ReadlineError::IO(err)) => Err(err),
                        Err(ReadlineError::Closed) => {
                            // DUPLEX_LOG.shared_writer has static lifetime
                            unreachable!()
                        }
                    };
                    let Some(response_tx) = pending_response_tx.take() else {
                        continue;
                    };
                    _ = readline.update_prompt("");
                    _ = response_tx.send(resp);
                }
            }
        }
        drop(readline);
        std::process::exit(1);
    }
}
