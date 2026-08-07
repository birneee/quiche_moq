mod key_value_pair;
mod key_value_pairs;
pub mod location;
mod reason_phrase;
mod setup_parameters;
pub mod object;
pub mod subgroup;
mod parameters;
mod tuple;
mod namespace;
mod parameter;
mod role;
mod bytes;
mod error;
mod octets;
pub mod control_message;
mod namespace_trackname;
mod version;
pub mod vi64;

pub use bytes::FromBytes;
pub use bytes::ToBytes;
pub use key_value_pair::{KeyValuePair, KeyValuePairValue};
pub use key_value_pairs::KeyValuePairs;
pub use error::Error;
pub use error::Result;
pub use parameter::Parameter;
pub use parameters::Parameters;
pub use reason_phrase::ReasonPhrase;
pub use setup_parameters::SetupParameters;
pub use role::Role;
pub use location::Location;
pub use namespace::Namespace;
pub use tuple::Tuple;
pub use namespace_trackname::NamespaceTrackname;
pub use version::Version;
pub use version::{
    alpn_protocol_string, version_from_alpn_protocol_string, version_to_name, version_from_draft_number,
    publisher_assigns_track_alias, stream_per_request, message_length_is_fixed_u16, kvp_type_delta_encoded,
    negotiates_version_in_band, legacy_parameter_encoding,
};
pub use control_message::RequestErrorMessage;

pub type RequestId = u64;
pub type TrackAlias = u64;
pub type SubgroupType = u64;

pub const SUPPORTED_MOQ_VERSIONS: &[Version] = &[
    MOQ_VERSION_DRAFT_07,
    MOQ_VERSION_DRAFT_08,
    MOQ_VERSION_DRAFT_09,
    MOQ_VERSION_DRAFT_10,
    MOQ_VERSION_DRAFT_11,
    MOQ_VERSION_DRAFT_12,
    MOQ_VERSION_DRAFT_13,
    MOQ_VERSION_DRAFT_14,
    MOQ_VERSION_DRAFT_15,
    MOQ_VERSION_DRAFT_16,
    MOQ_VERSION_DRAFT_17,
    MOQ_VERSION_DRAFT_18,
    MOQ_VERSION_LITE_01_BY_KIXELATED,
];

/// Selection precedence for automatic version negotiation, most-preferred
/// last. Deliberately independent of the raw `Version` integer value: e.g.
/// `MOQ_VERSION_LITE_01_BY_KIXELATED`'s value (0xff0dad01) is numerically
/// larger than any IETF draft's, but it isn't "newer" and isn't a drop-in
/// substitute (e.g. `ServerSetupMessage::message_id_for_version` doesn't
/// handle it) — so picking the raw numeric max is wrong. Least-preferred
/// first; consumers should pick the last entry both sides support.
pub const MOQ_VERSION_PRIORITY: &[Version] = &[
    MOQ_VERSION_LITE_01_BY_KIXELATED,
    MOQ_VERSION_DRAFT_07,
    MOQ_VERSION_DRAFT_08,
    MOQ_VERSION_DRAFT_09,
    MOQ_VERSION_DRAFT_10,
    MOQ_VERSION_DRAFT_11,
    MOQ_VERSION_DRAFT_12,
    MOQ_VERSION_DRAFT_13,
    MOQ_VERSION_DRAFT_14,
    MOQ_VERSION_DRAFT_15,
    MOQ_VERSION_DRAFT_16,
    // Not in SUPPORTED_MOQ_VERSIONS (so never auto-offered by default), but present
    // here so negotiation succeeds when a peer explicitly opts in (e.g. --setup-version
    // draft17, which forces Config::supported_versions to just that one entry).
    MOQ_VERSION_DRAFT_17,
    MOQ_VERSION_DRAFT_18,
];

