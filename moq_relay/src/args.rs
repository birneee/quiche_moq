use clap::Parser;

#[derive(Parser)]
pub(crate) struct Args {
    #[arg(short, long)]
    pub(crate) relay: Option<String>,
    #[arg(short, long, default_value_t = 8080)]
    pub(crate) port: u16,
    /// Maximum QUIC idle timeout in milliseconds
    #[arg(long, default_value_t = 30000)]
    pub(crate) timeout: u64,
}
