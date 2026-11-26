//! Message Parameters (draft-ietf-moq-transport-19 §10.2): typed values, each draft's encoding.
//! - draft 17 on: ascending Type Delta and a value of the parameter's type, unknown types are an error
//! - draft 11 to 16: key-value pairs, a varint for an even type, bytes for an odd one, unknown types skipped
//! - draft 7 to 10: type, length and bytes, numbers as a varint inside, unknown types skipped

use crate::prelude::*;
use crate::bytes::{from_slice, to_vec, FromBytes, ToBytes};
use crate::error::{Error, Result};
use crate::{property, GroupOrder, KeyValuePair, KeyValuePairs, Location, Mode, NamespaceTuple, Reader, VarIntForm, VarintKey, Version, Writer};

/// A Message Parameter of draft 19, or of an older draft where draft 19 has a Track Property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Param {
    AuthorizationToken,
    ObjectDeliveryTimeout,
    SubgroupDeliveryTimeout,
    RendezvousTimeout,
    Expires,
    FillTimeout,
    LargestObject,
    Forward,
    SubscriberPriority,
    LocationFilter,
    GroupOrder,
    NewGroupRequest,
    SubgroupFilter,
    ObjectIdFilter,
    PriorityFilter,
    ObjectPropertyFilter,
    TrackPropertyFilter,
    TrackNamespacePrefix,
    /// a parameter until draft 15, a Track Property from draft 16
    MaxCacheDuration,
    /// draft 15, a Track Property from draft 16
    PublisherPriority,
    /// draft 15, a Track Property from draft 16
    DynamicGroups,
}

const ALL: [Param; 21] = [
    Param::AuthorizationToken, Param::ObjectDeliveryTimeout, Param::SubgroupDeliveryTimeout, Param::RendezvousTimeout,
    Param::Expires, Param::FillTimeout, Param::LargestObject, Param::Forward, Param::SubscriberPriority, Param::LocationFilter,
    Param::GroupOrder, Param::NewGroupRequest, Param::SubgroupFilter, Param::ObjectIdFilter, Param::PriorityFilter,
    Param::ObjectPropertyFilter, Param::TrackPropertyFilter, Param::TrackNamespacePrefix, Param::MaxCacheDuration, Param::PublisherPriority,
    Param::DynamicGroups,
];

/// How a value is encoded from draft 17.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    U8,
    Varint,
    Location,
    Namespace,
    Bytes,
}

impl Param {
    const fn kind(self) -> Kind {
        match self {
            Self::Forward | Self::SubscriberPriority | Self::GroupOrder => Kind::U8,
            Self::LargestObject => Kind::Location,
            Self::TrackNamespacePrefix => Kind::Namespace,
            Self::AuthorizationToken | Self::LocationFilter | Self::SubgroupFilter | Self::ObjectIdFilter
            | Self::PriorityFilter | Self::ObjectPropertyFilter | Self::TrackPropertyFilter => Kind::Bytes,
            Self::ObjectDeliveryTimeout | Self::SubgroupDeliveryTimeout | Self::RendezvousTimeout | Self::Expires | Self::FillTimeout
            | Self::NewGroupRequest | Self::MaxCacheDuration | Self::PublisherPriority | Self::DynamicGroups => Kind::Varint,
        }
    }

    /// Its type in `version`, `None` if that draft doesn't have it as a parameter.
    pub(crate) fn ty(self, version: Version) -> Option<u64> {
        let draft = version.draft();
        let since = |first: u8, ty: u64| if draft >= first { Some(ty) } else { None };
        match self {
            Self::AuthorizationToken => Some(match draft { ..=10 => 0x02, 11 => 0x01, _ => 0x03 }),
            Self::ObjectDeliveryTimeout => Some(if draft <= 10 { 0x03 } else { 0x02 }),
            Self::MaxCacheDuration => if draft <= 15 { Some(0x04) } else { None },
            Self::PublisherPriority => if draft == 15 { Some(0x0E) } else { None },
            Self::DynamicGroups => if draft == 15 { Some(0x30) } else { None },
            // fields of the messages before draft 15
            Self::Expires => since(15, 0x08),
            Self::LargestObject => since(15, 0x09),
            Self::Forward => since(15, 0x10),
            Self::SubscriberPriority => since(15, 0x20),
            Self::LocationFilter => since(15, 0x21),
            Self::GroupOrder => since(15, 0x22),
            Self::NewGroupRequest => since(15, 0x32),
            Self::RendezvousTimeout => since(17, 0x04),
            Self::SubgroupDeliveryTimeout => since(18, 0x06),
            Self::SubgroupFilter => since(19, 0x25),
            Self::ObjectIdFilter => since(19, 0x26),
            Self::PriorityFilter => since(19, 0x27),
            Self::ObjectPropertyFilter => since(19, 0x28),
            Self::TrackPropertyFilter => since(19, 0x29),
            Self::FillTimeout => since(19, 0x0A),
            Self::TrackNamespacePrefix => since(19, 0x34),
        }
    }

