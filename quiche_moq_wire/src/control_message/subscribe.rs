use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::{Parameters, RequestId, TrackAlias, Version, ABSOLUTE_RANGE_FILTER_ID, ABSOLUTE_START_FILTER_ID, LARGEST_OBJECT_FILTER_ID, MAX_FULL_TRACK_NAME_LEN, MAX_TRACK_NAMESPACE_TUPLE_LENGTH, MIN_TRACK_NAMESPACE_TUPLE_LENGTH, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, NEXT_GROUP_START_FILTER_ID, SUBSCRIBE_CONTROL_MESSAGE_ID};
use octets::{Octets, OctetsMut};
use crate::control_message::header::ControlMessageHeader;
use crate::location::Location;
use crate::tuple::Tuple;

#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeMessage {
    pub request_id: RequestId,
    /// `Some` from draft 07 to draft 11
    pub track_alias: Option<TrackAlias>,
    pub track_namespace: Vec<Vec<u8>>,
    pub track_name: Vec<u8>,
    pub subscriber_priority: u8,
    pub group_order: u8,
    /// `Some` from draft 11 to draft 13
    pub forward: Option<u8>,
    pub filter_type: FilterType,
    pub start_location: Option<Location>,
    pub end_group: Option<u64>,
    pub parameters: Parameters,
}

impl SubscribeMessage {

    /// length of the full track name including track namespaces and track name
    pub fn full_track_name_len(&self) -> usize {
        self.track_namespace.iter().map(|n| n.len()).sum::<usize>() + self.track_name.len()
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if !(MIN_TRACK_NAMESPACE_TUPLE_LENGTH..=MAX_TRACK_NAMESPACE_TUPLE_LENGTH).contains(&self.track_namespace.len()) {
            return Err(Error::ProtocolViolation(format!("Namespace tuple MUST be between {} and {}", MIN_TRACK_NAMESPACE_TUPLE_LENGTH, MAX_TRACK_NAMESPACE_TUPLE_LENGTH)))
        }
        if self.full_track_name_len() > MAX_FULL_TRACK_NAME_LEN {
            return Err(Error::ProtocolViolation(format!("Full track name MUST not exceed {} bytes", MAX_FULL_TRACK_NAME_LEN)))
        }

        Ok(())
    }
}

impl FromBytes for SubscribeMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        assert_eq!(header.ty(), SUBSCRIBE_CONTROL_MESSAGE_ID);
        let request_id = b.get_varint()?;
        let track_alias = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => Some(b.get_varint()?),
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => None,
            _ => unimplemented!()
        };
        let track_namespace = Tuple::from_bytes(b, version)?.0;
        let track_name_len = b.get_varint()?;
        let track_name = b.get_bytes(track_name_len as usize)?.to_vec();
        let subscriber_priority = b.get_u8()?;
        let group_order = b.get_u8()?;
        let forward = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => None,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => Some(b.get_u8()?),
            _ => unimplemented!()
        };
        let filter_type = FilterType::from_bytes(b, version)?;
        let start_location = if filter_type.has_start_location() {
            Some(Location::from_bytes(b, version)?)
        } else {
            None
        };
        let end_group = if filter_type.has_end_group() {
            Some(b.get_varint()?)
        } else {
            None
        };
        let parameters = Parameters::from_bytes(b, version)?;
        Ok(Self {
            request_id,
            track_alias,
            track_namespace,
            track_name,
            subscriber_priority,
            group_order,
            forward,
            filter_type,
            start_location,
            end_group,
            parameters,
        })
    }
}

impl ToBytes for SubscribeMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        self.validate()?;
        b.put_varint(SUBSCRIBE_CONTROL_MESSAGE_ID)?;
        let len_off = b.off();
        b.skip(2)?;
        b.put_varint(self.request_id)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => { b.put_varint(self.track_alias.unwrap())?; },
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {},
            _ => unimplemented!()
        };
        b.put_varint(self.track_namespace.len() as u64)?;
        for namespace in &self.track_namespace {
            b.put_varint(namespace.len() as u64)?;
            b.put_bytes(namespace)?;
        }
        b.put_varint(self.track_name.len() as u64)?;
        b.put_bytes(&self.track_name)?;
        b.put_u8(self.subscriber_priority)?;
        b.put_u8(self.group_order)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {},
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => { b.put_u8(self.forward.unwrap())?; },
            _ => unimplemented!()
        }
        self.filter_type.to_bytes(b, version)?;
        if self.filter_type.has_start_location() {
            self.start_location.as_ref().unwrap().to_bytes(b, version)?;
        }
        if self.filter_type.has_end_group() {
            b.put_varint(self.end_group.unwrap())?;
        }
        self.parameters.to_bytes(b, version)?;
        crate::control_message::set_control_message_length(b, len_off, version)?;
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum FilterType {
    LargestObject,
    NextGroupStart,
    AbsoluteStart,
    AbsoluteRange,
}

impl FilterType {
    pub fn has_start_location(&self) -> bool {
        match self {
            FilterType::LargestObject => false,
            FilterType::NextGroupStart => false,
            FilterType::AbsoluteStart => true,
            FilterType::AbsoluteRange => true,
        }
    }

    pub fn has_end_group(&self) -> bool {
        match self {
            FilterType::LargestObject => false,
            FilterType::NextGroupStart => false,
            FilterType::AbsoluteStart => false,
            FilterType::AbsoluteRange => true,
        }
    }
}

impl ToBytes for FilterType {
    fn to_bytes(&self, b: &mut OctetsMut, _version: Version) -> crate::error::Result<()> {
        match self {
            FilterType::LargestObject => b.put_varint(LARGEST_OBJECT_FILTER_ID)?,
            FilterType::NextGroupStart => b.put_varint(NEXT_GROUP_START_FILTER_ID)?,
            _ => unimplemented!()
        };
        Ok(())
    }
}

impl FromBytes for FilterType {
    fn from_bytes(b: &mut Octets, _version: Version) -> crate::error::Result<Self> {
        let ty = b.get_varint()?;
        Ok(match ty {
            NEXT_GROUP_START_FILTER_ID => Self::NextGroupStart,
            LARGEST_OBJECT_FILTER_ID => Self::LargestObject,
            ABSOLUTE_START_FILTER_ID => Self::AbsoluteStart,
            ABSOLUTE_RANGE_FILTER_ID => Self::AbsoluteRange,
            _ => unimplemented!()
        })
    }
}
