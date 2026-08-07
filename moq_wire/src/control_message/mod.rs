use std::fmt::Debug;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error::ProtocolViolation;
use crate::vi64;
use crate::{control_message, version, Version};
#[cfg(test)]
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_18};
use octets::{Octets, OctetsMut};
pub use publish_namespace::PublishNamespaceMessage;
pub use publish_namespace_done::PublishNamespaceDoneMessage;
pub use request_ok::RequestOkMessage;
pub use client_setup::ClientSetupMessage;
pub use requests_blocked::RequestsBlockedMessage;
pub use server_setup::ServerSetupMessage;
pub use subscribe::SubscribeMessage;
pub use publish_done::PublishDoneMessage;
pub use request_error::RequestErrorMessage;
pub use subscribe_ok::{GroupOrder, SubscribeOkMessage};
pub use unsubscribe_namespace::UnsubscribeNamespaceMessage;
pub use unsubscribe::UnsubscribeMessage;
pub use publish::PublishMessage;
pub use publish_ok::PublishOkMessage;
pub use request_update::RequestUpdateMessage;
use crate::control_message::header::ControlMessageHeader;
use crate::control_message::track_status::TrackStatusMessage;
use crate::octets::{put_u16_at, put_varint_with_len_at};

mod publish_namespace;
mod publish_namespace_done;
mod request_ok;
mod client_setup;
pub(crate) mod header;
mod requests_blocked;
mod server_setup;
pub mod subscribe;
mod publish_done;
mod subscribe_ok;
mod request_error;
mod unsubscribe_namespace;
mod unsubscribe;
mod track_status;
mod publish;
mod publish_ok;
mod request_update;

macro_rules! control_message_enum {
    ( $( $variant:ident($type:ty) ),* $(,)? ) => {
        #[derive(Debug)]
        pub enum ControlMessageEnum {
            $( $variant($type), )*
        }

        impl ToBytes for ControlMessageEnum {
            fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()> {
                let start_off = b.off();
                match self {
                    $( ControlMessageEnum::$variant(m) => m.to_bytes(b, version)?, )*
                };
                debug_assert!(Self::length_ok(b, start_off, version));
                Ok(())
            }
        }

        impl FromBytes for ControlMessageEnum {
            fn from_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self> {
                let ty = vi64::peek_varint(b, version)?;
                $(
                    if <$type as ControlMessage>::MESSAGE_IDS.contains(&ty) {
                        return Ok(ControlMessageEnum::$variant(<$type>::from_bytes(b, version)?));
                    }
                )*
                Err(ProtocolViolation(format!("unexpected control message with id {}", ty)))
            }
        }

        impl ControlMessageEnum {
            pub fn qlog_type_name(&self) -> &'static str {
                match self {
                    $( ControlMessageEnum::$variant(m) => m.qlog_type_name(), )*
                }
            }
        }
    };
}

control_message_enum! {
    Subscribe(SubscribeMessage),
    ClientSetup(ClientSetupMessage),
    ServerSetup(ServerSetupMessage),
    SubscribeOk(SubscribeOkMessage),
    RequestsBlocked(RequestsBlockedMessage),
    PublishDone(PublishDoneMessage),
    RequestError(RequestErrorMessage),
    PublishNamespace(PublishNamespaceMessage),
    PublishNamespaceDone(PublishNamespaceDoneMessage),
    RequestOk(RequestOkMessage),
    UnsubscribeNamespace(UnsubscribeNamespaceMessage),
    Unsubscribe(UnsubscribeMessage),
    TrackStatus(TrackStatusMessage),
    Publish(PublishMessage),
    PublishOk(PublishOkMessage),
    RequestUpdate(RequestUpdateMessage),
}

impl ControlMessageEnum {
    /// check if the buf length matches the encoded length
    fn length_ok(b: &mut OctetsMut, start_off: usize, version: Version) -> bool {
        let end_off = b.off();
        let b = &b.buf()[start_off..end_off];
        let buf_len = b.len();
        let mut b = Octets::with_slice(b);
        let _type = vi64::read_varint(&mut b, version);
        let encoded_len = if version::message_length_is_fixed_u16(version) {
            b.get_u16().unwrap() as usize
        } else {
            vi64::read_varint(&mut b, version).unwrap() as usize
        };
        let header_len = b.off();
        buf_len - header_len == encoded_len
    }
}