    fn of_type(ty: u64, version: Version) -> Option<Self> {
        ALL.into_iter().find(|param| param.ty(version) == Some(ty))
    }

    /// Several of it in one message (draft-ietf-moq-transport-19 §10.2.2).
    /// Authorization tokens, and range filters with different SetIDs (draft-ietf-moq-transport-19 §5.1.3).
    fn repeatable(self) -> bool {
        matches!(self, Self::AuthorizationToken | Self::SubgroupFilter | Self::ObjectIdFilter | Self::PriorityFilter | Self::ObjectPropertyFilter | Self::TrackPropertyFilter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    U8(u8),
    Varint(u64),
    Location(Location),
    Namespace(NamespaceTuple),
    Bytes(Vec<u8>),
}

/// The parameters of one message.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct MessageParams(Vec<(Param, Value)>);

impl MessageParams {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn put_u8(&mut self, param: Param, v: u8) {
        self.0.push((param, Value::U8(v)));
    }

    pub(crate) fn put_varint(&mut self, param: Param, v: u64) {
        self.0.push((param, Value::Varint(v)));
    }

    pub(crate) fn put_location(&mut self, param: Param, v: Location) {
        self.0.push((param, Value::Location(v)));
    }

    pub(crate) fn put_bytes(&mut self, param: Param, v: Vec<u8>) {
        self.0.push((param, Value::Bytes(v)));
    }

    fn take(&mut self, param: Param) -> Option<Value> {
        let at = self.0.iter().position(|(p, _)| *p == param)?;
        Some(self.0.remove(at).1)
    }

    pub(crate) fn take_u8(&mut self, param: Param) -> Option<u8> {
        match self.take(param)? {
            Value::U8(v) => Some(v),
            _ => None,
        }
    }

    /// The FORWARD parameter's Forward State, `None` if omitted.
    pub(crate) fn take_forward(&mut self) -> Result<Option<bool>> {
        self.take_u8(Param::Forward).map(|forward| crate::control_message::subscribe::forward_state(forward.into())).transpose()
    }

    pub(crate) fn take_varint(&mut self, param: Param) -> Option<u64> {
        match self.take(param)? {
            Value::Varint(v) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn take_location(&mut self, param: Param) -> Option<Location> {
        match self.take(param)? {
            Value::Location(v) => Some(v),
            _ => None,
        }
    }

    pub(crate) fn take_bytes(&mut self, param: Param) -> Option<Vec<u8>> {
        match self.take(param)? {
            Value::Bytes(v) => Some(v),
            _ => None,
        }
    }

    /// Every value of `param`, e.g. the authorization tokens.
    pub(crate) fn take_all_bytes(&mut self, param: Param) -> Vec<Vec<u8>> {
        core::iter::from_fn(|| self.take_bytes(param)).collect()
    }

    /// Drops every value of `params`, those a message takes but doesn't act on.
    pub(crate) fn ignore(&mut self, params: &[Param]) {
        self.0.retain(|(param, _)| !params.contains(param));
    }

    /// Ends decoding: from draft 17 a parameter the message doesn't take is an error.
    pub(crate) fn finish(self, version: Version) -> Result<()> {
        match self.0.first() {
            Some((param, _)) if version.draft() >= 17 => Err(Error::ProtocolViolation(format!("{param:?} in a message without it"))),
            _ => Ok(()),
        }
    }

    fn typed(&self, version: Version) -> Result<Vec<(u64, &Value)>> {
        let mut typed = self.0.iter()
            .map(|(param, value)| param.ty(version).map(|ty| (ty, value)).ok_or_else(|| Error::Unsupported(format!("{param:?} in draft {}", version.draft()))))
            .collect::<Result<Vec<_>>>()?;
        typed.sort_by_key(|(ty, _)| *ty);
        Ok(typed)
    }
}

impl ToBytes for MessageParams {
    fn to_bytes(&self, w: &mut Writer, version: Version) -> Result<()> {
        let typed = self.typed(version)?;
        match version.draft() {
            ..=10 => {
                w.varint(typed.len() as u64)?;
                for (ty, value) in typed {
                    w.varint(ty)?;
                    match value {
                        Value::U8(v) => w.with_varint_length(|w| w.varint((*v).into()))?,
                        Value::Varint(v) => w.with_varint_length(|w| w.varint(*v))?,
                        Value::Location(v) => w.length_prefixed_bytes(&to_vec(v, version)?)?,
                        Value::Namespace(v) => w.length_prefixed_bytes(&to_vec(v, version)?)?,
                        Value::Bytes(v) => w.length_prefixed_bytes(v)?,
                    }
                }
                Ok(())
            }
            11..=16 => {
                let pairs = typed.into_iter().map(|(ty, value)| {
                    let pair = match value {
                        Value::U8(v) => KeyValuePair::new_varint(ty, (*v).into()),
                        Value::Varint(v) => KeyValuePair::new_varint(ty, *v),
                        Value::Location(v) => KeyValuePair::new_bytes(ty, to_vec(v, version)?),
                        Value::Namespace(v) => KeyValuePair::new_bytes(ty, to_vec(v, version)?),
                        Value::Bytes(v) => KeyValuePair::new_bytes(ty, v.clone()),
                    };
                    pair.ok_or(Error::Unencodable("a parameter whose type parity doesn't fit its value"))
                }).collect::<Result<KeyValuePairs>>()?;
                pairs.encode_counted(w, version)
            }
            _ => {
                w.varint(typed.len() as u64)?;
                let mut previous = 0;
                for (ty, value) in typed {
                    w.varint(ty - previous)?;
                    previous = ty;
                    match value {
                        Value::U8(v) => w.u8(*v),
                        Value::Varint(v) => w.varint(*v)?,
                        Value::Location(v) => v.to_bytes(w, version)?,
                        Value::Namespace(v) => v.to_bytes(w, version)?,
                        Value::Bytes(v) => w.length_prefixed_bytes(v)?,
                    }
                }
                Ok(())
            }
        }
    }
}

impl FromBytes for MessageParams {
    fn from_bytes<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> Result<Self> {
        let mut params = Self::new();
        match version.draft() {
            ..=10 => {
                for _ in 0..r.varint()? {
                    let ty = r.varint()?;
                    let value = r.length_prefixed_bytes()?;
                    let Some(param) = Param::of_type(ty, version) else { continue };
                    params.push(param, value_from_bytes(param.kind(), value, version)?)?;
                }
            }
            11..=16 => {
                for pair in KeyValuePairs::decode_counted(r, version)?.iter() {
                    let Some(param) = Param::of_type(pair.ty(), version) else { continue };
                    let value = match (param.kind(), pair) {
                        (Kind::U8, KeyValuePair::Varint(_, v)) => Value::U8(u8::try_from(*v).map_err(|_| Error::ProtocolViolation(format!("{param:?} {v} over 255")))?),
                        (Kind::Varint, KeyValuePair::Varint(_, v)) => Value::Varint(*v),
                        (kind, KeyValuePair::Bytes(_, bytes)) => value_from_bytes(kind, bytes, version)?,
                        (_, KeyValuePair::Varint(..)) => return Err(Error::ProtocolViolation(format!("{param:?} as a varint"))),
                    };
                    params.push(param, value)?;
                }
            }
            _ => {
                let mut previous: u64 = 0;
                for _ in 0..r.varint()? {
                    let ty = previous.checked_add(r.varint()?).ok_or_else(|| Error::ProtocolViolation("parameter type overflows".into()))?;
                    previous = ty;
                    let param = Param::of_type(ty, version).ok_or_else(|| Error::ProtocolViolation(format!("unknown parameter 0x{ty:x}")))?;
                    let value = match param.kind() {
                        Kind::U8 => Value::U8(r.u8()?),
                        Kind::Varint => Value::Varint(r.varint()?),
                        Kind::Location => Value::Location(Location::from_bytes(r, version)?),
                        Kind::Namespace => Value::Namespace(NamespaceTuple::from_bytes(r, version)?),
                        Kind::Bytes => Value::Bytes(r.length_prefixed_bytes()?.to_vec()),
                    };
                    params.push(param, value)?;
                }
            }
        }
        Ok(params)
    }
}

impl MessageParams {
    /// A received parameter, repeated only where allowed.
    fn push(&mut self, param: Param, value: Value) -> Result<()> {
        if !param.repeatable() && self.0.iter().any(|(p, _)| *p == param) {
            return Err(Error::ProtocolViolation(format!("{param:?} twice")));
        }
        self.0.push((param, value));
        Ok(())
    }
}

/// A value held in bytes: draft 7 to 10 numbers, and Locations before draft 17.
fn value_from_bytes(kind: Kind, bytes: &[u8], version: Version) -> Result<Value> {
    Ok(match kind {
        Kind::U8 | Kind::Varint => {
            let mut r = Reader::frame_of(bytes, VarIntForm::Quic);
            let v = r.varint()?;
            r.finish()?;
            match kind {
                Kind::U8 => Value::U8(u8::try_from(v).map_err(|_| Error::ProtocolViolation(format!("parameter value {v} over 255")))?),
                _ => Value::Varint(v),
            }
        }
        Kind::Location => Value::Location(from_slice(bytes, version)?),
        Kind::Namespace => Value::Namespace(from_slice(bytes, version)?),
        Kind::Bytes => Value::Bytes(bytes.to_vec()),
    })
}

/// Track Properties that drafts up to 15 send as message parameters.
const PROPERTY_PARAMS: [(VarintKey, Param); 4] = [
    (property::MAX_CACHE_DURATION, Param::MaxCacheDuration),
    (property::OBJECT_DELIVERY_TIMEOUT, Param::ObjectDeliveryTimeout),
    (property::DEFAULT_PUBLISHER_PRIORITY, Param::PublisherPriority),
    (property::DYNAMIC_GROUPS, Param::DynamicGroups),
];

/// Track Properties as the parameters of drafts up to 15, those a draft lacks are left out.
pub(crate) fn properties_as_params(properties: &KeyValuePairs, params: &mut MessageParams, version: Version) {
    for (key, param) in PROPERTY_PARAMS {
        if let Some(v) = properties.get_varint(key) && param.ty(version).is_some() {
            params.put_varint(param, v);
        }
    }
    // the publisher's order, a field before draft 15
    if version.draft() == 15 && let Some(order) = properties.get_varint(property::DEFAULT_PUBLISHER_GROUP_ORDER) {
        params.put_u8(Param::GroupOrder, order as u8);
    }
}

/// Inverse of [`properties_as_params`].
pub(crate) fn params_as_properties(params: &mut MessageParams, properties: &mut KeyValuePairs, version: Version) -> Result<()> {
    for (key, param) in PROPERTY_PARAMS {
        if let Some(v) = params.take_varint(param) {
            properties.insert(KeyValuePair::Varint(key, v));
        }
    }
    if version.draft() == 15 && let Some(order) = params.take_u8(Param::GroupOrder) {
        let order = GroupOrder::try_from(u64::from(order))?;
        properties.insert(KeyValuePair::Varint(property::DEFAULT_PUBLISHER_GROUP_ORDER, order.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
    use rstest::rstest;

    /// Draft 11 to 16 encode by parity: numbers need an even type, bytes and Locations an odd one.
    #[test]
    fn parity_fits_kind_up_to_draft_16() {
        for draft in 11..=16 {
            let version = Version::from_draft(draft).unwrap();
            for param in ALL {
                if let Some(ty) = param.ty(version) {
                    let numeric = matches!(param.kind(), Kind::U8 | Kind::Varint);
                    assert_eq!(ty % 2 == 0, numeric, "{param:?} in draft {draft}");
                }
            }
        }
    }

    #[test]
    fn types_are_unique_per_draft() {
        for draft in 7..=19 {
            let version = Version::from_draft(draft).unwrap();
            for param in ALL {
                if let Some(ty) = param.ty(version) {
                    assert_eq!(Param::of_type(ty, version), Some(param), "draft {draft}");
                }
            }
        }
    }

    fn example() -> MessageParams {
        let mut params = MessageParams::new();
        params.put_u8(Param::SubscriberPriority, 128);
        params.put_location(Param::LargestObject, Location { group: 1, object: 2 });
        params
    }

    /// draft-ietf-moq-transport-17 §9.3: a uint8 is one byte, a Location two varints without a length.
    #[test]
    fn draft_17_values_have_their_type() {
        let version = MOQ_VERSION_DRAFT_17;
        // count, LARGEST_OBJECT 0x09 (1, 2), SUBSCRIBER_PRIORITY 0x20 - 0x09 = 0x17 128
        assert_eq!(to_vec(&example(), version).unwrap(), [2, 0x09, 1, 2, 0x17, 128]);
    }

    /// draft-ietf-moq-transport-16 §1.4.2: even types a varint, odd types a length and bytes.
    #[test]
    fn draft_16_values_are_pairs() {
        let version = MOQ_VERSION_DRAFT_16;
        // count, LARGEST_OBJECT 0x09 length 2 (1, 2), delta 0x17 varint 128
        assert_eq!(to_vec(&example(), version).unwrap(), [2, 0x09, 2, 1, 2, 0x17, 0x40, 0x80]);
    }

    #[rstest]
    #[case(MOQ_VERSION_DRAFT_15)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(MOQ_VERSION_DRAFT_17)]
    #[case(MOQ_VERSION_DRAFT_18)]
    fn round_trips(#[case] version: Version) {
        let mut decoded: MessageParams = from_slice(&to_vec(&example(), version).unwrap(), version).unwrap();
        assert_eq!(decoded.take_u8(Param::SubscriberPriority), Some(128));
        assert_eq!(decoded.take_location(Param::LargestObject), Some(Location { group: 1, object: 2 }));
        decoded.finish(version).unwrap();
    }

    #[test]
    fn draft_7_numbers_are_varints_in_bytes() {
        let version = MOQ_VERSION_DRAFT_07;
        let mut params = MessageParams::new();
        params.put_varint(Param::ObjectDeliveryTimeout, 300);
        // count, DELIVERY TIMEOUT 0x03, length 2, varint 300
        let bytes = to_vec(&params, version).unwrap();
        assert_eq!(bytes, [1, 0x03, 2, 0x41, 0x2c]);
        let mut decoded: MessageParams = from_slice(&bytes, version).unwrap();
        assert_eq!(decoded.take_varint(Param::ObjectDeliveryTimeout), Some(300));
    }

    #[test]
    fn unknown_parameter_is_skipped_until_draft_16_and_an_error_after() {
        // count, type 0x3F
        assert!(from_slice::<MessageParams>(&[1, 0x3F, 1, 0], MOQ_VERSION_DRAFT_14).unwrap().0.is_empty());
        assert!(matches!(from_slice::<MessageParams>(&[1, 0x3F, 0], MOQ_VERSION_DRAFT_17), Err(Error::ProtocolViolation(_))));
    }

    #[test]
    fn leftover_parameter_is_an_error_from_draft_17() {
        let mut params = MessageParams::new();
        params.put_varint(Param::Expires, 1);
        assert!(params.clone().finish(MOQ_VERSION_DRAFT_16).is_ok());
        assert!(matches!(params.finish(MOQ_VERSION_DRAFT_17), Err(Error::ProtocolViolation(_))));
    }

    #[test]
    fn repeated_parameter_is_an_error() {
        // count 2, FORWARD 1, FORWARD again (delta 0) 1
        assert!(matches!(from_slice::<MessageParams>(&[2, 0x10, 1, 0, 1], MOQ_VERSION_DRAFT_17), Err(Error::ProtocolViolation(_))));
    }

    #[test]
    fn parameter_the_draft_lacks_is_unsupported() {
        let mut params = MessageParams::new();
        params.put_varint(Param::RendezvousTimeout, 1);
        assert!(matches!(to_vec(&params, MOQ_VERSION_DRAFT_16), Err(Error::Unsupported(_))));
    }
}
