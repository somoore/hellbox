//! CLI for running DOOM in an AWS Lambda MicroVM.

mod aws;
#[cfg(feature = "proxy")]
mod browser;
mod commands;
mod config;
mod embedded;
mod lifecycle;
mod poll;
mod state;

#[cfg(feature = "proxy")]
mod proxy;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "hellbox",
    version,
    about,
    after_help = "Running `hellbox` with no command is the same as `hellbox play`."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Get DOOM on screen, whatever it takes (default when no command given).
    Play {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// One-command install: AWS prerequisites stack, image build, launch, open.
    Deploy {
        #[command(subcommand)]
        action: Option<DeployAction>,
        #[arg(long, default_value = "doom")]
        name: String,
        /// Region to deploy into (default: AWS_REGION / AWS_DEFAULT_REGION / us-east-1).
        #[arg(long, short = 'r')]
        region: Option<String>,
        /// CloudFormation parameter override, KEY=VALUE (repeatable).
        #[arg(long = "parameter", short = 'p', value_name = "KEY=VALUE")]
        parameters: Vec<String>,
    },
    /// Full teardown: microvm, image, artifact bucket, stack, local state.
    Destroy {
        #[arg(long, default_value = "doom")]
        name: String,
        /// Actually delete (destroy refuses to run without this).
        #[arg(long)]
        yes: bool,
    },
    /// Bake the app into a MicroVM image.
    Build {
        #[arg(long, default_value = "doom")]
        name: String,
        #[arg(long)]
        app: Option<String>,
        /// Build context dir (default: ./capsule).
        #[arg(long = "capsule-dir")]
        capsule_dir: Option<String>,
    },
    /// Launch a MicroVM from the image.
    Up {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// Open the running capsule in a browser tab.
    Open {
        #[arg(long, default_value = "doom")]
        name: String,
        /// Start the proxy and print its URL but don't launch the browser.
        #[arg(long = "no-open")]
        no_open: bool,
    },
    /// View or change persistent settings.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Freeze the capsule.
    Suspend {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// Thaw the capsule.
    Resume {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// Terminate the capsule.
    Down {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// Delete the microvm image and local state.
    Rm {
        #[arg(long, default_value = "doom")]
        name: String,
    },
    /// List known capsules. Reconciles state against AWS by default; the
    /// platform can suspend a MicroVM on its own, so the local cache goes stale
    /// while hellbox isn't running. Use --no-refresh for a fast offline read.
    Ps {
        #[arg(long)]
        no_refresh: bool,
    },
}

#[derive(Subcommand)]
enum DeployAction {
    /// Open the stack template in $EDITOR; later deploys use the edited copy.
    Edit,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print all current settings.
    Show,
    /// Set a setting, e.g. `hellbox config set display h264`.
    Set { key: String, value: String },
    /// Clear an optional setting back to its default/off.
    Unset { key: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls needs one default provider when AWS SDK and WSS deps both bring TLS.
    #[cfg(feature = "proxy")]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    // Default: our own INFO lines only, everything else at warn so the console
    // reads clean. aws-config is pinned to error: its credential-chain chatter
    // (which also prints the access key id) is noise, and it WARNs on the
    // login_session profile-provider failure that `aws::resolve` expects and
    // recovers from via credential_process — surfacing that would be alarming
    // for a case we handle. `RUST_LOG=hellbox=debug` or `RUST_LOG=info` opts
    // back into the detail.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hellbox=info,aws_config=error,warn".into()),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    match cli.cmd.unwrap_or(Cmd::Play {
        name: "doom".to_string(),
    }) {
        Cmd::Play { name } => commands::play::run(&name).await,
        Cmd::Deploy {
            action: Some(DeployAction::Edit),
            ..
        } => commands::deploy::edit(),
        Cmd::Deploy {
            action: None,
            name,
            region,
            parameters,
        } => commands::deploy::run(&name, region.as_deref(), &parameters).await,
        Cmd::Destroy { name, yes } => commands::destroy::run(&name, yes).await,
        Cmd::Build {
            name,
            app,
            capsule_dir,
        } => commands::build::run(&name, app.as_deref(), capsule_dir.as_deref()).await,
        Cmd::Up { name } => commands::up::run(&name).await,
        Cmd::Open { name, no_open } => commands::open::run(&name, no_open).await,
        Cmd::Config { action } => match action {
            ConfigAction::Show => commands::config_cmd::show(),
            ConfigAction::Set { key, value } => commands::config_cmd::set(&key, &value),
            ConfigAction::Unset { key } => commands::config_cmd::unset(&key),
        },
        Cmd::Suspend { name } => commands::suspend::run(&name).await,
        Cmd::Resume { name } => commands::resume::run(&name).await,
        Cmd::Down { name } => commands::down::run(&name).await,
        Cmd::Rm { name } => commands::rm::run(&name).await,
        Cmd::Ps { no_refresh } => commands::ps::run(!no_refresh).await,
    }
}