pub const MOQ_VERSION_DRAFT_07: Version = 0xff000007;
pub const MOQ_VERSION_DRAFT_08: Version = 0xff000008;
pub const MOQ_VERSION_DRAFT_09: Version = 0xff000009;
pub const MOQ_VERSION_DRAFT_10: Version = 0xff00000a;
pub const MOQ_VERSION_DRAFT_11: Version = 0xff00000b;
pub const MOQ_VERSION_DRAFT_12: Version = 0xff00000c;
pub const MOQ_VERSION_DRAFT_13: Version = 0xff00000d;
pub const MOQ_VERSION_DRAFT_14: Version = 0xff00000e;
pub const MOQ_VERSION_DRAFT_15: Version = 0xff00000f;
pub const MOQ_VERSION_DRAFT_16: Version = 0xff000010;
/// Drops SETUP-based version negotiation (ALPN/WT-Available-Protocols instead) and
/// moves the control stream from one bidi stream to a pair of uni streams — this
/// codebase deliberately keeps the old CLIENT_SETUP/SERVER_SETUP in-band negotiation
/// shape anyway, as a testing simplification. See [`crate::vi64`] for the new vi64
/// varint encoding this version introduces.
pub const MOQ_VERSION_DRAFT_17: Version = 0xff000011;
/// Adds the FIRST_OBJECT bit to SUBGROUP_HEADER and a 7-byte vi64 length; see
/// [`crate::vi64`].
pub const MOQ_VERSION_DRAFT_18: Version = 0xff000012;
/// draft-lcurley-moq-lite-{01..05}: a separate, unrelated protocol (not an
/// MoQT draft) that negotiates purely via ALPN ("moq-lite-xx", no in-band
/// version field). `LITE_01` is kept in `SUPPORTED_MOQ_VERSIONS`/
/// `MOQ_VERSION_PRIORITY` only because a real client was observed offering
/// this exact value in CLIENT_SETUP's version list alongside a real draft
/// (see `client_setup.rs`'s `decode_draft07` test) — not because this crate
/// implements moq-lite. `LITE_02..05` are deliberately *not* added to either
/// list: they just reserve the numeric space so a future version bump can't
/// collide with it.
pub const MOQ_VERSION_LITE_01_BY_KIXELATED: Version = 0xff0dad01;
pub const MOQ_VERSION_LITE_02_BY_KIXELATED: Version = 0xff0dad02;
pub const MOQ_VERSION_LITE_03_BY_KIXELATED: Version = 0xff0dad03;
pub const MOQ_VERSION_LITE_04_BY_KIXELATED: Version = 0xff0dad04;
pub const MOQ_VERSION_LITE_05_BY_KIXELATED: Version = 0xff0dad05;

// Control Message IDs (draft-16)
// https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#name-control-messages

pub const CLIENT_SETUP_MESSAGE_ID: u64 = 0x20;
pub const SERVER_SETUP_MESSAGE_ID: u64 = 0x21;
/// 0x40 in drafts <= 10
pub const CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10: u64 = 0x40;
/// 0x41 in drafts <= 10
pub const SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10: u64 = 0x41;

pub const SUBSCRIBE_MESSAGE_ID: u64 = 0x03;
pub const SUBSCRIBE_OK_MESSAGE_ID: u64 = 0x04;
/// REQUEST_ERROR in draft-15+, SUBSCRIBE_ERROR in drafts <= 14
pub const REQUEST_ERROR_MESSAGE_ID: u64 = 0x05;
/// REQUEST_UPDATE in draft-16, SUBSCRIBE_UPDATE in drafts <= 15
pub const REQUEST_UPDATE_MESSAGE_ID: u64 = 0x02;
pub const UNSUBSCRIBE_MESSAGE_ID: u64 = 0x0A;

pub const PUBLISH_MESSAGE_ID: u64 = 0x1D;
pub const PUBLISH_OK_MESSAGE_ID: u64 = 0x1E;
/// PUBLISH_ERROR in draft-14 only
pub const PUBLISH_ERROR_MESSAGE_ID: u64 = 0x1F;
/// PUBLISH_DONE in draft-14+, SUBSCRIBE_DONE in drafts <= 13
pub const PUBLISH_DONE_MESSAGE_ID: u64 = 0x0B;

