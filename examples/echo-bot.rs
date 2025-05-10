use std::path::{Path, PathBuf};
use std::time::Duration;

use color_eyre::eyre::Result;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::api::client::filter::FilterDefinition;
use matrix_sdk::ruma::api::client::receipt::create_receipt::v3::ReceiptType;
use matrix_sdk::ruma::events::receipt::ReceiptThread;
use matrix_sdk::ruma::events::room::member::{
    MembershipState, OriginalSyncRoomMemberEvent, StrippedRoomMemberEvent,
};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::{Client, Room, RoomState};
use matrixbot_ezlogin;
use tracing::{error, info, instrument, warn};
use tracing_subscriber::{EnvFilter, prelude::*};

#[derive(clap::Parser)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    #[clap(about = "Perform initial setup of Matrix account")]
    Setup {
        #[clap(
            long = "data",
            value_name = "PATH",
            help = "Path to store Matrix data between sessions"
        )]
        data_dir: PathBuf,
        #[clap(
            long,
            value_name = "DEVICE_NAME",
            default_value = "matrixbot-ezlogin/echo-bot",
            help = "Device name to use for this session"
        )]
        device_name: String,
    },
    #[clap(about = "Run the bot")]
    Run {
        #[clap(
            long = "data",
            value_name = "PATH",
            help = "Path to store Matrix data between sessions"
        )]
        data_dir: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    tracing_subscriber::registry()
        .with(tracing_error::ErrorLayer::default())
        .with({
            let mut filter = EnvFilter::new("warn,echo_bot=info,matrixbot_ezlogin=info");
            if let Some(env) = std::env::var_os(EnvFilter::DEFAULT_ENV) {
                for segment in env.to_string_lossy().split(',') {
                    if let Ok(directive) = segment.parse() {
                        filter = filter.add_directive(directive);
                    }
                }
            }
            filter
        })
        .with(
            tracing_subscriber::fmt::layer().with_writer(matrixbot_ezlogin::DuplexLog::get_writer),
        )
        .init();

    let args: Args = clap::Parser::parse();

    match args.command {
        Command::Setup {
            data_dir,
            device_name,
        } => {
            matrixbot_ezlogin::setup_interactive(&data_dir, &device_name).await?;
            Ok(())
        }
        Command::Run { data_dir } => run(&data_dir).await,
    }
}

async fn run(data_dir: &Path) -> Result<()> {
    let client = matrixbot_ezlogin::login(data_dir).await?;
    let sync_helper = matrixbot_ezlogin::SyncHelper::new(data_dir)?;

    client.add_event_handler(on_invite);
    client.add_event_handler(on_leave);

    // Enable room members lazy-loading, it will speed up the initial sync a lot
    // with accounts in lots of rooms.
    // See <https://spec.matrix.org/v1.6/client-server-api/#lazy-loading-room-members>.
    let sync_settings =
        SyncSettings::default().filter(FilterDefinition::with_lazy_loading().into());

    info!("Skipping events since last logout.");
    sync_helper
        .sync_once(&client, sync_settings.clone())
        .await?;

    client.add_event_handler(on_message);

    info!("Starting sync.");
    sync_helper.sync(&client, sync_settings).await?;

    Ok(())
}

// https://spec.matrix.org/v1.14/client-server-api/#mroommessage
#[instrument(skip_all)]
async fn on_message(event: OriginalSyncRoomMessageEvent, client: Client, room: Room) {
    if room.state() != RoomState::Joined {
        return;
    }
    if event.sender == client.user_id().unwrap() {
        return;
    }
    info!("Received message: {:?}", event.content);
    if !matches!(
        event.content.msgtype,
        MessageType::Audio(_)
            | MessageType::Emote(_)
            | MessageType::File(_)
            | MessageType::Image(_)
            | MessageType::Location(_)
            | MessageType::Text(_)
            | MessageType::Video(_)
    ) {
        return;
    }
    let room_clone = room.clone();
    tokio::spawn(async move {
        if let Err(err) = room_clone
            .send_single_receipt(ReceiptType::Read, ReceiptThread::Unthreaded, event.event_id)
            .await
        {
            error!("Failed to send read receipt: {:?}", err);
        }
    });
    tokio::spawn(async move {
        if let Err(err) = room.send(event.content).await {
            error!("Failed to send message: {:?}", err);
        }
    });
}

// https://spec.matrix.org/v1.14/client-server-api/#mroommember
// https://spec.matrix.org/v1.14/client-server-api/#stripped-state
#[instrument(skip_all)]
async fn on_invite(event: StrippedRoomMemberEvent, client: Client, room: Room) {
    if event.content.membership != MembershipState::Invite {
        return;
    }
    // The user for which a membership applies is represented by the state_key.
    if event.state_key != client.user_id().unwrap() {
        return;
    }
    if !event.content.is_direct.unwrap_or(false) {
        return;
    }

    tokio::spawn(async move {
        let mut retry = Duration::from_secs(2);
        while retry <= Duration::from_secs(3600) {
            info!("Trying to join room {}.", room.room_id());
            if let Err(err) = room.join().await {
                // https://github.com/matrix-org/synapse/issues/4345
                warn!("Failed to join room {}: {:?}", room.room_id(), err);
                warn!("This is normal, will retry in {}s.", retry.as_secs());
                tokio::time::sleep(retry).await;
                retry += retry;
                continue;
            }
            info!("Joined room {}.", room.room_id());
            return;
        }
        error!(
            "Failed to join room {} after 60 minutes, giving up.",
            room.room_id()
        );
    });
}

#[instrument(skip_all)]
// https://spec.matrix.org/v1.14/client-server-api/#mroommember
async fn on_leave(event: OriginalSyncRoomMemberEvent, client: Client, room: Room) {
    if !matches!(
        event.content.membership,
        MembershipState::Leave | MembershipState::Ban
    ) {
        return;
    }
    if event.state_key == client.user_id().unwrap() {
        // I am kicked or banned
        tokio::spawn(async move {
            info!("Forgetting room {}", room.room_id());
            if let Err(err) = room.forget().await {
                error!("Failed to forget room {}: {:?}", room.room_id(), err);
            }
            info!("Forgot room {}.", room.room_id());
        });
    } else if room.joined_members_count() <= 1 {
        tokio::spawn(async move {
            info!("Leaving room {}", room.room_id());
            if let Err(err) = room.leave().await {
                error!("Failed to forget room {}: {:?}", room.room_id(), err);
            }
            info!("Left room {}.", room.room_id());
        });
    }
}
