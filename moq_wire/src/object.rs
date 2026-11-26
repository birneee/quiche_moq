
use crate::prelude::*;
use crate::error::{Error, Result};
use crate::{Mode, Reader, Version, Writer};
use crate::key_value_pairs::KeyValuePairs;
use crate::subgroup::{SubgroupHeader, SubgroupType};

/// Object Status (draft-ietf-moq-transport-18 §11.2.1.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectStatus {
    #[default]
    Normal,
    /// No Objects from this one on in its Group.
    EndOfGroup,
    /// No Objects from this one on in the Track.
    EndOfTrack,
}

impl ObjectStatus {
    /// Undefined values are a protocol error.
    pub fn decode(value: u64, version: Version) -> Result<Self> {
        match (value, version.draft()) {
            (0x0, _) => Ok(Self::Normal),
            (0x3, _) => Ok(Self::EndOfGroup),
            // End of Track and Group before draft 11
            (0x4, _) | (0x5, 8..=10) => Ok(Self::EndOfTrack),
            // TODO Object Does Not Exist of drafts 7 to 15 and End of Subgroup of draft 7, implement when needed
            (0x1, ..=15) | (0x5, 7) => Err(Error::Unsupported(format!("Object Status {value:#x} in draft {}", version.draft()))),
            _ => Err(Error::ProtocolViolation(format!("Object Status {value:#x}"))),
        }
    }

    pub fn encode(self) -> u64 {
        match self {
            Self::Normal => 0x0,
            Self::EndOfGroup => 0x3,
            Self::EndOfTrack => 0x4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectHeader {
    id: u64,
    subgroup_ty: SubgroupType,
    properties: KeyValuePairs,
    payload_len: usize,
    /// on the wire only without payload
    status: ObjectStatus,
}

impl ObjectHeader {
    /// A zero-length payload is sent with status Normal unless `with_status`.
    pub fn new(id: u64, payload_len: usize, subgroup_ty: SubgroupType, properties: KeyValuePairs) -> Self {
        Self { id, subgroup_ty, properties, payload_len, status: ObjectStatus::Normal }
    }

    /// The Object Status, on the wire only without payload (draft-ietf-moq-transport-18 §11.2.1.1).
    pub fn with_status(mut self, status: ObjectStatus) -> Self {
        self.status = status;
        self
    }

    /// The object after `previous` on its subgroup stream, `None` for the first.
    pub fn decode<M: Mode>(b: &mut Reader<'_, M>, version: Version, subgroup: &SubgroupHeader, previous: Option<u64>) -> Result<Self> {
        let subgroup_ty = subgroup.ty();
        let id = id_from_field(b.varint()?, previous, version)?;
        let properties = match PropertiesField::of(subgroup_ty, version) {
            PropertiesField::Absent => KeyValuePairs::new(),
            PropertiesField::Counted => KeyValuePairs::decode_counted(b, version)?,
            PropertiesField::Length => {
                let len = b.varint()?;
                KeyValuePairs::decode_to_end(&mut b.frame(len)?, version)?
            }
        };
        let payload_len = usize::try_from(b.varint()?).map_err(|_| Error::ProtocolViolation("payload length beyond the address space".into()))?;
        let status = if payload_len == 0 { ObjectStatus::decode(b.varint()?, version)? } else { ObjectStatus::Normal };
        Ok(Self { id, subgroup_ty, properties, payload_len, status })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn payload_len(&self) -> usize {
        self.payload_len
    }

    pub fn properties_len(&self) -> usize {
        self.properties.len()
    }

    pub fn properties(&self) -> &KeyValuePairs {
        &self.properties
    }

    /// The Object Status of an object without payload.
    pub fn status(&self) -> Option<ObjectStatus> {
        (self.payload_len == 0).then_some(self.status)
    }

    /// The properties as qlog's `[* MOQTExtensionHeader]` (draft-pardue-moq-qlog-moq-events).
    #[cfg(feature = "qlog")]
    pub fn properties_to_qlog(&self) -> alloc::vec::Vec<serde_json::Value> {
        use crate::KeyValuePair;
        self.properties.iter().map(|pair| match pair {
            KeyValuePair::Varint(key, v) => serde_json::json!({
                "header_type": key.get(),
                "header_value": v,
            }),
            KeyValuePair::Bytes(key, b) => {
                let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
                serde_json::json!({
                    "header_type": key.get(),
                    "header_length": b.len() as u64,
                    "payload": { "data": hex },
                })
            }
        }).collect()
    }
}

impl ObjectHeader {
    /// The object after `previous` on its subgroup stream, `None` for the first.
    pub fn encode(&self, b: &mut Writer, version: Version, previous: Option<u64>) -> Result<()> {
        b.varint(field_from_id(self.id, previous, version)?)?;
        match PropertiesField::of(self.subgroup_ty, version) {
            PropertiesField::Absent if !self.properties.is_empty() => {
                return Err(Error::Unsupported("object properties before draft 8 or on a subgroup type without them".into()));
            }
            PropertiesField::Absent => {}
            PropertiesField::Counted => self.properties.encode_counted(b, version)?,
            PropertiesField::Length => b.with_varint_length(|b| self.properties.encode_uncounted(b, version))?,
        }
        b.varint(self.payload_len as u64)?;
        if self.payload_len == 0 {
            b.varint(self.status.encode())?;
        }
        Ok(())
    }
}

/// How an object carries its properties.
enum PropertiesField {
    Absent,
    /// draft 8: Extension Count
    Counted,
    /// draft 9 on: Extension Headers Length
    Length,
}

impl PropertiesField {
    fn of(subgroup_ty: SubgroupType, version: Version) -> Self {
        match version.draft() {
            ..=7 => Self::Absent,
            8 => Self::Counted,
            9..=10 => Self::Length,
            _ if subgroup_ty.extensions => Self::Length,
            _ => Self::Absent,
        }
    }
}

/// From draft 14 an object gives its ID as Object ID Delta, the previous object's ID plus 1 plus it
/// (draft-ietf-moq-transport-19 §11.4.2), the first object of a stream its ID.
fn id_from_field(field: u64, previous: Option<u64>, version: Version) -> Result<u64> {
    match previous {
        Some(previous) if version.draft() >= 14 => previous.checked_add(field).and_then(|id| id.checked_add(1))
            .ok_or_else(|| Error::ProtocolViolation("Object ID beyond 2^64 - 1".into())),
        _ => Ok(field),
    }
}

/// Inverse of [`id_from_field`].
fn field_from_id(id: u64, previous: Option<u64>, version: Version) -> Result<u64> {
    match previous {
        Some(previous) if version.draft() >= 14 => id.checked_sub(previous).and_then(|delta| delta.checked_sub(1))
            .ok_or(Error::Unencodable("an Object ID not above the previous one on its stream")),
        _ => Ok(id),
    }
}
