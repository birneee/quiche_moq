use clap::{Parser, Subcommand};
use std::path::PathBuf;

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

#[derive(Parser)]
pub(crate) struct PublishArgs {
    /// The url to connect to
    #[arg()]
    pub(crate) url: String,
    #[arg(env = "SSLKEYLOGFILE")]
    pub(crate) ssl_key_log_file: Option<PathBuf>,
}

#[derive(Parser, Clone)]
pub(crate) struct SubscribeArgs {
    /// The url to connect to
    #[arg()]
    pub(crate) url: String,
    #[arg(env = "SSLKEYLOGFILE")]
    pub(crate) ssl_key_log_file: Option<PathBuf>,
    #[arg(long)]
    pub(crate) namespace: String,
    #[arg(long)]
    pub(crate) trackname: String,
    /// The output file path.
    /// "-" can be used for stdout.
    #[arg(long, short = 'o')]
    pub(crate) output: Option<PathBuf>,
}
