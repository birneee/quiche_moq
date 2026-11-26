//! The stream answering a FETCH (draft-ietf-moq-transport-19 §11.4.4): FETCH_HEADER, then objects whose
//! fields the Serialization Flags elide by the prior object. Implemented from draft 15.

use crate::prelude::*;
use crate::error::{Error, Result};
use crate::{GroupOrder, KeyValuePairs, Location, Mode, Reader, RequestId, Version, Writer};
use crate::object::ObjectStatus;

/// The stream type of FETCH_HEADER.
pub const FETCH_HEADER_TYPE: u64 = 0x05;

mod flag {
    pub(super) const SUBGROUP_ZERO: u64 = 0x00;
    pub(super) const SUBGROUP_PRIOR: u64 = 0x01;
    pub(super) const SUBGROUP_NEXT: u64 = 0x02;
    pub(super) const SUBGROUP_PRESENT: u64 = 0x03;
    pub(super) const SUBGROUP_MASK: u64 = 0x03;
    pub(super) const OBJECT: u64 = 0x04;
    pub(super) const GROUP: u64 = 0x08;
    pub(super) const PRIORITY: u64 = 0x10;
    pub(super) const PROPERTIES: u64 = 0x20;
    pub(super) const DATAGRAM: u64 = 0x40;
    pub(super) const END_OF_NON_EXISTENT_RANGE: u64 = 0x8C;
    pub(super) const END_OF_UNKNOWN_RANGE: u64 = 0x10C;
}

/// One entry of a fetch stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchEntry {
    Object(FetchObject),
    /// The objects after the prior entry up to `location`, inclusive, don't exist or are unknown, draft 16 on
    EndOfRange { location: Location, unknown: bool },
}

/// An object header of a fetch stream, its payload follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchObject {
    pub location: Location,
    /// `None` for an object of a datagram track, draft 16 on
    pub subgroup_id: Option<u64>,
    pub publisher_priority: u8,
    pub properties: KeyValuePairs,
    pub payload_len: usize,
}

#[derive(Debug, Clone, Copy)]
struct Prior {
    location: Location,
    /// of the last object, not an End of Range
    subgroup_id: Option<Option<u64>>,
    priority: Option<u8>,
}

/// A fetch stream's codec: what the next entry refers back to.
#[derive(Debug, Clone)]
pub struct FetchStream {
    version: Version,
    descending: bool,
    prior: Option<Prior>,
}

impl FetchStream {
    /// The stream of a FETCH answered in `group_order`, which deltas of Group IDs follow from draft 18.
    pub fn new(version: Version, group_order: GroupOrder) -> Self {
        Self { version, descending: group_order == GroupOrder::Descending, prior: None }
    }

    fn supported(&self) -> Result<()> {
        match self.version.draft() {
            ..=14 => Err(Error::Unsupported(format!("fetch streams of draft {}", self.version.draft()))),
            _ => Ok(()),
        }
    }

    pub fn encode_header(w: &mut Writer, request_id: RequestId) -> Result<()> {
        w.varint(FETCH_HEADER_TYPE)?;
        Ok(w.varint(request_id.0)?)
    }

    /// The Request ID after the stream type.
    pub fn decode_header<M: Mode>(r: &mut Reader<'_, M>) -> Result<RequestId> {
        Ok(RequestId(r.varint()?))
    }

    fn deltas(&self) -> bool {
        self.version.draft() >= 18
    }

    /// The Group ID field of `group`, absolute before draft 18 and for the first entry.
    fn group_field(&self, group: u64) -> Result<u64> {
        let out_of_order = || Error::Unencodable("fetch stream groups out of the requested order");
        match self.prior {
            Some(prior) if self.deltas() => {
                let delta = if self.descending { prior.location.group.checked_sub(group) } else { group.checked_sub(prior.location.group) };
                delta.and_then(|d| d.checked_sub(1)).ok_or_else(out_of_order)
            }
            _ => Ok(group),
        }
    }

