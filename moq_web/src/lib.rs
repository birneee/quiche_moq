mod audio;
mod decode;
mod mp4;
mod session;
mod transport;
mod util;
mod video;

pub use session::{MoqSession, MoqTrack};

/// Parses a `?draft=` URL parameter ("07".."18" or "draft07".."draft18", matching
/// `moq_cli --setup-version`'s convention) into a `Version`.
fn parse_draft_version(s: &str) -> std::result::Result<moq_core::wire::Version, String> {
    let digits = s.strip_prefix("draft").unwrap_or(s);
    let n: u64 = digits.parse().map_err(|_| format!("invalid MoQ draft version: {s:?}"))?;
    moq_core::wire::version_from_draft_number(n)
        .ok_or_else(|| format!("unsupported MoQ draft version: {n} (supported: 07-18)"))
}