/// PUBLISH_NAMESPACE in draft-14+, ANNOUNCE in drafts <= 13
pub const PUBLISH_NAMESPACE_MESSAGE_ID: u64 = 0x06;
/// REQUEST_OK in draft-15+, PUBLISH_NAMESPACE_OK in draft-14, ANNOUNCE_OK in drafts <= 13
pub const REQUEST_OK_MESSAGE_ID: u64 = 0x07;
/// NAMESPACE in draft-16, PUBLISH_NAMESPACE_ERROR in draft-14, ANNOUNCE_ERROR in drafts <= 13
pub const NAMESPACE_MESSAGE_ID: u64 = 0x08;
/// UNANNOUNCE in drafts <= 13
pub const PUBLISH_NAMESPACE_DONE_MESSAGE_ID: u64 = 0x09;
/// ANNOUNCE_CANCEL in drafts <= 13
pub const PUBLISH_NAMESPACE_CANCEL_MESSAGE_ID: u64 = 0x0C;
/// NAMESPACE_DONE in draft-16, TRACK_STATUS_OK in draft-13/14, TRACK_STATUS in draft-07
pub const NAMESPACE_DONE_MESSAGE_ID: u64 = 0x0E;

/// TRACK_STATUS_REQUEST in draft-07
pub const TRACK_STATUS_MESSAGE_ID: u64 = 0x0D;
/// TRACK_STATUS_ERROR in drafts 13-14
pub const TRACK_STATUS_ERROR_MESSAGE_ID: u64 = 0x0F;

pub const GOAWAY_MESSAGE_ID: u64 = 0x10;
/// MAX_SUBSCRIBE_ID in draft-07
pub const MAX_REQUEST_ID_MESSAGE_ID: u64 = 0x15;
pub const REQUESTS_BLOCKED_MESSAGE_ID: u64 = 0x1A;

pub const FETCH_MESSAGE_ID: u64 = 0x16;
pub const FETCH_OK_MESSAGE_ID: u64 = 0x18;
pub const FETCH_CANCEL_MESSAGE_ID: u64 = 0x17;
/// FETCH_ERROR in drafts 13-14
pub const FETCH_ERROR_MESSAGE_ID: u64 = 0x19;

/// SUBSCRIBE_ANNOUNCES in draft-07
pub const SUBSCRIBE_NAMESPACE_MESSAGE_ID: u64 = 0x11;
/// SUBSCRIBE_NAMESPACE_OK in drafts 13-14
pub const SUBSCRIBE_NAMESPACE_OK_MESSAGE_ID: u64 = 0x12;
/// SUBSCRIBE_NAMESPACE_ERROR in drafts 13-14
pub const SUBSCRIBE_NAMESPACE_ERROR_MESSAGE_ID: u64 = 0x13;
/// UNSUBSCRIBE_ANNOUNCES in draft-07
pub const UNSUBSCRIBE_NAMESPACE_MESSAGE_ID: u64 = 0x14;

/// https://www.rfc-editor.org/rfc/rfc9000#name-variable-length-integer-enc
pub const MAX_VARINT_LEN: usize = 8;


/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.html#name-role
/// only valid from draft tbd to draft 7.
pub const ROLE_SETUP_PARAMETER_ID: u64 = 0x00;
///https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-path
pub const PATH_SETUP_PARAMETER_ID: u64 = 0x01;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-max_request_id
pub const MAX_REQUEST_ID_SETUP_PARAMETER_ID: u64 = 0x02;

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.html#name-role
/// only valid from draft tbd to draft 7.
pub const PUBLISHER_ROLE_ID: u64 = 0x01;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.html#name-role
/// only valid from draft tbd to draft 7.
pub const SUBSCRIBER_ROLE_ID: u64 = 0x02;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-07.html#name-role
/// only valid from draft tbd to draft 7.
pub const PUB_SUB_ROLE_ID: u64 = 0x03;

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-max_request_id
pub const DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER: u64 = 0;

