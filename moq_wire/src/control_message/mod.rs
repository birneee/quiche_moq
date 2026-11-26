use crate::prelude::*;
use core::fmt::Debug;
use crate::bytes::{FromBytes, ToBytes};
use crate::error::Error::ProtocolViolation;
use crate::Version;
#[cfg(test)]
use crate::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11};
use crate::{Namespace, RequestId};
use crate::{Frame, Mode, Reader, Writer};
pub use publish_namespace::PublishNamespaceMessage;
pub use publish_namespace_done::PublishNamespaceDoneMessage;
pub use publish_namespace_error::PublishNamespaceErrorMessage;
pub use request_ok::RequestOkMessage;
pub use client_setup::ClientSetupMessage;
pub use requests_blocked::RequestsBlockedMessage;
pub use max_request_id::MaxRequestIdMessage;
pub use server_setup::ServerSetupMessage;
pub use subscribe::SubscribeMessage;
pub use publish_done::PublishDoneMessage;
pub use request_error::{Redirect, RequestErrorMessage};
pub use subscribe_ok::SubscribeOkMessage;
pub use crate::GroupOrder;
pub use unsubscribe_namespace::UnsubscribeNamespaceMessage;
pub use subscribe_namespace::{NamespaceDoneMessage, NamespaceMessage, SubscribeNamespaceMessage};
pub use subscribe_tracks::{PublishBlockedMessage, SubscribeTracksMessage};
pub use unsubscribe::UnsubscribeMessage;
pub use publish::PublishMessage;
pub use publish_ok::PublishOkMessage;
pub use request_update::RequestUpdateMessage;
pub use setup::SetupMessage;
use crate::control_message::header::ControlMessageHeader;
pub use track_status::{TrackStatusErrorMessage, TrackStatusMessage, TrackStatusOkMessage};
pub use fetch::{FetchCancelMessage, FetchErrorMessage, FetchKind, FetchMessage, FetchOkMessage, FetchRange};

mod publish_namespace;
mod publish_namespace_done;
mod publish_namespace_error;
mod request_ok;
mod client_setup;
pub(crate) mod header;
mod requests_blocked;
mod max_request_id;
mod server_setup;
pub mod subscribe;
mod publish_done;
mod subscribe_ok;
mod request_error;
mod unsubscribe_namespace;
mod subscribe_namespace;
mod subscribe_tracks;
mod unsubscribe;
mod track_status;
mod publish;
mod publish_ok;
mod request_update;
mod fetch;
mod setup;

