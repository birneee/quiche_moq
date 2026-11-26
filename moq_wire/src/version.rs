use core::fmt;

use crate::prelude::*;
use moq_wire_primitives::{Frame, Reader, VarIntForm, Writer};

/// A MoQT draft this crate implements, ordered by draft number. Version
/// numbers from the wire that aren't one stay `u64`, e.g. a peer's CLIENT_SETUP list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(u8);

impl Version {
    pub const fn from_draft(draft: u8) -> Option<Self> {
        if 7 <= draft && draft <= 19 { Some(Self(draft)) } else { None }
    }

    pub const fn from_wire(version: u64) -> Option<Self> {
        if version >> 8 == 0xff_0000 { Self::from_draft(version as u8) } else { None }
    }

    pub const fn to_wire(self) -> u64 {
        0xff00_0000 + self.0 as u64
    }

    pub const fn draft(self) -> u8 {
        self.0
    }

    /// QUIC varints until draft 16, leading-ones varints from draft 17, with 7 bytes from draft 18.
    pub const fn varint_form(self) -> VarIntForm {
        match self.0 {
            ..=16 => VarIntForm::Quic,
            17 => VarIntForm::LeadingOnes { seven_bytes: false },
            _ => VarIntForm::LeadingOnes { seven_bytes: true },
        }
    }

    /// Decodes `buf`, which may continue.
    pub fn reader(self, buf: &[u8]) -> Reader<'_> {
        Reader::new(buf, self.varint_form())
    }

    /// Decodes `buf` as one complete frame.
    pub fn frame_reader(self, buf: &[u8]) -> Reader<'_, Frame> {
        Reader::frame_of(buf, self.varint_form())
    }

    /// Appends to `buf`.
    pub fn writer(self, buf: &mut Vec<u8>) -> Writer<'_> {
        Writer::new(buf, self.varint_form())
    }

    /// The ALPN / WT-Available-Protocols protocol string (e.g. "moqt-18"),
    /// matching Cloudflare's moq-rs. `None` before draft 15, which negotiates
    /// in-band instead (see `moq_core::VersionNegotiation`).
    pub fn alpn_protocol(self) -> Option<String> {
        (!self.negotiates_in_band()).then(|| format!("moqt-{self}"))
    }

    pub fn from_alpn_protocol(protocol: &str) -> Option<Self> {
        let version = Self::from_draft(protocol.strip_prefix("moqt-")?.parse().ok()?)?;
        (!version.negotiates_in_band()).then_some(version)
    }

    /// Drafts 7–14 exchange versions in CLIENT_SETUP/SERVER_SETUP, later ones
    /// via ALPN / WT-Available-Protocols (see `moq_core::VersionNegotiation`).
    pub const fn negotiates_in_band(self) -> bool {
        self.0 <= 14
    }

    /// Draft 17+: each request gets its own stream, so REQUEST_OK/SUBSCRIBE_OK/
    /// REQUEST_ERROR/PUBLISH_OK drop Request ID from the wire and are
    /// correlated by stream instead.
    pub const fn stream_per_request(self) -> bool {
        self.0 >= 17
    }

    /// Draft 12+: the publisher assigns the track alias (in SUBSCRIBE_OK),
    /// before the subscriber did (in SUBSCRIBE).
    pub const fn publisher_assigns_track_alias(self) -> bool {
        self.0 >= 12
    }

    /// Draft 11+: a control message's length is a `u16`, before a varint.
    pub const fn message_length_is_fixed_u16(self) -> bool {
        self.0 >= 11
    }

    /// Draft 15+: a KVP's type is delta-encoded from the previous one (sorted
    /// by type), before it was absolute and in original order.
    pub const fn kvp_type_delta_encoded(self) -> bool {
        self.0 >= 15
    }

    /// Drafts 7–10: parameters are (type, length, bytes), later ones
    /// `KeyValuePairs`.
    pub const fn legacy_parameter_encoding(self) -> bool {
        self.0 <= 10
    }

    /// Key-value pair values of at most 2^16-1 bytes.
    pub const fn limits_pair_values(self) -> bool {
        self.0 >= 11
    }

    /// MAX_REQUEST_ID limits the peer's requests (MAX_SUBSCRIBE_ID before draft 11), QUIC's stream limit from draft 17.
    pub const fn limits_requests(self) -> bool {
        self.0 <= 16
    }

    /// REQUEST_OK and REQUEST_ERROR answer every request, before each request has its own replies.
    pub const fn has_generic_replies(self) -> bool {
        self.0 >= 15
    }

    /// REQUEST_UPDATE changes what its parameters name and is answered. SUBSCRIBE_UPDATE before:
    /// fixed fields naming the whole subscription, no reply.
    pub const fn request_update_has_parameters(self) -> bool {
        self.0 >= 15
    }

    /// An update consumes a Request ID of its own (draft-ietf-moq-transport-14 §9.10).
    pub const fn request_update_has_request_id(self) -> bool {
        self.0 >= 14
    }

    /// Request messages name the request they depend on (draft-ietf-moq-transport-17 §9.2).
    pub const fn has_required_request_id(self) -> bool {
        self.0 == 17
    }
}

/// The draft number, two digits ("07", "18").
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}", self.0)
    }
}

/// A draft number as `18`, `draft18` or `draft-18`.
impl core::str::FromStr for Version {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let digits = s.strip_prefix("draft").unwrap_or(s).trim_start_matches('-');
        let draft: u8 = digits.parse().map_err(|_| format!("invalid MoQ draft version: {s:?}"))?;
        Version::from_draft(draft).ok_or_else(|| {
            let (first, last) = (crate::SUPPORTED_MOQ_VERSIONS[0], crate::SUPPORTED_MOQ_VERSIONS[crate::SUPPORTED_MOQ_VERSIONS.len() - 1]);
            format!("unsupported MoQ draft version: {draft} (supported: {first}-{last})")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_draft_numbers() {
        assert_eq!(["19", "draft19", "draft-19"].map(|s| s.parse::<Version>().ok()), [Version::from_draft(19); 3]);
        assert_eq!("6".parse::<Version>(), Err("unsupported MoQ draft version: 6 (supported: 07-19)".into()));
        assert!("x".parse::<Version>().is_err());
    }

    #[test]
    fn wire_round_trip_only_for_implemented_drafts() {
        assert_eq!(Version::from_wire(0xff000010), Version::from_draft(16));
        assert_eq!(Version::from_draft(16).unwrap().to_wire(), 0xff000010);
        assert_eq!(Version::from_wire(0xff000006), None);
        assert_eq!(Version::from_wire(0xff000013), Version::from_draft(19));
        assert_eq!(Version::from_wire(0xff000014), None);
        assert_eq!(Version::from_wire(0xff0dad01), None, "moq-lite");
    }

    #[test]
    fn alpn_protocol_from_draft_15() {
        let d15 = Version::from_draft(15).unwrap();
        assert_eq!(d15.alpn_protocol().as_deref(), Some("moqt-15"));
        assert_eq!(Version::from_alpn_protocol("moqt-15"), Some(d15));
        assert_eq!(Version::from_draft(14).unwrap().alpn_protocol(), None);
        assert_eq!(Version::from_alpn_protocol("moqt-14"), None);
    }
}
