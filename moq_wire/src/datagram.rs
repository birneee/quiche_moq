//! OBJECT_DATAGRAM (draft-ietf-moq-transport-19 §11.3.1) from draft 14, whose Type is a set of flags.
// TODO the datagrams of drafts 7 to 13, implement when needed

use crate::error::{Error, Result};
use crate::key_value_pairs::KeyValuePairs;
use crate::object::ObjectStatus;
use crate::prelude::*;
use crate::{Location, TrackAlias, Version};

const PROPERTIES: u64 = 0x01;
const END_OF_GROUP: u64 = 0x02;
const ZERO_OBJECT_ID: u64 = 0x04;
/// from draft 15
const DEFAULT_PRIORITY: u64 = 0x08;
const STATUS: u64 = 0x20;

/// One Object in a datagram, its payload the rest of the datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectDatagram {
    pub track_alias: TrackAlias,
    pub location: Location,
    /// `None`: the subscription's, from draft 15
    pub publisher_priority: Option<u8>,
    /// No Object after this one in its Group.
    pub end_of_group: bool,
    pub properties: KeyValuePairs,
    /// `Some` for an Object without payload.
    pub status: Option<ObjectStatus>,
}

impl ObjectDatagram {
    /// Whether `version` has datagrams of this form.
    pub fn accepts(version: Version) -> bool {
        version.draft() >= 14
    }

    /// The datagram and its payload.
    pub fn decode(datagram: &[u8], version: Version) -> Result<(Self, &[u8])> {
        if !Self::accepts(version) {
            return Err(Error::Unsupported("datagrams before draft 14".into()));
        }
        let mut b = version.frame_reader(datagram);
        let ty = b.varint()?;
        let defined = if version.draft() == 14 { PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | STATUS } else { PROPERTIES | END_OF_GROUP | ZERO_OBJECT_ID | DEFAULT_PRIORITY | STATUS };
        let status_ends_group = ty & STATUS != 0 && ty & END_OF_GROUP != 0;
        let zero_id_status_in_14 = version.draft() == 14 && ty & STATUS != 0 && ty & ZERO_OBJECT_ID != 0;
        if ty & !defined != 0 || status_ends_group || zero_id_status_in_14 {
            return Err(Error::ProtocolViolation(format!("datagram type {ty:#x}")));
        }
        let track_alias = TrackAlias(b.varint()?);
        let group = b.varint()?;
        let object = if ty & ZERO_OBJECT_ID != 0 { 0 } else { b.varint()? };
        let publisher_priority = if ty & DEFAULT_PRIORITY != 0 { None } else { Some(b.u8()?) };
        let properties = if ty & PROPERTIES != 0 {
            let len = b.varint()?;
            if len == 0 {
                return Err(Error::ProtocolViolation("datagram with the PROPERTIES bit and no properties".into()));
            }
            KeyValuePairs::decode_to_end(&mut b.frame(len)?, version)?
        } else {
            KeyValuePairs::new()
        };
        let status = if ty & STATUS != 0 { Some(ObjectStatus::decode(b.varint()?, version)?) } else { None };
        if status.is_some_and(|s| s != ObjectStatus::Normal) && !properties.is_empty() {
            return Err(Error::ProtocolViolation("properties on an Object that isn't Normal".into()));
        }
        let payload = if status.is_some() { b.finish()?; &[][..] } else { b.rest() };
        let datagram = Self { track_alias, location: Location { group, object }, publisher_priority, end_of_group: ty & END_OF_GROUP != 0, properties, status };
        Ok((datagram, payload))
    }