    fn group_from_field(&self, field: u64) -> Result<u64> {
        let beyond = || Error::ProtocolViolation("fetch stream Group ID beyond its range".into());
        match self.prior {
            Some(prior) if self.deltas() => {
                let step = field.checked_add(1).ok_or_else(beyond)?;
                if self.descending { prior.location.group.checked_sub(step) } else { prior.location.group.checked_add(step) }.ok_or_else(beyond)
            }
            _ => Ok(field),
        }
    }

    /// Appends `entry`, eliding what the prior one implies. Nothing is left behind if it fails.
    pub fn encode(&mut self, w: &mut Writer, entry: &FetchEntry) -> Result<()> {
        w.all_or_nothing(|w| self.encode_entry(w, entry))
    }

    fn encode_entry(&mut self, w: &mut Writer, entry: &FetchEntry) -> Result<()> {
        self.supported()?;
        let draft = self.version.draft();
        let (location, mut flags) = match entry {
            FetchEntry::EndOfRange { .. } if draft < 16 => return Err(Error::Unsupported("End of Range before draft 16".into())),
            FetchEntry::EndOfRange { location, unknown } => (*location, if *unknown { flag::END_OF_UNKNOWN_RANGE } else { flag::END_OF_NON_EXISTENT_RANGE }),
            FetchEntry::Object(object) => (object.location, 0),
        };
        let new_group = self.prior.is_none_or(|prior| prior.location.group != location.group);
        // the prior object's ID plus one, also in a new group
        let object_implied = self.prior.is_some_and(|prior| prior.location.object.checked_add(1) == Some(location.object));
        let FetchEntry::Object(object) = entry else {
            if !new_group && self.deltas() {
                return Err(Error::Unencodable("End of Range within the prior group"));
            }
            w.varint(flags)?;
            self.write_id_fields(w, flags, location)?;
            self.prior = Some(Prior { location, ..self.prior.unwrap_or(Prior { location, subgroup_id: None, priority: None }) });
            return Ok(());
        };
        let prior_subgroup = self.prior.and_then(|prior| prior.subgroup_id);
        flags |= match object.subgroup_id {
            None if draft < 16 => return Err(Error::Unsupported("datagram objects in fetch streams before draft 16".into())),
            None => flag::DATAGRAM,
            Some(0) => flag::SUBGROUP_ZERO,
            Some(id) if prior_subgroup == Some(Some(id)) => flag::SUBGROUP_PRIOR,
            Some(id) if prior_subgroup.flatten().is_some_and(|prior| prior.checked_add(1) == Some(id)) => flag::SUBGROUP_NEXT,
            Some(_) => flag::SUBGROUP_PRESENT,
        };
        if new_group {
            flags |= flag::GROUP;
        }
        if !object_implied {
            flags |= flag::OBJECT;
        }
        let prior_priority = self.prior.and_then(|prior| prior.priority);
        if prior_priority != Some(object.publisher_priority) {
            flags |= flag::PRIORITY;
        }
        if !object.properties.is_empty() {
            flags |= flag::PROPERTIES;
        }
        if draft == 15 {
            w.u8(flags as u8);
        } else {
            w.varint(flags)?;
        }
        self.write_id_fields(w, flags, location)?;
        if flags & flag::SUBGROUP_MASK == flag::SUBGROUP_PRESENT && flags & flag::DATAGRAM == 0 {
            w.varint(object.subgroup_id.unwrap_or(0))?;
        }
        if flags & flag::PRIORITY != 0 {
            w.u8(object.publisher_priority);
        }
        if flags & flag::PROPERTIES != 0 {
            w.with_varint_length(|w| object.properties.encode_uncounted(w, self.version))?;
        }
        w.varint(object.payload_len as u64)?;
        if draft == 15 && object.payload_len == 0 {
            w.varint(ObjectStatus::Normal.encode())?;
        }
        self.prior = Some(Prior { location, subgroup_id: Some(object.subgroup_id), priority: Some(object.publisher_priority) });
        Ok(())
    }

