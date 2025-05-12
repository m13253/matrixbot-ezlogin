use std::borrow::Cow;
use std::io::{IsTerminal, Write};
use std::sync::LazyLock;

use rustyline_async::{Readline, ReadlineError, ReadlineEvent, SharedWriter};
use scopeguard::guard;
use tokio::select;
use tokio::sync::{mpsc, oneshot};

static DUPLEX_LOG: LazyLock<Option<DuplexLog>> = LazyLock::new(DuplexLog::init_global);

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

    pub fn init() {
        LazyLock::force(&DUPLEX_LOG);
    }

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
            .unwrap();
        response_rx.await.unwrap()
    }

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
                    let (prompt, response_tx) = req.unwrap();
                    _ = readline.update_prompt(&prompt);
                    pending_response_tx = Some(response_tx);
                }
                line = readline.readline() => {
                    let Some(resp_tx) = pending_response_tx.take() else {
                        if let Ok(ReadlineEvent::Interrupted) = line {
                            running = false;
                        }
                        continue;
                    };
                    let resp = match line {
                        Ok(ReadlineEvent::Line(s)) => Ok(s),
                        Ok(ReadlineEvent::Eof) => Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
                        Ok(ReadlineEvent::Interrupted) => {
                            running = false;
                            Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                        },
                        Err(ReadlineError::IO(err)) => Err(err),
                        Err(ReadlineError::Closed) => panic!(),
                    };
                    _ = readline.update_prompt("");
                    _ = resp_tx.send(resp);
                }
            }
        }
        drop(readline);
        std::process::exit(1);
    }
}