    /// The datagram carrying `payload`, empty with a `status`.
    pub fn encode(&self, payload: &[u8], version: Version) -> Result<Vec<u8>> {
        if !Self::accepts(version) {
            return Err(Error::Unencodable("datagrams before draft 14"));
        }
        if self.status.is_some() && !payload.is_empty() {
            return Err(Error::Unencodable("a status datagram with payload"));
        }
        if self.status.is_some() && self.end_of_group {
            return Err(Error::Unencodable("a status datagram ending its Group"));
        }
        let publisher_priority = match (self.publisher_priority, version.draft()) {
            (None, ..=14) => return Err(Error::Unencodable("DEFAULT_PRIORITY before draft 15")),
            (priority, _) => priority,
        };
        let flag = |set: bool, bit: u64| if set { bit } else { 0 };
        let status = self.status;
        let zero_object_id = self.location.object == 0 && !(version.draft() == 14 && status.is_some());
        let ty = flag(!self.properties.is_empty(), PROPERTIES) | flag(self.end_of_group, END_OF_GROUP) | flag(zero_object_id, ZERO_OBJECT_ID)
            | flag(publisher_priority.is_none(), DEFAULT_PRIORITY) | flag(status.is_some(), STATUS);
        let mut buf = Vec::new();
        let mut b = version.writer(&mut buf);
        b.varint(ty)?;
        b.varint(self.track_alias.0)?;
        b.varint(self.location.group)?;
        if !zero_object_id {
            b.varint(self.location.object)?;
        }
        if let Some(priority) = publisher_priority {
            b.u8(priority);
        }
        if !self.properties.is_empty() {
            b.with_varint_length(|b| self.properties.encode_uncounted(b, version))?;
        }
        match status {
            Some(status) => b.varint(status.encode())?,
            None => b.bytes(payload),
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyValuePair, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18, VarintKey};

    fn datagram(object: u64) -> ObjectDatagram {
        ObjectDatagram {
            track_alias: TrackAlias(3),
            location: Location { group: 5, object },
            publisher_priority: Some(128),
            end_of_group: false,
            properties: KeyValuePairs::new(),
            status: None,
        }
    }

    #[rstest::rstest]
    fn round_trips(
        #[values(MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_18)] version: Version,
        #[values(0, 7)] object: u64,
        #[values(false, true)] end_of_group: bool,
        #[values(false, true)] with_properties: bool,
    ) {
        let mut properties = KeyValuePairs::new();
        if with_properties {
            properties.insert(KeyValuePair::Varint(VarintKey::new(2), 9));
        }
        let sent = ObjectDatagram { end_of_group, properties, ..datagram(object) };
        let bytes = sent.encode(b"payload", version).unwrap();
        let (received, payload) = ObjectDatagram::decode(&bytes, version).unwrap();
        assert_eq!(received, sent);
        assert_eq!(payload, b"payload");
    }

    #[rstest::rstest]
    fn status_round_trips(#[values(MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_18)] version: Version, #[values(0, 7)] object: u64) {
        let sent = ObjectDatagram { status: Some(ObjectStatus::EndOfGroup), ..datagram(object) };
        let bytes = sent.encode(&[], version).unwrap();
        assert_eq!(ObjectDatagram::decode(&bytes, version).unwrap(), (sent, &[][..]));
    }

    #[test]
    fn default_priority_from_draft_15() {
        let sent = ObjectDatagram { publisher_priority: None, ..datagram(1) };
        assert!(sent.encode(b"x", MOQ_VERSION_DRAFT_14).is_err());
        let bytes = sent.encode(b"x", MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(bytes[0], 0x08);
        assert_eq!(ObjectDatagram::decode(&bytes, MOQ_VERSION_DRAFT_16).unwrap().0, sent);
    }

    /// draft-ietf-moq-transport-18 §11.3.1
    #[rstest::rstest]
    #[case::status_and_end_of_group(&[0x22, 3, 5, 7, 128, 3])]
    #[case::undefined_bit(&[0x10, 3, 5, 7, 128])]
    #[case::empty_properties(&[0x01, 3, 5, 7, 128, 0])]
    #[case::properties_on_status_other_than_normal(&[0x21, 3, 5, 7, 128, 2, 2, 9, 3])]
    #[case::undefined_status(&[0x20, 3, 5, 7, 128, 2])]
    fn invalid_datagrams_are_protocol_violations(#[case] bytes: &[u8]) {
        assert!(matches!(ObjectDatagram::decode(bytes, MOQ_VERSION_DRAFT_16), Err(Error::ProtocolViolation(_))));
    }
}
