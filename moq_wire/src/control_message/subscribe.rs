use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error;
use crate::vi64;
use crate::{Namespace, NamespaceTrackname, Parameter, Parameters, RequestId, TrackAlias, Version, ABSOLUTE_RANGE_FILTER_ID, ABSOLUTE_START_FILTER_ID, DELIVERY_TIMEOUT_PARAMETER_ID, FORWARD_PARAMETER_ID, GROUP_ORDER_PARAMETER_ID, LARGEST_OBJECT_FILTER_ID, MAX_FULL_TRACK_NAME_LEN, MAX_TRACK_NAMESPACE_TUPLE_LENGTH, MIN_TRACK_NAMESPACE_TUPLE_LENGTH, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_18, NEXT_GROUP_START_FILTER_ID, SUBSCRIBER_PRIORITY_PARAMETER_ID, SUBSCRIBE_MESSAGE_ID, SUBSCRIPTION_FILTER_PARAMETER_ID};
use octets::{Octets, OctetsMut};
use crate::control_message::ControlMessage;
use crate::location::Location;
use crate::tuple::Tuple;

#[derive(Debug, Eq, PartialEq)]
pub struct SubscribeMessage {
    pub request_id: RequestId,
    /// `Some` from draft 07 to draft 11
    pub track_alias: Option<TrackAlias>,
    pub namespace_trackname: NamespaceTrackname,
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
        self.track_namespace().iter().map(|n| n.len()).sum::<usize>() + self.track_name().len()
    }

    pub fn validate(&self) -> crate::error::Result<()> {
        if !(MIN_TRACK_NAMESPACE_TUPLE_LENGTH..=MAX_TRACK_NAMESPACE_TUPLE_LENGTH).contains(&self.track_namespace().len()) {
            return Err(Error::ProtocolViolation(format!("Namespace tuple MUST be between {} and {}", MIN_TRACK_NAMESPACE_TUPLE_LENGTH, MAX_TRACK_NAMESPACE_TUPLE_LENGTH)))
        }
        if self.full_track_name_len() > MAX_FULL_TRACK_NAME_LEN {
            return Err(Error::ProtocolViolation(format!("Full track name MUST not exceed {} bytes", MAX_FULL_TRACK_NAME_LEN)))
        }

        Ok(())
    }

    pub fn track_namespace(&self) -> &Namespace {
        self.namespace_trackname.namespace()
    }

    pub fn track_name(&self) -> &[u8] {
        self.namespace_trackname.trackname()
    }

    /// Returns the DELIVERY_TIMEOUT in milliseconds, if present in the parameters.
    pub fn delivery_timeout(&self) -> Option<u64> {
        self.parameters.get_varint(DELIVERY_TIMEOUT_PARAMETER_ID)
    }
}

impl ControlMessage for SubscribeMessage {
    const MESSAGE_IDS: &'static [u64] = &[SUBSCRIBE_MESSAGE_ID];