macro_rules! control_message_enum {
    ( $( $variant:ident($type:ty) ),* $(,)? ) => {
        #[derive(Debug)]
        pub enum ControlMessageEnum {
            $( $variant($type), )*
        }

        impl ToBytes for ControlMessageEnum {
            fn to_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()> {
                match self {
                    $( ControlMessageEnum::$variant(m) => m.to_bytes(b, version)?, )*
                };
                Ok(())
            }
        }

        impl FromBytes for ControlMessageEnum {
            fn from_bytes<M: Mode>(b: &mut Reader<'_, M>, version: Version) -> crate::error::Result<Self> {
                let ty = b.clone().varint()?;
                $(
                    if <$type as ControlMessage>::MESSAGE_IDS.contains(&ty) && <$type as ControlMessage>::accepts(version) {
                        return Ok(ControlMessageEnum::$variant(<$type>::from_bytes(b, version)?));
                    }
                )*
                Err(crate::Error::UnknownMessage(ty))
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
    // before the messages of older drafts sharing their types
    Namespace(NamespaceMessage),
    NamespaceDone(NamespaceDoneMessage),
    SubscribeNamespace(SubscribeNamespaceMessage),
    SubscribeTracks(SubscribeTracksMessage),
    PublishBlocked(PublishBlockedMessage),
    Subscribe(SubscribeMessage),
    ClientSetup(ClientSetupMessage),
    ServerSetup(ServerSetupMessage),
    SubscribeOk(SubscribeOkMessage),
    RequestsBlocked(RequestsBlockedMessage),
    MaxRequestId(MaxRequestIdMessage),
    PublishDone(PublishDoneMessage),
    RequestError(RequestErrorMessage),
    PublishNamespace(PublishNamespaceMessage),
    PublishNamespaceDone(PublishNamespaceDoneMessage),
    PublishNamespaceError(PublishNamespaceErrorMessage),
    RequestOk(RequestOkMessage),
    UnsubscribeNamespace(UnsubscribeNamespaceMessage),
    Unsubscribe(UnsubscribeMessage),
    TrackStatus(TrackStatusMessage),
    TrackStatusOk(TrackStatusOkMessage),
    TrackStatusError(TrackStatusErrorMessage),
    Fetch(FetchMessage),
    FetchOk(FetchOkMessage),
    FetchError(FetchErrorMessage),
    FetchCancel(FetchCancelMessage),
    Publish(PublishMessage),
    PublishOk(PublishOkMessage),
    RequestUpdate(RequestUpdateMessage),
    Setup(SetupMessage),
}

/// How a received reply, end or update names the request it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestRef<'a> {
    Id(RequestId),
    /// the namespace of a PUBLISH_NAMESPACE, before draft 11 and for PUBLISH_NAMESPACE_DONE before draft 16
    Namespace(&'a Namespace),
    /// the request's stream, draft 17 on
    Stream,
}

impl ControlMessageEnum {
    /// How this message, received in `version`, names the request it answers, ends or updates, `None` for other messages.
    pub fn request_ref<'a>(&'a self, version: Version) -> Option<RequestRef<'a>> {
        use ControlMessageEnum::*;
        let by_id = |request_id| if version.stream_per_request() { RequestRef::Stream } else { RequestRef::Id(request_id) };
        let by_namespace = |namespace: Option<&'a crate::Namespace>, request_id| namespace.map_or_else(|| by_id(request_id), RequestRef::Namespace);
        Some(match self {
            SubscribeOk(m) => by_id(m.request_id),
            PublishOk(m) => by_id(m.request_id),
            RequestError(m) => by_id(m.request_id),
            PublishDone(m) => by_id(m.request_id),
            RequestOk(m) => by_namespace(m.track_namespace(), m.request_id),
            PublishNamespaceError(m) => by_namespace(m.track_namespace(), m.request_id),
            PublishNamespaceDone(m) => by_namespace(m.namespace(), m.request_id),
            RequestUpdate(m) => by_id(m.existing_request_id),
            TrackStatusOk(m) => by_id(m.request_id),
            TrackStatusError(m) => by_id(m.request_id),
            FetchOk(m) => by_id(m.request_id),
            FetchError(m) => by_id(m.request_id),
            FetchCancel(m) => by_id(m.request_id),
            _ => return None,
        })
    }

    /// From draft 17 a request stream begins with it (draft-ietf-moq-transport-19 §3.3).
    pub fn begins_request_stream(&self) -> bool {
        matches!(self, Self::Subscribe(_) | Self::Publish(_) | Self::PublishNamespace(_) | Self::TrackStatus(_) | Self::Fetch(_) | Self::SubscribeNamespace(_) | Self::SubscribeTracks(_))
    }

    /// Names the request a [`RequestRef::Namespace`] or [`RequestRef::Stream`] resolved to.
    pub fn set_request_id(&mut self, request_id: RequestId) {
        use ControlMessageEnum::*;
        match self {
            SubscribeOk(m) => m.request_id = request_id,
            PublishOk(m) => m.request_id = request_id,
            RequestError(m) => m.request_id = request_id,
            PublishDone(m) => m.request_id = request_id,
            RequestOk(m) => m.request_id = request_id,
            PublishNamespaceError(m) => m.request_id = request_id,
            PublishNamespaceDone(m) => m.request_id = request_id,
            RequestUpdate(m) => m.existing_request_id = request_id,
            TrackStatusOk(m) => m.request_id = request_id,
            TrackStatusError(m) => m.request_id = request_id,
            FetchOk(m) => m.request_id = request_id,
            FetchError(m) => m.request_id = request_id,
            FetchCancel(m) => m.request_id = request_id,
            _ => {}
        }
    }
}

/// The Request ID of a request message, in draft 17 with a Required Request ID Delta of 0.
pub(crate) fn encode_request_id(w: &mut Writer, request_id: RequestId, version: Version) -> crate::Result<()> {
    w.varint(request_id.0)?;
    if version.has_required_request_id() {
        w.varint(0)?;
    }
    Ok(())
}

/// Inverse of [`encode_request_id`], a request depending on another isn't supported.
pub(crate) fn decode_request_id<M: Mode>(r: &mut Reader<'_, M>, version: Version) -> crate::Result<RequestId> {
    let request_id = RequestId(r.varint()?);
    if version.has_required_request_id() {
        // draft-ietf-moq-transport-17 §9.2: Required Request ID = Request ID - 2 * delta
        let delta = r.varint()?;
        if delta.checked_mul(2).is_none_or(|d| d > request_id.0) {
            return Err(ProtocolViolation(format!("Required Request ID Delta {delta} beyond Request ID {}", request_id.0)));
        }
        if delta != 0 {
            return Err(crate::Error::Unsupported("requests depending on other requests".into()));
        }
    }
    Ok(request_id)
}

/// A message drafts after `last` don't have: sending it is unsupported, receiving it a protocol violation.
pub(crate) fn until_draft(last: u8, message: &str, version: Version, receiving: bool) -> crate::Result<()> {
    if version.draft() <= last {
        return Ok(());
    }
    let reason = format!("{message} after draft {last}");
    Err(if receiving { ProtocolViolation(reason) } else { crate::Error::Unsupported(reason) })
}

/// A field only some drafts have, `what` names it when the message lacks it.
pub(crate) fn required<T>(field: Option<T>, what: &'static str) -> crate::Result<T> {
    field.ok_or(crate::Error::Unencodable(what))
}

pub(crate) trait ControlMessage: Debug + Sized {
    /// The message ID(s) that identify this message type.
    /// Multiple IDs are used for version-specific message IDs.
    const MESSAGE_IDS: &'static [u64];

    fn qlog_type_name(&self) -> &'static str;

    /// Whether `version` has this message, under its type: types are reused across drafts.
    fn accepts(_version: Version) -> bool {
        true
    }

    /// Returns the message ID to use for encoding based on the version.
    /// Default implementation returns the first ID.
    fn message_id_for_version(_version: Version) -> u64 {
        Self::MESSAGE_IDS[0]
    }

    /// without header
    fn to_body_bytes(&self, b: &mut Writer, version: Version) -> crate::error::Result<()>;

    /// without header
    fn from_body_bytes(b: &mut Reader<'_, Frame>, version: Version) -> crate::error::Result<Self>;
}

impl<T> ToBytes for T where T: ControlMessage {
    fn to_bytes(&self, b: &mut Writer, version: Version) -> crate::Result<()> {
        b.varint(T::message_id_for_version(version))?;
        match version.message_length_is_fixed_u16() {
            true => b.with_u16_length(|b| self.to_body_bytes(b, version)),
            false => b.with_varint_length(|b| self.to_body_bytes(b, version)),
        }
    }
}

impl<T> FromBytes for T where T: ControlMessage {
    fn from_bytes<M: Mode>(b: &mut Reader<'_, M>, version: Version) -> crate::Result<Self> {
        let header = ControlMessageHeader::from_bytes(b, version)?;
        // e.g. a SETUP message of another draft
        if header.ty() != T::message_id_for_version(version) {
            return Err(crate::error::Error::ProtocolViolation(format!("unexpected message type 0x{:x}", header.ty())));
        }
        // the body as a frame: fields can't overrun into the next message, leftovers are malformed
        let mut body = b.frame(header.len())?;
        let message = T::from_body_bytes(&mut body, version)?;
        body.finish()?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use crate::{RequestId, TrackAlias};
    /// Messages a draft doesn't have close the session instead of panicking.
    #[rstest]
    #[case(crate::PUBLISH_MESSAGE_ID, MOQ_VERSION_DRAFT_15)]
    #[case(crate::PUBLISH_OK_MESSAGE_ID, MOQ_VERSION_DRAFT_11)]
    fn foreign_message_is_protocol_violation(#[case] ty: u64, #[case] version: crate::Version) {
        // type, u16 length, body
        let bytes = [ty as u8, 0, 1, 0];
        let decoded = ControlMessageEnum::from_bytes(&mut version.reader(&bytes), version);
        assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
    }

    /// Valid messages we don't implement, not the peer's fault.
    #[rstest]
    #[case(crate::UNSUBSCRIBE_NAMESPACE_MESSAGE_ID, MOQ_VERSION_DRAFT_16)]
    fn unsupported_message_is_reported(#[case] ty: u64, #[case] version: crate::Version) {
        // type, u16 length, body
        let bytes = [ty as u8, 0, 1, 0];
        let decoded = ControlMessageEnum::from_bytes(&mut version.reader(&bytes), version);
        assert!(matches!(decoded, Err(crate::Error::Unsupported(_))), "{decoded:?}");
    }

    use crate::control_message::subscribe::LocationFilter;
    use crate::GroupOrder;
    
    use crate::{MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16};
    use rstest::rstest;
    use super::*;

    /// UNSUBSCRIBE for request 5, type and u16 length in draft 16, with `len` claimed and `body` sent.
    fn unsubscribe_claiming(len: u16, body: &[u8]) -> Vec<u8> {
        let mut bytes = vec![crate::UNSUBSCRIBE_MESSAGE_ID as u8];
        bytes.extend(len.to_be_bytes());
        bytes.extend(body);
        bytes
    }

    /// A complete message whose fields run past its length is malformed, not incomplete.
    #[test]
    fn fields_past_message_length_are_protocol_violation() {
        let bytes = unsubscribe_claiming(0, &[5]);
        let decoded = ControlMessageEnum::from_bytes(&mut MOQ_VERSION_DRAFT_16.reader(&bytes), MOQ_VERSION_DRAFT_16);
        assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
    }

    /// draft-ietf-moq-transport-16 §9: a length not matching the content is a PROTOCOL_VIOLATION.
    #[test]
    fn trailing_bytes_are_protocol_violation() {
        let bytes = unsubscribe_claiming(2, &[5, 0]);
        let decoded = ControlMessageEnum::from_bytes(&mut MOQ_VERSION_DRAFT_16.reader(&bytes), MOQ_VERSION_DRAFT_16);
        assert!(matches!(decoded, Err(crate::Error::ProtocolViolation(_))), "{decoded:?}");
    }

    #[test]
    fn truncated_message_is_incomplete() {
        let bytes = unsubscribe_claiming(1, &[]);
        let decoded = ControlMessageEnum::from_bytes(&mut MOQ_VERSION_DRAFT_16.reader(&bytes), MOQ_VERSION_DRAFT_16);
        assert!(matches!(decoded, Err(crate::Error::Incomplete { .. })), "{decoded:?}");
    }

    #[test]
    fn decode_subscribe_draft7() {
        let mesgs = &[
            [0x03, 0x24, 0x0, 0x0, 0x1, 0xf, 0x69, 0x6e, 0x6a, 0x75, 0x72, 0x65, 0x64, 0x2d, 0x77, 0x61, 0x6c, 0x6c, 0x61, 0x62, 0x79, 0xc, 0x63, 0x61, 0x74, 0x61, 0x6c, 0x6f, 0x67, 0x2e, 0x6a, 0x73, 0x6f, 0x6e, 0x0, 0x2, 0x1, 0x0].as_slice(),
            [0x03, 0x1d, 0x1, 0x1, 0x1, 0xf, 0x69, 0x6e, 0x6a, 0x75, 0x72, 0x65, 0x64, 0x2d, 0x77, 0x61, 0x6c, 0x6c, 0x61, 0x62, 0x79, 0x5, 0x76, 0x69, 0x64, 0x65, 0x6f, 0x1, 0x2, 0x1, 0x0].as_slice(),
        ];

        for msg in mesgs {
            let mut o = MOQ_VERSION_DRAFT_07.reader(msg);
            let cm = ControlMessageEnum::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
            println!("{:?}", cm);
            let ControlMessageEnum::Subscribe(cm) = cm else { panic!() };
            println!("namespace: {}", cm.track_namespace().iter().map(|e| str::from_utf8(e).unwrap()).collect::<Vec<&str>>().join(" "));
            println!("name: {}", str::from_utf8(cm.track_name()).unwrap());
        }
    }

    #[test]
    fn decode_subscribe_ok_draft7() {
        let b = [0x4, 0x5, 0x0, 0x0, 0x2, 0x0, 0x0];
        let mut o = MOQ_VERSION_DRAFT_07.reader(&b);
        let cm = ControlMessageEnum::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        println!("{:?}", cm);
    }

    #[test]
    fn recode_subscribe_ok_draft7() {
        let sm = SubscribeMessage {
            track_alias: Some(TrackAlias(7)),
            subscriber_priority: 0,
            group_order: None,
            forward: true,
            filter: LocationFilter::LargestObject,
            ..SubscribeMessage::new(RequestId(5), "namespace--track".parse().unwrap())
        };
        let som = SubscribeOkMessage::new(sm.request_id, sm.track_alias.unwrap(), None, None);
        let mut b = vec![];
        som.to_bytes(&mut MOQ_VERSION_DRAFT_07.writer(&mut b), MOQ_VERSION_DRAFT_07).unwrap();
        let len = b.len();
        let mut o = MOQ_VERSION_DRAFT_07.reader(&b[..len]);
        let som2 = SubscribeOkMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_07).unwrap();
        // the group order is a field until draft 14, Ascending unless given
        assert_eq!((som2.request_id(), som2.largest_location(), som2.group_order()), (som.request_id(), None, Some(GroupOrder::Ascending)));
    }

    /// Round-trips a SUBSCRIBE message across every draft this crate implements,
    /// including draft-17/18's leading-ones varints.
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
        let track_alias = (version < MOQ_VERSION_DRAFT_12).then_some(TrackAlias(7));
        let forward = version < MOQ_VERSION_DRAFT_11;
        let cm1 = SubscribeMessage {
            track_alias,
            subscriber_priority: 1,
            group_order: Some(GroupOrder::Descending),
            forward,
            filter: LocationFilter::LargestObject,
            ..SubscribeMessage::new(RequestId(5), "namespace--name".parse().unwrap())
        };
        let mut b = vec![];
        cm1.to_bytes(&mut version.writer(&mut b), version).unwrap();
        let len = b.len();
        let mut o = version.reader(&b[..len]);
        let cm2 = SubscribeMessage::from_bytes(&mut o, version).unwrap();
        assert_eq!(cm1, cm2);
    }

    #[test]
    fn recode_subscribe_ok_draft16() {
        use crate::{MOQ_VERSION_DRAFT_16, location::Location};
        let sm = SubscribeMessage {
            track_alias: None, // draft-16: no track_alias in SUBSCRIBE
            subscriber_priority: 0,
            group_order: None,
            forward: true,
            filter: LocationFilter::LargestObject,
            ..SubscribeMessage::new(RequestId(3), "namespace--track".parse().unwrap())
        };
        let largest = Some(Location { group: 1, object: 42 });
        let som = SubscribeOkMessage::new(sm.request_id, TrackAlias(99), largest, None);
        let mut b = vec![];
        som.to_bytes(&mut MOQ_VERSION_DRAFT_16.writer(&mut b), MOQ_VERSION_DRAFT_16).unwrap();
        let len = b.len();
        let mut o = MOQ_VERSION_DRAFT_16.reader(&b[..len]);
        let som2 = SubscribeOkMessage::from_bytes(&mut o, MOQ_VERSION_DRAFT_16).unwrap();
        assert_eq!(som, som2);
    }

    /// A received REQUEST_OK names its request as its draft does: by namespace, Request ID or stream.
    #[rstest]
    #[case::namespace(MOQ_VERSION_DRAFT_10)]
    #[case::request_id(MOQ_VERSION_DRAFT_16)]
    #[case::stream(crate::MOQ_VERSION_DRAFT_17)]
    fn request_ok_names_its_request_as_its_draft_does(#[case] version: Version) {
        let namespace: Namespace = "n1".parse().unwrap();
        let sent = ControlMessageEnum::RequestOk(RequestOkMessage::for_publish_namespace(RequestId(4), namespace.clone()));
        let mut received: ControlMessageEnum = crate::bytes::from_slice(&crate::bytes::to_vec(&sent, version).unwrap(), version).unwrap();
        let expected = match version.draft() {
            ..=10 => RequestRef::Namespace(&namespace),
            11..=16 => RequestRef::Id(RequestId(4)),
            _ => RequestRef::Stream,
        };
        assert_eq!(received.request_ref(version), Some(expected));
        received.set_request_id(RequestId(4));
        assert!(matches!(received, ControlMessageEnum::RequestOk(m) if m.request_id() == RequestId(4)));
    }
}