/// DELIVERY_TIMEOUT parameter type ID (draft-16 section 9.2.2.2) and Track Extension type ID (section 11.1).
/// Renamed OBJECT_DELIVERY_TIMEOUT in draft-18 (section 10.2.4, same type ID) when SUBGROUP_DELIVERY_TIMEOUT
/// was split out as a separate parameter. Even type → varint milliseconds.
pub const DELIVERY_TIMEOUT_PARAMETER_ID: u64 = 0x02;
/// SUBGROUP_DELIVERY_TIMEOUT parameter type ID, added in draft-18 (section 10.2.3) alongside the
/// renamed OBJECT_DELIVERY_TIMEOUT (see [`DELIVERY_TIMEOUT_PARAMETER_ID`]). Even type → varint milliseconds.
/// Not otherwise wired up (moq_core doesn't distinguish subgroup vs. object delivery timeouts).
pub const SUBGROUP_DELIVERY_TIMEOUT_PARAMETER_ID: u64 = 0x06;
/// FILL_TIMEOUT parameter type ID, added in draft-18 (section 10.2.5). Even type → varint milliseconds.
/// Not otherwise wired up (moq_core has no FILL_TIMEOUT handling).
pub const FILL_TIMEOUT_PARAMETER_ID: u64 = 0x0A;
/// EXPIRES parameter type ID (draft-16 section 9.2.2.6). Even type → varint value.
pub const EXPIRES_PARAMETER_ID: u64 = 0x8;
/// LARGEST_OBJECT parameter type ID (draft-16 section 9.2.2.7). Odd type → length-prefixed Location.
pub const LARGEST_OBJECT_PARAMETER_ID: u64 = 0x9;
/// DEFAULT_PUBLISHER_GROUP_ORDER Track Extension type ID (draft-16 section 11.1). Even type → varint value.
pub const DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID: u64 = 0x22;
/// FORWARD parameter type ID (draft-16 section 9.2.2.8): SUBSCRIBE, REQUEST_UPDATE,
/// PUBLISH, PUBLISH_OK, SUBSCRIBE_NAMESPACE. Even type → varint, 0 (don't forward) or 1 (forward).
pub const FORWARD_PARAMETER_ID: u64 = 0x10;
/// SUBSCRIBER_PRIORITY parameter type ID (draft-16 section 9.2.2.3): SUBSCRIBE, FETCH,
/// REQUEST_UPDATE, PUBLISH_OK. Even type → varint 0-255, default 128 if omitted.
pub const SUBSCRIBER_PRIORITY_PARAMETER_ID: u64 = 0x20;
/// SUBSCRIPTION_FILTER parameter type ID (draft-16 section 9.2.2.5): SUBSCRIBE, PUBLISH_OK,
/// REQUEST_UPDATE. Odd type → length-prefixed Subscription Filter (section 5.1.2): Filter
/// Type, optional Start Location, optional End Group — the same fields draft <= 14 sent as
/// fixed SUBSCRIBE fields, packed into one parameter from draft 15 on.
pub const SUBSCRIPTION_FILTER_PARAMETER_ID: u64 = 0x21;
/// GROUP_ORDER parameter type ID (draft-16 section 9.2.2.4): SUBSCRIBE, PUBLISH_OK, FETCH.
/// Even type → varint enum, Ascending (0x1) or Descending (0x2).
pub const GROUP_ORDER_PARAMETER_ID: u64 = 0x22;
/// MAX_CACHE_DURATION Track Extension type ID (draft-16 section 11.1.1). Even type → varint milliseconds.
pub const MAX_CACHE_DURATION_EXTENSION_ID: u64 = 0x04;

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe
pub const LARGEST_OBJECT_FILTER_ID: u64 = 0x2;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe
pub const NEXT_GROUP_START_FILTER_ID: u64 = 0x1;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe
pub const ABSOLUTE_START_FILTER_ID: u64 = 0x3;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe
pub const ABSOLUTE_RANGE_FILTER_ID: u64 = 0x4;

/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming
const MIN_TRACK_NAMESPACE_TUPLE_LENGTH: usize = 1;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming
const MAX_TRACK_NAMESPACE_TUPLE_LENGTH: usize = 32;
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming
const MAX_FULL_TRACK_NAME_LEN: usize = 4096;

