use crate::prelude::*;
use moq_wire::{MOQ_VERSION_DRAFT_15, SUPPORTED_MOQ_VERSIONS, Version};

/// How a session determines its MoQT version. draft 15 moved version
/// negotiation out of SETUP and into ALPN / WT-Available-Protocols
/// (draft-16 §3.1), so SETUP for draft >= 15 no longer carries a candidate
/// list — the version must already be resolved below this layer.
#[derive(Clone, Debug)]
pub enum VersionNegotiation {
    /// draft < 15: the peer's acceptable versions are exchanged in-band, in
    /// CLIENT_SETUP's version list; negotiation picks the newest version
    /// present in both `candidates` and the peer's own list. Every entry must
    /// be < draft 15.
    InBand { candidates: Vec<Version> },
    /// draft >= 15: `version` was already determined below this layer
    /// (native QUIC ALPN, or the WT-Available-Protocols header for
    /// WebTransport) before the session was created — there is nothing left
    /// to negotiate in SETUP.
    Pinned(Version),
}

impl VersionNegotiation {
    /// Just `version`: pinned from draft 15, which negotiates by ALPN, offered
    /// in-band before.
    pub fn only(version: Version) -> Self {
        if version.negotiates_in_band() {
            VersionNegotiation::InBand { candidates: vec![version] }
        } else {
            VersionNegotiation::Pinned(version)
        }
    }

    /// The version to encode this session's own first SETUP message with,
    /// and the fallback guess for decoding an incoming one before
    /// `selected_version` is known (see `bootstrap_version_guess`).
    pub fn preferred(&self) -> Version {
        match self {
            VersionNegotiation::Pinned(v) => *v,
            VersionNegotiation::InBand { candidates } => *candidates.iter().max().expect("InBand candidates must be non-empty"),
        }
    }

    /// Rejects a shape that can't actually be conveyed on the wire: draft >=
    /// 15 has no version list to be ambiguous about `Pinned` on, and draft <
    /// 15 has no ALPN/WT-Available-Protocols pinning mechanism at all.
    pub(crate) fn validate(&self) -> crate::error::Result<()> {
        match self {
            VersionNegotiation::InBand { candidates } => {
                if candidates.is_empty() {
                    return Err(crate::error::Error::Wire(moq_wire::Error::ProtocolViolation(
                        "InBand version negotiation requires at least one candidate".into(),
                    )));
                }
                if let Some(v) = candidates.iter().find(|v| **v >= MOQ_VERSION_DRAFT_15) {
                    return Err(crate::error::Error::Wire(moq_wire::Error::ProtocolViolation(
                        format!("draft {v} negotiates via ALPN, not CLIENT_SETUP's version list — use VersionNegotiation::Pinned instead"),
                    )));
                }
            }
            VersionNegotiation::Pinned(v) => {
                if *v < MOQ_VERSION_DRAFT_15 {
                    return Err(crate::error::Error::Wire(moq_wire::Error::ProtocolViolation(
                        format!("draft {v} has no ALPN/WT-Available-Protocols pinning mechanism — use VersionNegotiation::InBand instead"),
                    )));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Config {
    pub version_negotiation: VersionNegotiation,
    pub ignore_max_request_quota: bool,
    /// MAX_REQUEST_ID sent in our SETUP.
    /// - raised by this much each time the peer used up half of it, drafts 11 to 16
    pub out_max_request_id: u64,
    /// Can be disabled afterwards.
    pub enable_padding_stream: bool,
    /// Padding stream rate cap in bytes/sec; `None` = uncapped.
    /// - stream priority can't reorder bytes already in the link queue
    /// - uncapped padding delays control frames on narrow paths
    pub padding_stream_max_bytes_per_sec: Option<u64>,
    /// MAX_FILTER_RANGES sent in our draft 19+ SETUP: the Ranges the peer's Range Filters may have per subscription.
    pub max_filter_ranges: u64,
    /// AUTHORITY and PATH of the URI a client connects to on native QUIC (draft-ietf-moq-transport-18 §3.1.4).
    pub native_quic_uri: Option<(Vec<u8>, Vec<u8>)>,
}

impl Default for Config {
    /// `InBand` over every draft below the ALPN cutoff. draft >= 15 has no
    /// unpinned/multi-candidate mode to default to — it requires an explicit
    /// `VersionNegotiation::Pinned`, since this crate doesn't (yet) inspect
    /// ALPN/WT-Available-Protocols itself.
    fn default() -> Self {
        Self {
            version_negotiation: VersionNegotiation::InBand {
                candidates: SUPPORTED_MOQ_VERSIONS.iter().copied().filter(|v| *v < MOQ_VERSION_DRAFT_15).collect(),
            },
            ignore_max_request_quota: false,
            out_max_request_id: 100,
            enable_padding_stream: false,
            padding_stream_max_bytes_per_sec: None,
            max_filter_ranges: 16,
            native_quic_uri: None,
        }
    }
}
