#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
#[cfg(test)]
#[macro_use]
extern crate std;

/// What std's prelude adds over core's.
mod prelude {
    #[allow(unused_imports)]
    pub(crate) use alloc::{borrow::ToOwned, boxed::Box, format, string::{String, ToString}, vec, vec::Vec};
}

mod codes;
mod ids;
mod key_value_pair;
mod key_value_pairs;
mod message_parameters;
pub mod property;
pub mod location;
mod setup_options;
pub mod object;
pub mod subgroup;
pub mod range_filter;
pub mod fetch;
pub mod datagram;
mod namespace;
mod role;
mod bytes;
mod error;
mod group_order;
pub mod control_message;
mod namespace_trackname;
mod version;
#[cfg(test)]
mod fuzz_tests;
#[cfg(test)]
mod golden_tests;

pub use bytes::FromBytes;
pub use moq_wire_primitives::{DecodeError, EncodeError, Frame, Mode, Reader, Stream, VarIntForm, Writer};
pub use bytes::ToBytes;
pub use key_value_pair::{BytesKey, KeyValuePair, VarintKey};
pub use key_value_pairs::KeyValuePairs;
pub use error::Error;
pub use error::Result;
pub use group_order::GroupOrder;
/// Diagnostic text of an error, at most 1024 bytes of UTF-8 (draft-ietf-moq-transport-16 §1.4.3).
pub type ReasonPhrase = moq_wire_primitives::BoundedString<1024>;
pub use setup_options::SetupOptions;
pub use role::Role;
pub use location::{Location, SubgroupLocation};
pub use namespace::{Namespace, NamespaceTuple};
pub use namespace_trackname::NamespaceTrackname;
pub use version::Version;
pub use control_message::{RequestErrorMessage, RequestRef};
pub use control_message::subscribe::LocationFilter;

pub use codes::{PublishDoneStatus, RequestErrorCode, ResetCode, SessionCloseCode};
pub use ids::{RequestId, TrackAlias};

/// Ascending, so the last one both sides support is the newest.
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
    MOQ_VERSION_DRAFT_19,
];

const fn draft(n: u8) -> Version {
    match Version::from_draft(n) {
        Some(v) => v,
        None => panic!("draft not implemented"),
    }
}

pub const MOQ_VERSION_DRAFT_07: Version = draft(7);
pub const MOQ_VERSION_DRAFT_08: Version = draft(8);
pub const MOQ_VERSION_DRAFT_09: Version = draft(9);
pub const MOQ_VERSION_DRAFT_10: Version = draft(10);
pub const MOQ_VERSION_DRAFT_11: Version = draft(11);
pub const MOQ_VERSION_DRAFT_12: Version = draft(12);
pub const MOQ_VERSION_DRAFT_13: Version = draft(13);
pub const MOQ_VERSION_DRAFT_14: Version = draft(14);
pub const MOQ_VERSION_DRAFT_15: Version = draft(15);
pub const MOQ_VERSION_DRAFT_16: Version = draft(16);
/// Drops SETUP-based version negotiation (ALPN/WT-Available-Protocols instead) and
/// moves the control stream from one bidi stream to a pair of uni streams, replaces
/// CLIENT_SETUP/SERVER_SETUP with SETUP, removes MAX_REQUEST_ID and introduces the
/// leading-ones varint, see [`VarIntForm`].
pub const MOQ_VERSION_DRAFT_17: Version = draft(17);
/// Adds the FIRST_OBJECT bit to SUBGROUP_HEADER and a 7-byte varint length, see
/// [`VarIntForm`].
pub const MOQ_VERSION_DRAFT_18: Version = draft(18);
/// Adds range filters and MAX_REQUEST_UPDATES, moves GROUP_ORDER from PUBLISH_OK to SUBSCRIBE_TRACKS.
pub const MOQ_VERSION_DRAFT_19: Version = draft(19);

// Control Message IDs (draft-16)
// https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html#name-control-messages

pub const CLIENT_SETUP_MESSAGE_ID: u64 = 0x20;
pub const SERVER_SETUP_MESSAGE_ID: u64 = 0x21;
/// SETUP from draft 17, for both endpoints
pub const SETUP_MESSAGE_ID: u64 = 0x2F00;
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
/// TRACK_STATUS_OK of drafts 13 and 14, the TRACK_STATUS reply before
pub const TRACK_STATUS_OK_MESSAGE_ID: u64 = 0x0E;
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
/// SUBSCRIBE_NAMESPACE from draft 18, on its own request stream
pub const SUBSCRIBE_NAMESPACE_REQUEST_MESSAGE_ID: u64 = 0x50;
/// SUBSCRIBE_TRACKS from draft 18
pub const SUBSCRIBE_TRACKS_MESSAGE_ID: u64 = 0x51;
/// PUBLISH_BLOCKED from draft 18
pub const PUBLISH_BLOCKED_MESSAGE_ID: u64 = 0xF;

/// <https://www.rfc-editor.org/rfc/rfc9000#name-variable-length-integer-enc>
pub const MAX_VARINT_LEN: usize = 8;

/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-11.html#name-max_request_id>
pub const DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER: u64 = 0;

/// SUBSCRIBER_PRIORITY if omitted.
pub const DEFAULT_SUBSCRIBER_PRIORITY: u8 = 128;

/// Publisher Priority if neither the track nor the header names one (draft-ietf-moq-transport-18 §12.4).
pub const DEFAULT_PUBLISHER_PRIORITY: u8 = 128;

/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe>
pub const LARGEST_OBJECT_FILTER_ID: u64 = 0x2;
/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe>
pub const NEXT_GROUP_START_FILTER_ID: u64 = 0x1;
/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe>
pub const ABSOLUTE_START_FILTER_ID: u64 = 0x3;
/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-subscribe>
pub const ABSOLUTE_RANGE_FILTER_ID: u64 = 0x4;

/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming>
pub(crate) const MIN_TRACK_NAMESPACE_TUPLE_LENGTH: usize = 1;
/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming>
pub(crate) const MAX_TRACK_NAMESPACE_TUPLE_LENGTH: usize = 32;
/// <https://www.ietf.org/archive/id/draft-ietf-moq-transport-13.html#name-track-naming>
pub(crate) const MAX_FULL_TRACK_NAME_LEN: usize = 4096;

/// used from draft 18
pub const PADDING_STREAM_TYPE_ID: u64 = 0x132B3E28;

