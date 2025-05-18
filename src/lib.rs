//! matrixbot-ezlogin: "I wrote the login and E2EE bootstrap code for Matrix bots so you don’t have to."
//!
//! Writing a Matrix bot is easy, but supporting end-to-end encryption is extremely difficult.
//!
//! Not only because the bot must maintain a database to store encryption keys between sessions, but also because the bootstrap process requires a human to interactively type in or copy out the recovery key.
//!
//! Sadly, the [official Matrix SDK](matrix_sdk) doesn't provide a complete solution to bootstrap a Matrix bot, resulting in bot developers needing to waste time writing the authentication code again and again.
//!
//! Here, I publish this library called matrixbot-ezlogin, as a good starting point for every Matrix bot. So, you can skip the trouble and directly hop into the bot logic.
//!
//! # Two-stages of a bot
//!
//! In order to set up a Matrix bot with E2EE support, there are two steps:
//!
//! 1. Creating a Matrix session, setting up cross-signing keys, creating or recovering from a server-side backup, then exit. This step requires human intervention.
//!
//! 2. Restoring the Matrix session to run actual bot logic. This step is unattended and can be set to auto-start when computer boots up.
//!
//! # Components of matrixbot-ezlogin
//!
//! This library provides the functions [`setup`] (or [`setup_interactive`]) and [`login`] to simplify these two steps.
//!
//! Additionally, [`DuplexLog`] helps handling duplex terminal input / output. [`SyncHelper`] helps remembering sync tokens between process restarts.
//!
//! The `examples` folder contains a simple echo-bot for you to experience the feature of matrixbot-ezlogin, and serves as a good starting point to develop a new Matrix bot.

mod auth;
mod db;
mod duplex_log;
mod interactive;
mod sync;

pub use auth::{SetupConfig, login, logout, setup};
pub use duplex_log::DuplexLog;
pub use interactive::setup_interactive;
pub use sync::SyncHelper;
