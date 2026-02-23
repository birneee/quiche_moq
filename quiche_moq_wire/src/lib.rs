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
pub use version::version_to_name;

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
    MOQ_VERSION_LITE_01_BY_KIXELATED,
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
pub const MOQ_VERSION_LITE_01_BY_KIXELATED: Version = 0xff0dad01;

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

/// EXPIRES parameter type ID (draft-16 section 9.2.2.6). Even type → varint value.
pub const EXPIRES_PARAMETER_ID: u64 = 0x8;
/// LARGEST_OBJECT parameter type ID (draft-16 section 9.2.2.7). Odd type → length-prefixed Location.
pub const LARGEST_OBJECT_PARAMETER_ID: u64 = 0x9;
/// DEFAULT_PUBLISHER_GROUP_ORDER Track Extension type ID (draft-16 section 11.1). Even type → varint value.
pub const DEFAULT_PUBLISHER_GROUP_ORDER_EXTENSION_ID: u64 = 0x22;

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
