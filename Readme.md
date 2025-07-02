matrixbot-ezlogin
=================

[![Download the crate from crates.io](https://img.shields.io/crates/v/matrixbot-ezlogin)](https://crates.io/crates/matrixbot-ezlogin)
[![Read the documentation on docs.rs](https://img.shields.io/docsrs/matrixbot-ezlogin)](https://docs.rs/matrixbot-ezlogin)

Writing a Matrix bot is easy, but supporting end-to-end encryption is extremely difficult.

Not only because the bot must maintain a database to store encryption keys between sessions, but also because the bootstrap process requires a human to interactively type in or copy out the recovery key.

Sadly, [the official Matrix SDK](https://github.com/matrix-org/matrix-rust-sdk) doesn’t provide a complete solution to bootstrap a Matrix bot, resulting in bot developers needing to waste time writing the authentication code again and again.

Here, I publish this library called matrixbot-ezlogin, as a good starting point for every Matrix bot. So, you can skip the trouble and directly hop into the bot logic.

## Running the example

To experience matrixbot-ezlogin, you can try the provided echo-bot example.

1. Create a Matrix account.

   I suggest registering your bot on [a self-hosted Synapse server](https://element-hq.github.io/synapse/latest/setup/installation.html), because you can easily hit the login rate limit if you want to explore all different features.

   ```
   $ register_new_matrix_user --no-admin -c homeserver.yaml -t bot
   ```

   To loosen the rate limit of Synapse, use the [`rc_login`](https://element-hq.github.io/synapse/latest/usage/configuration/config_documentation.html#rc_login) option.

   The bot account has to use password authentication. Multi-factor authentication and single sign-on are unsupported, as they can’t function unattended.

2. Perform the setup procedure.

   ```
   $ cargo run --example=echo-bot setup --data=/path/to/database
   Matrix homeserver: <HOMESERVER>
   User name: <USERNAME>
   Password: <PASSWORD>
   ```

   Depending on whether a backup exists on the server, you may be asked:
   ```
   Backup recovery key: <RECOVERY KEY>
   ```
   Or:
   ```
   Are you ready to reset the cryptographic identity to enable server-side backup (y/n)? y
   Please move <DATA PATH>/recovery-key.txt to a safe place, then press ENTER to continue:
   ```

3. Run the bot.

   ```
   $ cargo run --example=echo-bot run --data=/path/to/database
   ```

   The database path has to match the previous step. If you want to run multiple bots, each one has to use a different database path.

   Since matrixbot-ezlogin remembers your authentication, this step requires no human interaction, and can be set to start automatically on computer bootup.

4. Remove any unverified registration session.

   The `register_new_matrix_user` program mentioned above may have created an unverified session. This session should be removed to prevent encryption problems.

   Use a Matrix client, for example, [Element](https://matrix.org/ecosystem/clients/element/), to log into your bot account. Type in your recovery key when asked.

   Go to settings. In the Sessions tab, sign out of any unverified or unrecognized sessions.

   Finally, sign out of Element.

5. Chat with echo-bot.

   Echo-bot responds to every direct message, but not group chats.

   Send something to echo-bot in a DM, and see if it echoes back.

6. (If anything goes wrong,) reset the cryptographic identity.

   First, delete echo-bot’s database directory.

   Log into your bot account on Element. Choose “Reset cryptographic identity” if your recovery key no longer works.

   Go to settings. In the Sessions tab, sign out of all other sessions.

   In the Encryption tab, click “Reset cryptographic identity” again to ensure really no data carry over.

   After resetting, close and reopen Element.

   In the Encryption tab, turn off “Allow key storage” if it was automatically turned on.

   Finally, sign out of Element. Do not enable backup when asked.

   This should clear the E2EE-related data.

## Dependency versions

As the whole application has to link to a single SQLite version, [`Cargo.toml`](Cargo.toml) specifies extremely loose version requirements for the `matrix-sdk` and `rusqlite` crates.

This can avoid version conflicts, but may result in worse forward-compatibility.

Please make sure any upper-layer applications that use matrixbot-ezlogin specify more strict version requirements for the `matrix-sdk` and `rusqlite` crates in their `Cargo.toml` files.

## License

This library is released under [the MIT license](LICENSE).