/// used from draft 7 to draft 10
const STREAM_HEADER_SUBGROUP_STREAM_TYPE_ID: u64 = 0x4;

#[allow(unused)]
const FETCH_HEADER_SUBGROUP_STREAM_TYPE_ID: u64 = 0x5;

/// used from draft 10 to draft 13
const SUBGROUP_UNI_STREAM_TYPE_IDS: [u64; 6] = [0x8, 0x9, 0xA, 0xB, 0xC, 0xD];

/// used from draft 18
pub const PADDING_STREAM_TYPE_ID: u64 = 0x132B3E28;

/// used from draft 18
pub const PADDING_DATAGRAM_TYPE_ID: u64 = 0x132B3E29;

#[allow(unused)]
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-data-streams-and-datagrams
const FETCH_UNI_STREAM_TYPE_ID: u64 = 0x05;

pub const RESET_STREAM_CODE_INTERNAL_ERROR: u64 = 0x0;
pub const RESET_STREAM_CODE_CANCELED: u64 = 0x1;
pub const RESET_STREAM_CODE_DELIVERY_TIMEOUT: u64 = 0x2;
pub const RESET_STREAM_CODE_SESSION_CLOSED: u64 = 0x3;

pub const NO_ERROR: u32 = 0x0;
pub const INTERNAL_ERROR: u32 = 0x1;
pub const UNAUTHORIZED: u32 = 0x2;
pub const PROTOCOL_VIOLATION: u32 = 0x3;
pub const INVALID_REQUEST_ID: u32 = 0x4;
pub const DUPLICATE_TRACK_ALIAS: u32 = 0x5;
pub const KEY_VALUE_FORMATTING_ERROR: u32 = 0x6;
pub const TOO_MANY_REQUESTS: u32 = 0x7;
pub const INVALID_PATH: u32 = 0x8;
pub const MALFORMED_PATH: u32 = 0x9;
pub const GOAWAY_TIMEOUT: u32 = 0x10;
pub const CONTROL_MESSAGE_TIMEOUT: u32 = 0x11;
pub const DATA_STREAM_TIMEOUT: u32 = 0x12;
pub const AUTH_TOKEN_CACHE_OVERFLOW: u32 = 0x13;
pub const DUPLICATE_AUTH_TOKEN_ALIAS: u32 = 0x14;
pub const VERSION_NEGOTIATION_FAILED: u32 = 0x15;
pub const MALFORMED_AUTH_TOKEN: u32 = 0x16;
pub const UNKNOWN_AUTH_TOKEN_ALIAS: u32 = 0x17;
pub const EXPIRED_AUTH_TOKEN: u32 = 0x18;
pub const INVALID_AUTHORITY: u32 = 0x19;
pub const MALFORMED_AUTHORITY: u32 = 0x1A;

pub type ErrorCode = u64;

pub const REQUEST_ERROR_INTERNAL_ERROR: ErrorCode = 0x0;
pub const REQUEST_ERROR_UNAUTHORIZED: ErrorCode = 0x1;
pub const REQUEST_ERROR_TIMEOUT: ErrorCode = 0x2;
pub const REQUEST_ERROR_NOT_SUPPORTED: ErrorCode = 0x3;
pub const REQUEST_ERROR_MALFORMED_AUTH_TOKEN: ErrorCode = 0x4;
pub const REQUEST_ERROR_EXPIRED_AUTH_TOKEN: ErrorCode = 0x5;
pub const REQUEST_ERROR_DOES_NOT_EXIST: ErrorCode = 0x10;
pub const REQUEST_ERROR_INVALID_RANGE: ErrorCode = 0x11;
pub const REQUEST_ERROR_MALFORMED_TRACK: ErrorCode = 0x12;
pub const REQUEST_ERROR_DUPLICATE_SUBSCRIPTION: ErrorCode = 0x19;
pub const REQUEST_ERROR_UNINTERESTED: ErrorCode = 0x20;
pub const REQUEST_ERROR_PREFIX_OVERLAP: ErrorCode = 0x30;
pub const REQUEST_ERROR_JOINING_REQUEST_ID: ErrorCode = 0x32;
