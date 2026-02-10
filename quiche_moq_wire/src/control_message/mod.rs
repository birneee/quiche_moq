use std::fmt::Debug;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error::ProtocolViolation;
use crate::{control_message, Version, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_13};
use octets::{Octets, OctetsMut};
pub use announce::PublishNamespaceMessage;
pub use announce_ok::AnnounceOkMessage;
pub use client_setup::ClientSetupMessage;
pub use request_blocked::RequestBlockedMessage;
pub use server_setup::ServerSetupMessage;
pub use subscribe::SubscribeMessage;
pub use subscribe_done::SubscribeDoneMessage;
pub use subscribe_error::SubscribeErrorMessage;
pub use subscribe_ok::SubscribeOkMessage;
pub use unsubscribe_namespace::UnsubscribeNamespaceMessage;
pub use publish_ok::PublishOkMessage;
use crate::control_message::header::ControlMessageHeader;
use crate::control_message::track_status::TrackStatusMessage;
use crate::octets::{peek_varint, put_u16_at, put_varint_with_len_at};

mod announce;
mod announce_ok;
mod client_setup;
pub(crate) mod header;
mod request_blocked;
mod server_setup;
pub mod subscribe;
mod subscribe_done;
mod subscribe_ok;
mod subscribe_error;
mod unsubscribe_namespace;
mod track_status;
mod publish_ok;

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
                let ty = peek_varint(b)?;
                $(
                    if <$type as ControlMessage>::MESSAGE_IDS.contains(&ty) {
                        return Ok(ControlMessageEnum::$variant(<$type>::from_bytes(b, version)?));
                    }
                )*
                Err(ProtocolViolation(format!("unexpected control message with id {}", ty)))
            }
        }
    };
}

control_message_enum! {
    Subscribe(SubscribeMessage),
    ClientSetup(ClientSetupMessage),
    ServerSetup(ServerSetupMessage),
    SubscribeOk(SubscribeOkMessage),
    RequestBlocked(RequestBlockedMessage),
    SubscribeDone(SubscribeDoneMessage),
    SubscribeError(SubscribeErrorMessage),
    PublishNamespace(PublishNamespaceMessage),
    AnnounceOk(AnnounceOkMessage),
    UnsubscribeNamespace(UnsubscribeNamespaceMessage),
    TrackStatus(TrackStatusMessage),
    PublishOk(PublishOkMessage),
}

impl ControlMessageEnum {
    /// check if the buf length matches the encoded length
    fn length_ok(b: &mut OctetsMut, start_off: usize, version: Version) -> bool {
        let end_off = b.off();
        let b = &b.buf()[start_off..end_off];
        let buf_len = b.len();
        let mut b = Octets::with_slice(b);
        let _type = b.get_varint();
        let encoded_len = match version {
            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => b.get_varint().unwrap() as usize,
            MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => b.get_u16().unwrap() as usize,
            _ => unimplemented!(),
        };
        let header_len = b.off();
        buf_len - header_len == encoded_len
    }
}

#[inline]
// helper to encode a control message with its length
pub fn encode_control_message<F: FnOnce(&mut OctetsMut) -> crate::error::Result<()>>(ty: u64, version: Version, b: &mut OctetsMut, f: F) -> crate::error::Result<()> {
    b.put_varint(ty)?;
    let len_off = b.off();
    b.skip(2)?;
    f(b)?;
    control_message::set_control_message_length(b, len_off, version)?;
    Ok(())
}

/// base_off: the base offset before the first byte of the message was added to the buffer
pub(crate) fn set_control_message_length(b: &mut OctetsMut, len_off: usize, version: Version) -> octets::Result<()> {
    let len = b.off() - len_off - 2;
    match version {
        MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_10 => {
            put_varint_with_len_at(b, len as u64, 2, len_off)?;
        }
        MOQ_VERSION_DRAFT_11..=MOQ_VERSION_DRAFT_13 => {
            put_u16_at(b, len as u16, len_off)?;
        }
        _ => unimplemented!()
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::control_message::subscribe::FilterType;
    use crate::Parameters;
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
        let som = SubscribeOkMessage::from(&sm, None);
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        som.to_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        let len = o.off();
        let mut o = Octets::with_slice(&b[..len]);
        let som2 = SubscribeOkMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        assert_eq!(som, som2);
    }
}

pub(crate) trait ControlMessage: Debug + Sized {
    /// The message ID(s) that identify this message type.
    /// Multiple IDs are used for version-specific message IDs.
    const MESSAGE_IDS: &'static [u64];

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
        T::from_body_bytes(b, version)
    }
}