#[inline]
// helper to encode a control message with its length
pub fn encode_control_message<F: FnOnce(&mut OctetsMut) -> crate::error::Result<()>>(ty: u64, version: Version, b: &mut OctetsMut, f: F) -> crate::error::Result<()> {
    vi64::write_varint(b, ty, version)?;
    let len_off = b.off();
    b.skip(2)?;
    f(b)?;
    control_message::set_control_message_length(b, len_off, version)?;
    Ok(())
}

/// base_off: the base offset before the first byte of the message was added to the buffer
pub(crate) fn set_control_message_length(b: &mut OctetsMut, len_off: usize, version: Version) -> octets::Result<()> {
    let len = b.off() - len_off - 2;
    if version::message_length_is_fixed_u16(version) {
        put_u16_at(b, len as u16, len_off)?;
    } else {
        put_varint_with_len_at(b, len as u64, 2, len_off)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::control_message::subscribe::FilterType;
    use crate::Parameters;
    use crate::{MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16};
    use rstest::rstest;
    use super::*;

    #[test]
    fn decode_subscribe_draft7() {
        let mesgs = &[
            [0x03, 0x24, 0x0, 0x0, 0x1, 0xf, 0x69, 0x6e, 0x6a, 0x75, 0x72, 0x65, 0x64, 0x2d, 0x77, 0x61, 0x6c, 0x6c, 0x61, 0x62, 0x79, 0xc, 0x63, 0x61, 0x74, 0x61, 0x6c, 0x6f, 0x67, 0x2e, 0x6a, 0x73, 0x6f, 0x6e, 0x0, 0x2, 0x1, 0x0].as_slice(),
            [0x03, 0x1d, 0x1, 0x1, 0x1, 0xf, 0x69, 0x6e, 0x6a, 0x75, 0x72, 0x65, 0x64, 0x2d, 0x77, 0x61, 0x6c, 0x6c, 0x61, 0x62, 0x79, 0x5, 0x76, 0x69, 0x64, 0x65, 0x6f, 0x1, 0x2, 0x1, 0x0].as_slice(),
        ];

        for msg in mesgs {
            let mut o = Octets::with_slice(&msg);
            let cm = ControlMessageEnum::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
            println!("{:?}", cm);
            let ControlMessageEnum::Subscribe(cm) = cm else { panic!() };
            println!("namespace: {}", cm.track_namespace().iter().map(|e| str::from_utf8(&e).unwrap()).collect::<Vec<&str>>().join(" "));
            println!("name: {}", str::from_utf8(&cm.track_name()).unwrap());
        }
    }

    #[test]
    fn recode_subscribe_draft7() {
        let cm1 = SubscribeMessage {
            request_id: 5,
            track_alias: Some(7),
            namespace_trackname: "namespace--name".parse().unwrap(),
            subscriber_priority: 1,
            group_order: 2,
            forward: None,
            filter_type: FilterType::LargestObject,
            start_location: None,
            end_group: None,
            parameters: Parameters(vec![]),
        };
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        cm1.to_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        let len = o.off();
        let b = &b[..len];
        let mut o = Octets::with_slice(&b);
        let cm2 = SubscribeMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(cm1, cm2);
    }

    #[test]
    fn decode_subscribe_ok_draft7() {
        let b = [0x4, 0x5, 0x0, 0x0, 0x2, 0x0, 0x0];
        let mut o = Octets::with_slice(&b);
        let cm = ControlMessageEnum::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        println!("{:?}", cm);
    }

    #[test]
    fn recode_subscribe_ok_draft7() {
        let sm = SubscribeMessage {
            request_id: 5,
            track_alias: Some(7),
            namespace_trackname: "namespace--track".parse().unwrap(),
            subscriber_priority: 0,
            group_order: 0,
            forward: None,
            filter_type: FilterType::LargestObject,
            start_location: None,
            end_group: None,
            parameters: Parameters(vec![]),
        };
        let som = SubscribeOkMessage::from(&sm, None, None, None);
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        som.to_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        let len = o.off();
        let mut o = Octets::with_slice(&b[..len]);
        let som2 = SubscribeOkMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(som, som2);
    }

    /// Round-trips a SUBSCRIBE message across every draft this crate implements,
    /// including draft-17/18's vi64 varint encoding — a regression here would mean
    /// the wire-level sweep (see `vi64.rs`) missed a call site in this message.
    #[rstest]
    #[case(MOQ_VERSION_DRAFT_07)]
    #[case(MOQ_VERSION_DRAFT_08)]
    #[case(MOQ_VERSION_DRAFT_09)]
    #[case(MOQ_VERSION_DRAFT_10)]
    #[case(MOQ_VERSION_DRAFT_11)]
    #[case(MOQ_VERSION_DRAFT_12)]
    #[case(MOQ_VERSION_DRAFT_13)]
    #[case(MOQ_VERSION_DRAFT_14)]
    #[case(MOQ_VERSION_DRAFT_15)]
    #[case(MOQ_VERSION_DRAFT_16)]
    #[case(crate::MOQ_VERSION_DRAFT_17)]
    #[case(crate::MOQ_VERSION_DRAFT_18)]
    fn subscribe_message_roundtrips_across_versions(#[case] version: Version) {
        let track_alias = (MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11).contains(&version).then_some(7);
        let forward = (version >= MOQ_VERSION_DRAFT_11).then_some(0);
        let cm1 = SubscribeMessage {
            request_id: 5,
            track_alias,
            namespace_trackname: "namespace--name".parse().unwrap(),
            subscriber_priority: 1,
            group_order: 2,
            forward,
            filter_type: FilterType::LargestObject,
            start_location: None,
            end_group: None,
            parameters: Parameters(vec![]),
        };
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        cm1.to_bytes(&mut o, version).unwrap();
        let len = o.off();
        let mut o = Octets::with_slice(&b[..len]);
        let cm2 = SubscribeMessage::from_bytes(&mut o, version).unwrap();
        assert_eq!(cm1, cm2);
    }

    #[test]
    fn recode_subscribe_ok_draft16() {
        use crate::{MOQ_VERSION_DRAFT_16, location::Location};
        let sm = SubscribeMessage {
            request_id: 3,
            track_alias: None, // draft-16: no track_alias in SUBSCRIBE
            namespace_trackname: "namespace--track".parse().unwrap(),
            subscriber_priority: 0,
            group_order: 0,
            forward: None,
            filter_type: FilterType::LargestObject,
            start_location: None,
            end_group: None,
            parameters: Parameters(vec![]),
        };
        let largest = Some(Location { group: 1, object: 42 });
        let som = SubscribeOkMessage::from(&sm, Some(99), largest, None);
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        som.to_bytes(&mut o, MOQ_VERSION_DRAFT_16).unwrap();
        let len = o.off();
        let mut o = Octets::with_slice(&b[..len]);
        let som2 = SubscribeOkMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(som, som2);
    }
}

pub(crate) trait ControlMessage: Debug + Sized {
    /// The message ID(s) that identify this message type.
    /// Multiple IDs are used for version-specific message IDs.
    const MESSAGE_IDS: &'static [u64];

    fn qlog_type_name(&self) -> &'static str;

    /// Returns the message ID to use for encoding based on the version.
    /// Default implementation returns the first ID.
    fn message_id_for_version(_version: Version) -> u64 {
        Self::MESSAGE_IDS[0]
    }

    /// without header
    fn to_body_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::error::Result<()>;

    /// without header
    fn from_body_bytes(b: &mut Octets, version: Version) -> crate::error::Result<Self>;
}

impl<T> ToBytes for T where T: ControlMessage {
    fn to_bytes(&self, b: &mut OctetsMut, version: Version) -> crate::Result<()> {
        encode_control_message(T::message_id_for_version(version), version, b, |b| {
            T::to_body_bytes(self, b, version)
        })
    }
}

impl<T> FromBytes for T where T: ControlMessage {
    fn from_bytes(b: &mut Octets, version: Version) -> crate::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        debug_assert_eq!(T::message_id_for_version(version), header.ty());
        // Bound the body to exactly `header.len()` bytes so fields whose length is
        // implicit (e.g. draft-18+ Track Properties, which run to the end of the
        // message rather than being count-prefixed) can read "until exhausted"
        // without overrunning into the next control message.
        let body = b.get_bytes(header.len())?;
        let mut body = Octets::with_slice(body.as_ref());
        T::from_body_bytes(&mut body, version)
    }
}
