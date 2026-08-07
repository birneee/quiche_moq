use crate::{
    MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11,
    MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16,
    MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18,
    MOQ_VERSION_LITE_01_BY_KIXELATED, MOQ_VERSION_LITE_02_BY_KIXELATED, MOQ_VERSION_LITE_03_BY_KIXELATED,
    MOQ_VERSION_LITE_04_BY_KIXELATED, MOQ_VERSION_LITE_05_BY_KIXELATED,
};

pub type Version = u64;

/// The ALPN / WT-Available-Protocols protocol string for `v` (e.g. "moqt-18"
/// for draft 18), matching the naming Cloudflare's moq-rs uses for its
/// native-QUIC ALPN. `None` below draft 15, which negotiates in-band via
/// CLIENT_SETUP's version list instead (see `moq_core::VersionNegotiation`)
/// and has no such string.
pub fn alpn_protocol_string(v: Version) -> Option<String> {
    match v {
        MOQ_VERSION_DRAFT_15..=MOQ_VERSION_DRAFT_18 => Some(format!("moqt-{}", version_to_name(v))),
        _ => None,
    }
}

/// The inverse of `alpn_protocol_string`: the draft `s` (e.g. "moqt-18")
/// names, or `None` if it isn't a protocol string this crate recognizes.
pub fn version_from_alpn_protocol_string(s: &str) -> Option<Version> {
    let n: u64 = s.strip_prefix("moqt-")?.parse().ok()?;
    let v = 0xff000000 + n;
    (MOQ_VERSION_DRAFT_15..=MOQ_VERSION_DRAFT_18).contains(&v).then_some(v)
}

/// draft 17+: each request gets its own stream, so REQUEST_OK/SUBSCRIBE_OK/
/// REQUEST_ERROR/PUBLISH_OK drop Request ID from the wire and are correlated
/// by stream instead.
pub fn stream_per_request(v: Version) -> bool {
    matches!(v, MOQ_VERSION_DRAFT_17 | MOQ_VERSION_DRAFT_18)
}

/// draft 12+: the publisher assigns the track alias (in SUBSCRIBE_OK).
/// Before that, the subscriber assigns it (in SUBSCRIBE).
pub fn publisher_assigns_track_alias(v: Version) -> bool {
    v >= MOQ_VERSION_DRAFT_12 && v <= MOQ_VERSION_DRAFT_18
}

/// The `Version` for draft number `n` (7..=18), or `None` if out of range.
pub fn version_from_draft_number(n: u64) -> Option<Version> {
    (7..=18).contains(&n).then_some(0xff000000 + n)
}

/// draft 11-18: a control message's length field on the wire is a fixed
/// `u16`. Before that, it's a varint. Bounded above (not just `>=`) so an
/// unrelated, numerically larger `Version` (e.g. `MOQ_VERSION_LITE_*`) isn't
/// silently misclassified as a newer draft.
pub fn message_length_is_fixed_u16(v: Version) -> bool {
    (MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_18).contains(&v)
}

/// draft 15-18: a KVP's type field is delta-encoded from the previous key in
/// the sequence (and the sequence is sorted by type). Before that, it's the
/// absolute type ID in original order. Bounded above for the same reason as
/// `message_length_is_fixed_u16`.
pub fn kvp_type_delta_encoded(v: Version) -> bool {
    (MOQ_VERSION_DRAFT_15..=MOQ_VERSION_DRAFT_18).contains(&v)
}

/// draft < 15: the peer's acceptable versions are exchanged in-band, in
/// CLIENT_SETUP/SERVER_SETUP's version list. draft 15+ negotiates via ALPN /
/// WT-Available-Protocols instead (see `moq_core::VersionNegotiation`) and
/// carries no version list on the wire.
pub fn negotiates_version_in_band(v: Version) -> bool {
    v < MOQ_VERSION_DRAFT_15
}

/// draft <= 10: parameters use the legacy (type, length, bytes) encoding via
/// `Parameter::from_bytes`/`to_bytes` directly. draft 11+ delegates to
/// `KeyValuePairs`.
pub fn legacy_parameter_encoding(v: Version) -> bool {
    v <= MOQ_VERSION_DRAFT_10
}

pub fn version_to_name(v: Version) -> &'static str {
    match v {
        MOQ_VERSION_DRAFT_07 => "07",
        MOQ_VERSION_DRAFT_08 => "08",
        MOQ_VERSION_DRAFT_09 => "09",
        MOQ_VERSION_DRAFT_10 => "10",
        MOQ_VERSION_DRAFT_11 => "11",
        MOQ_VERSION_DRAFT_12 => "12",
        MOQ_VERSION_DRAFT_13 => "13",
        MOQ_VERSION_DRAFT_14 => "14",
        MOQ_VERSION_DRAFT_15 => "15",
        MOQ_VERSION_DRAFT_16 => "16",
        MOQ_VERSION_DRAFT_17 => "17",
        MOQ_VERSION_DRAFT_18 => "18",
        MOQ_VERSION_LITE_01_BY_KIXELATED => "lite01",
        MOQ_VERSION_LITE_02_BY_KIXELATED => "lite02",
        MOQ_VERSION_LITE_03_BY_KIXELATED => "lite03",
        MOQ_VERSION_LITE_04_BY_KIXELATED => "lite04",
        MOQ_VERSION_LITE_05_BY_KIXELATED => "lite05",
        _ => "unknown",
    }
}
