mod key_value_pair;
mod location;
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

pub use bytes::FromBytes;
pub use bytes::ToBytes;
pub use error::Error;
pub use error::Result;
pub use parameter::Parameter;
pub use parameters::Parameters;
pub use reason_phrase::ReasonPhrase;
pub use setup_parameters::SetupParameters;
pub use role::Role;

pub type RequestId = u64;
pub type Version = u64;
pub type TrackAlias = u64;
pub type SubgroupType = u64;

pub const MOQ_VERSION_DRAFT_07: u64 = 0xff000007;
pub const MOQ_VERSION_DRAFT_08: u64 = 0xff000008;
pub const MOQ_VERSION_DRAFT_09: u64 = 0xff000009;
pub const MOQ_VERSION_DRAFT_10: u64 = 0xff00000A;
pub const MOQ_VERSION_DRAFT_11: u64 = 0xff00000B;
pub const MOQ_VERSION_DRAFT_12: u64 = 0xff00000C;
pub const MOQ_VERSION_DRAFT_13: u64 = 0xff00000D;

// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-control-messages

pub const CLIENT_SETUP_CONTROL_MESSAGE_ID: u64 = 0x20;
pub const SERVER_SETUP_CONTROL_MESSAGE_ID: u64 = 0x21;
pub const SUBSCRIBE_CONTROL_MESSAGE_ID: u64 = 0x3;
pub const SUBSCRIBE_OK_CONTROL_MESSAGE_ID: u64 = 0x4;
pub const SUBSCRIBE_ERROR_CONTROL_MESSAGE_ID: u64 = 0x5;
pub const ANNOUNCE_CONTROL_MESSAGE_ID: u64 = 0x6;
pub const ANNOUNCE_OK_CONTROL_MESSAGE_ID: u64 = 0x7;
pub const UNSUBSCRIBE_NAMESPACE_MESSAGE_ID: u64 = 0x14;
pub const REQUEST_BLOCKED_CONTROL_MESSAGE_ID: u64 = 0x1A;
pub const SUBSCRIBE_DONE_CONTROL_MESSAGE_ID: u64 = 0xB;
pub const CLIENT_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10: u64 = 0x40;
pub const SERVER_SETUP_CONTROL_MESSAGE_ID_VERSION_UNTIL_10: u64 = 0x41;

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

const FETCH_HEADER_SUBGROUP_STREAM_TYPE_ID: u64 = 0x5;

/// used from draft 10 to draft 13
const SUBGROUP_UNI_STREAM_TYPE_IDS: [u64; 6] = [0x8, 0x9, 0xA, 0xB, 0xC, 0xD];
/// https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-data-streams-and-datagrams
const FETCH_UNI_STREAM_TYPE_ID: u64 = 0x05;

pub const RESET_STREAM_CODE_INTERNAL_ERROR: u64 = 0x0;
pub const RESET_STREAM_CODE_CANCELED: u64 = 0x1;
pub const RESET_STREAM_CODE_DELIVERY_TIMEOUT: u64 = 0x2;
pub const RESET_STREAM_CODE_SESSION_CLOSED: u64 = 0x3;