    fn qlog_type_name(&self) -> &'static str { "subscribe" }

    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        self.validate()?;
        vi64::write_varint(b, self.request_id, version)?;
        match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => { vi64::write_varint(b, self.track_alias.unwrap(), version)?; },
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_18 => {},
            _ => unimplemented!()
        };
        vi64::write_varint(b, self.track_namespace().len() as u64, version)?;
        for namespace in self.track_namespace() {
            vi64::write_varint(b, namespace.len() as u64, version)?;
            b.put_bytes(namespace)?;
        }
        vi64::write_varint(b, self.track_name().len() as u64, version)?;
        b.put_bytes(self.track_name())?;
        // draft 15+ moved subscriber_priority/group_order/forward/filter out of
        // the fixed fields and into Parameters (draft-16 §9.2.2.3-.5, .8).
        if version < MOQ_VERSION_DRAFT_15 {
            b.put_u8(self.subscriber_priority)?;
            b.put_u8(self.group_order)?;
            match version {
                MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {},
                MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_14 => { b.put_u8(self.forward.unwrap())?; },
                _ => unreachable!()
            }
            self.filter_type.to_bytes(b, version)?;
            if self.filter_type.has_start_location() {
                self.start_location.as_ref().unwrap().to_bytes(b, version)?;
            }
            if self.filter_type.has_end_group() {
                vi64::write_varint(b, self.end_group.unwrap(), version)?;
            }
            self.parameters.to_bytes(b, version)?;
        } else {
            let mut params = vec![
                Parameter::new_varint(SUBSCRIBER_PRIORITY_PARAMETER_ID, self.subscriber_priority as u64),
                Parameter::new_varint(GROUP_ORDER_PARAMETER_ID, self.group_order as u64),
            ];
            if let Some(forward) = self.forward {
                params.push(Parameter::new_varint(FORWARD_PARAMETER_ID, forward as u64));
            }
            let mut filter_buf = [0u8; 64];
            let filter_len = {
                let mut fb = OctetsMut::with_slice(&mut filter_buf);
                self.filter_type.to_bytes(&mut fb, version)?;
                if self.filter_type.has_start_location() {
                    self.start_location.as_ref().unwrap().to_bytes(&mut fb, version)?;
                }
                if self.filter_type.has_end_group() {
                    vi64::write_varint(&mut fb, self.end_group.unwrap(), version)?;
                }
                fb.off()
            };
            params.push(Parameter::new_bytes(SUBSCRIPTION_FILTER_PARAMETER_ID, filter_buf[..filter_len].to_vec()));
            params.extend(self.parameters.0.iter().cloned());
            Parameters(params).to_bytes(b, version)?;
        }
        Ok(())
    }

    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let request_id = vi64::read_varint(b, version)?;
        let track_alias = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => Some(vi64::read_varint(b, version)?),
            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_18 => None,
            _ => unimplemented!()
        };
        let track_namespace = Tuple::from_bytes(b, version)?.0;
        let track_name_len = vi64::read_varint(b, version)?;
        let track_name = b.get_bytes(track_name_len as usize)?.to_vec();
        if version < MOQ_VERSION_DRAFT_15 {
            let subscriber_priority = b.get_u8()?;
            let group_order = b.get_u8()?;
            let forward = match version {
                MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => None,
                MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_14 => Some(b.get_u8()?),
                _ => unreachable!()
            };
            let filter_type = FilterType::from_bytes(b, version)?;
            let start_location = if filter_type.has_start_location() {
                Some(Location::from_bytes(b, version)?)
            } else {
                None
            };
            let end_group = if filter_type.has_end_group() {
                Some(vi64::read_varint(b, version)?)
            } else {
                None
            };
            let parameters = Parameters::from_bytes(b, version)?;
            Ok(Self {
                request_id,
                track_alias,
                namespace_trackname: NamespaceTrackname::new(track_namespace, track_name),
                subscriber_priority,
                group_order,
                forward,
                filter_type,
                start_location,
                end_group,
                parameters,
            })
        } else {
            let parameters = Parameters::from_bytes(b, version)?;
            let subscriber_priority = parameters.get_varint(SUBSCRIBER_PRIORITY_PARAMETER_ID).map(|v| v as u8).unwrap_or(128);
            let group_order = parameters.get_varint(GROUP_ORDER_PARAMETER_ID).map(|v| v as u8).unwrap_or(2);
            let forward = parameters.get_varint(FORWARD_PARAMETER_ID).map(|v| v as u8);
            let (filter_type, start_location, end_group) = if let Some(filter_bytes) = parameters.get_bytes(SUBSCRIPTION_FILTER_PARAMETER_ID) {
                let mut fb = Octets::with_slice(filter_bytes);
                let filter_type = FilterType::from_bytes(&mut fb, version)?;
                let start_location = if filter_type.has_start_location() {
                    Some(Location::from_bytes(&mut fb, version)?)
                } else {
                    None
                };
                let end_group = if filter_type.has_end_group() {
                    Some(vi64::read_varint(&mut fb, version)?)
                } else {
                    None
                };
                (filter_type, start_location, end_group)
            } else {
                // "If omitted from SUBSCRIBE ... the subscription is unfiltered" (§9.2.2.5)
                // — modeled as the spec's own stated equivalent, AbsoluteStart{0, 0}
                // (§5.1.2), since `FilterType` has no separate "unfiltered" variant.
                (FilterType::AbsoluteStart, Some(Location { group: 0, object: 0 }), None)
            };
            let extra: Vec<Parameter> = parameters.0.into_iter()
                .filter(|p| !matches!(p.ty, SUBSCRIBER_PRIORITY_PARAMETER_ID | GROUP_ORDER_PARAMETER_ID | FORWARD_PARAMETER_ID | SUBSCRIPTION_FILTER_PARAMETER_ID))
                .collect();
            Ok(Self {
                request_id,
                track_alias,
                namespace_trackname: NamespaceTrackname::new(track_namespace, track_name),
                subscriber_priority,
                group_order,
                forward,
                filter_type,
                start_location,
                end_group,
                parameters: Parameters(extra),
            })
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
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
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
        let id = match self {
            FilterType::LargestObject => LARGEST_OBJECT_FILTER_ID,
            FilterType::NextGroupStart => NEXT_GROUP_START_FILTER_ID,
            FilterType::AbsoluteStart => ABSOLUTE_START_FILTER_ID,
            FilterType::AbsoluteRange => ABSOLUTE_RANGE_FILTER_ID,
        };
        vi64::write_varint(b, id, version)?;
        Ok(())
    }
}

impl FromBytes for FilterType {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
        let ty = vi64::read_varint(b, version)?;
        Ok(match ty {
            NEXT_GROUP_START_FILTER_ID => Self::NextGroupStart,
            LARGEST_OBJECT_FILTER_ID => Self::LargestObject,
            ABSOLUTE_START_FILTER_ID => Self::AbsoluteStart,
            ABSOLUTE_RANGE_FILTER_ID => Self::AbsoluteRange,
            _ => unimplemented!()
        })
    }
}
