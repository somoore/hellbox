//! CLI for running DOOM in an AWS Lambda MicroVM.

mod aws;
mod browser;
mod commands;
mod config;
mod lifecycle;
mod poll;
mod state;

#[cfg(feature = "proxy")]
mod proxy;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ldoom", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bake the app into a MicroVM image.
    Build {
        #[arg(long)]
        name: String,
        #[arg(long)]
        app: Option<String>,
        /// Build context dir (default: ./capsule).
        #[arg(long = "capsule-dir")]
        capsule_dir: Option<String>,
    },
    /// Launch a MicroVM from the image.
    Up {
        #[arg(long)]
        name: String,
    },
    /// Open the running capsule in a browser tab.
    Open {
        #[arg(long)]
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
        #[arg(long)]
        name: String,
    },
    /// Thaw the capsule.
    Resume {
        #[arg(long)]
        name: String,
    },
    /// Terminate the capsule.
    Down {
        #[arg(long)]
        name: String,
    },
    /// Delete the microvm image and local state.
    Rm {
        #[arg(long)]
        name: String,
    },
    /// List known capsules.
    Ps {
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Print all current settings.
    Show,
    /// Set a setting, e.g. `ldoom config set display h264`.
    Set { key: String, value: String },
    /// Clear an optional setting back to its default/off.
    Unset { key: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // rustls needs one default provider when AWS SDK and WSS deps both bring TLS.
    #[cfg(feature = "proxy")]
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "ldoom=info,lambdadoom=info,info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
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
        Cmd::Ps { refresh } => commands::ps::run(refresh).await,
    }
}