    /// The Group ID and Object ID fields `flags` name: Object ID absolute after a Group ID, else a delta from draft 18.
    fn write_id_fields(&self, w: &mut Writer, flags: u64, location: Location) -> Result<()> {
        if flags & flag::GROUP != 0 {
            w.varint(self.group_field(location.group)?)?;
        }
        if flags & flag::OBJECT != 0 {
            let field = match self.prior {
                Some(prior) if self.deltas() && flags & flag::GROUP == 0 => location.object.checked_sub(prior.location.object)
                    .ok_or(Error::Unencodable("fetch stream objects out of order within a group"))?,
                _ => location.object,
            };
            w.varint(field)?;
        }
        Ok(())
    }

    /// The next entry, `Error::Incomplete` until all of it arrived, leaving the codec as it was then.
    pub fn decode<M: Mode>(&mut self, r: &mut Reader<'_, M>) -> Result<FetchEntry> {
        self.supported()?;
        let draft = self.version.draft();
        let flags = if draft == 15 { u64::from(r.u8()?) } else { r.varint()? };
        let violation = |what: &str| Error::ProtocolViolation(format!("fetch stream {what}"));
        let end_of_range = match flags {
            flag::END_OF_NON_EXISTENT_RANGE | flag::END_OF_UNKNOWN_RANGE if draft >= 16 => Some(flags == flag::END_OF_UNKNOWN_RANGE),
            0x40.. if draft == 15 => return Err(violation("Serialization Flags beyond draft 15")),
            0x80.. => return Err(violation(&format!("Serialization Flags 0x{flags:x}"))),
            _ => None,
        };
        let prior = self.prior;
        // the first entry names its Group ID and Object ID
        if prior.is_none() && flags & (flag::GROUP | flag::OBJECT) != flag::GROUP | flag::OBJECT {
            return Err(violation("starting with a reference to a prior object"));
        }
        let group = if flags & flag::GROUP != 0 { self.group_from_field(r.varint()?)? } else { prior.map_or(0, |p| p.location.group) };
        let object = match prior {
            _ if flags & flag::OBJECT == 0 => prior.and_then(|p| p.location.object.checked_add(1)).ok_or_else(|| violation("Object ID beyond 2^64 - 1"))?,
            Some(p) if self.deltas() && flags & flag::GROUP == 0 => p.location.object.checked_add(r.varint()?).ok_or_else(|| violation("Object ID beyond 2^64 - 1"))?,
            _ => r.varint()?,
        };
        let location = Location { group, object };
        if let Some(unknown) = end_of_range {
            self.prior = Some(Prior { location, ..prior.unwrap_or(Prior { location, subgroup_id: None, priority: None }) });
            return Ok(FetchEntry::EndOfRange { location, unknown });
        }
        let prior_subgroup = prior.and_then(|p| p.subgroup_id);
        let subgroup_id = if flags & flag::DATAGRAM != 0 {
            None
        } else {
            Some(match flags & flag::SUBGROUP_MASK {
                flag::SUBGROUP_ZERO => 0,
                flag::SUBGROUP_PRIOR => prior_subgroup.flatten().ok_or_else(|| violation("referencing a prior Subgroup ID it lacks"))?,
                flag::SUBGROUP_NEXT => prior_subgroup.flatten().and_then(|s| s.checked_add(1)).ok_or_else(|| violation("referencing a prior Subgroup ID it lacks"))?,
                _ => r.varint()?,
            })
        };
        let publisher_priority = match flags & flag::PRIORITY {
            0 => prior.and_then(|p| p.priority).ok_or_else(|| violation("referencing a prior priority it lacks"))?,
            _ => r.u8()?,
        };
        let properties = match flags & flag::PROPERTIES {
            0 => KeyValuePairs::new(),
            _ => {
                let len = r.varint()?;
                KeyValuePairs::decode_to_end(&mut r.frame(len)?, self.version)?
            }
        };
        let payload_len = usize::try_from(r.varint()?).map_err(|_| violation("payload length beyond the address space"))?;
        if draft == 15 && payload_len == 0 {
            // TODO carry the Object Status, until then such an object reads as empty
            ObjectStatus::decode(r.varint()?, self.version)?;
        }
        self.prior = Some(Prior { location, subgroup_id: Some(subgroup_id), priority: Some(publisher_priority) });
        Ok(FetchEntry::Object(FetchObject { location, subgroup_id, publisher_priority, properties, payload_len }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KeyValuePair, VarintKey, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_19};
    use rstest::rstest;

    fn object(group: u64, object: u64, subgroup_id: u64) -> FetchEntry {
        FetchEntry::Object(FetchObject { location: Location { group, object }, subgroup_id: Some(subgroup_id), publisher_priority: 7, properties: KeyValuePairs::new(), payload_len: 3 })
    }

    fn round_trip(version: Version, order: GroupOrder, entries: &[FetchEntry]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut encoder = FetchStream::new(version, order);
        for entry in entries {
            encoder.encode(&mut version.writer(&mut bytes), entry).unwrap();
        }
        let mut decoder = FetchStream::new(version, order);
        let mut r = version.reader(&bytes);
        for entry in entries {
            assert_eq!(&decoder.decode(&mut r).unwrap(), entry, "{version}");
        }
        assert!(r.is_empty());
        bytes
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_15, GroupOrder::Ascending)]
    #[case(MOQ_VERSION_DRAFT_16, GroupOrder::Descending)]
    #[case(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending)]
    #[case(MOQ_VERSION_DRAFT_19, GroupOrder::Descending)]
    fn objects_round_trip(#[case] version: Version, #[case] order: GroupOrder) {
        let (first, second) = if order == GroupOrder::Ascending { (2, 5) } else { (5, 2) };
        let with_properties = FetchEntry::Object(FetchObject {
            properties: [KeyValuePair::Varint(VarintKey::new(0x0A), 1)].into_iter().collect(),
            ..match object(second, 4, 9) { FetchEntry::Object(o) => o, _ => unreachable!() }
        });
        round_trip(version, order, &[object(first, 0, 0), object(first, 1, 0), object(first, 3, 1), object(second, 0, 2), with_properties]);
    }

    /// Draft 19 §11.4.4: the first object names its Group ID and Object ID, the next ones elide what follows.
    #[test]
    fn draft_19_elides_what_follows() {
        let bytes = round_trip(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending, &[object(2, 0, 0), object(2, 1, 0)]);
        // flags GROUP|OBJECT|PRIORITY (subgroup 0), group 2, object 0, priority 7, payload 3 | flags 0 (subgroup 0), payload 3
        assert_eq!(bytes, [0x1C, 2, 0, 7, 3, 0x00, 3]);
    }

    #[test]
    fn end_of_range_round_trips() {
        let range = FetchEntry::EndOfRange { location: Location { group: 4, object: 9 }, unknown: true };
        round_trip(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending, &[object(2, 0, 0), range, object(5, 0, 0)]);
    }

    #[test]
    fn unencodable_entry_leaves_nothing_behind() {
        let mut bytes = Vec::new();
        let mut encoder = FetchStream::new(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending);
        encoder.encode(&mut MOQ_VERSION_DRAFT_19.writer(&mut bytes), &object(2, 1, 0)).unwrap();
        let len = bytes.len();
        assert!(matches!(encoder.encode(&mut MOQ_VERSION_DRAFT_19.writer(&mut bytes), &object(2, 0, 0)), Err(Error::Unencodable(_))));
        assert_eq!(bytes.len(), len);
    }

    #[test]
    fn first_entry_must_name_its_location() {
        let mut decoder = FetchStream::new(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending);
        assert!(matches!(decoder.decode(&mut MOQ_VERSION_DRAFT_19.reader(&[0x01, 3])), Err(Error::ProtocolViolation(_))));
    }

    /// A partial entry leaves the codec as it was.
    #[test]
    fn partial_entry_is_incomplete() {
        let mut bytes = Vec::new();
        FetchStream::new(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending).encode(&mut MOQ_VERSION_DRAFT_19.writer(&mut bytes), &object(2, 0, 0)).unwrap();
        let mut decoder = FetchStream::new(MOQ_VERSION_DRAFT_19, GroupOrder::Ascending);
        assert!(matches!(decoder.decode(&mut MOQ_VERSION_DRAFT_19.reader(&bytes[..2])), Err(Error::Incomplete { .. })));
        assert_eq!(decoder.decode(&mut MOQ_VERSION_DRAFT_19.reader(&bytes)).unwrap(), object(2, 0, 0));
    }
}
