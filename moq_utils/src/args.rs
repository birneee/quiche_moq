use clap::{Parser, Subcommand};
use std::path::PathBuf;
use quiche_moq::wire::{Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_13};

#[derive(Parser)]
pub(crate) struct Args {
    #[clap(subcommand)]
    pub(crate) command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(name = "pub", about = "Publish a track")]
    Publish(PublishArgs),
    #[command(name = "sub", about = "Subscribe to a track")]
    Subscribe(SubscribeArgs),
}

#[derive(Parser, Clone)]
pub(crate) struct PublishArgs {
    /// The url to connect to
    #[arg()]
    pub(crate) url: String,
    #[arg(env = "SSLKEYLOGFILE", long)]
    pub(crate) ssl_key_log_file: Option<PathBuf>,
    #[arg(short = 't')]
    /// Namespace and track name
    /// e.g. example.2enet-team2-project_x--report
    pub(crate) namespace_trackname: String,
}

#[derive(Parser, Clone)]
pub(crate) struct SubscribeArgs {
    /// The url to connect to
    #[arg()]
    pub(crate) url: String,
    #[arg(env = "SSLKEYLOGFILE")]
    pub(crate) ssl_key_log_file: Option<PathBuf>,
    #[arg(long)]
    /// Namespace and track name
    /// e.g. example.2enet-team2-project_x--report
    pub(crate) namespace_trackname: String,
    /// The output file path.
    /// "-" can be used for stdout.
    #[arg(long, short = 'o')]
    pub(crate) output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t=SetupVersion::Draft13)]
    pub(crate) setup_version: SetupVersion,
    /// Add separator between objects in output
    /// e.g. "\n"
    #[arg(long, default_value="")]
    pub(crate) separator: String,
}

#[derive(Parser, Copy, Clone, clap::ValueEnum)]
pub(crate) enum SetupVersion {
    Draft07,
    Draft13,
}

impl From<SetupVersion> for Version {
    fn from(val: SetupVersion) -> Self {
        match val {
            SetupVersion::Draft07 => MOQ_VERSION_DRAFT_07,
            SetupVersion::Draft13 => MOQ_VERSION_DRAFT_13,
        }
    }
}
