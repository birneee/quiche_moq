use crate::config::Config;
use crate::error::Error;
use crate::error::Result;
use crate::in_stream::InStream;
use crate::in_track::InTrack;
use crate::out_stream::OutStream;
use crate::out_track::OutTrack;
use crate::pending_subscribe::PendingSubscribe;
use crate::session::PublishStatus::{Accepted, Pending, Unknown};
use log::{debug, error, trace};
use octets::{Octets, OctetsMut};
use partial_borrow::SplitOff;
use partial_borrow::prelude::*;
use quiche::{Shutdown, h3};
use quiche_moq_wire::ErrorCode;
use quiche_moq_wire::control_message::subscribe::{FilterType, SubscribeMessage};
use quiche_moq_wire::control_message::{
    ClientSetupMessage, ControlMessageEnum, PublishNamespaceMessage, PublishOkMessage,
    RequestErrorMessage, ServerSetupMessage, SubscribeOkMessage,
};
use quiche_moq_wire::object::ObjectHeader;
use quiche_moq_wire::{
    DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER, FromBytes, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11,
    MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, Namespace, NamespaceTrackname, PROTOCOL_VIOLATION,
    Parameters, RESET_STREAM_CODE_DELIVERY_TIMEOUT, RequestId, Role, SetupParameters, ToBytes,
    TrackAlias, Tuple, Version,
};
use quiche_utils::stream_id::StreamID;
use quiche_webtransport as wt;
use short_buf::ShortBuf;
use smallvec::SmallVec;
use std::collections::{HashMap, HashSet};

const INITIAL_CLIENT_REQUEST_ID: RequestId = 0;
const INITIAL_SERVER_REQUEST_ID: RequestId = 1;

#[derive(PartialBorrow)]
pub struct MoqTransportSession {
    server: bool,
    /// Always `Some` for client
    /// Is `None` for server if the client has not opened the control stream yet
    control_stream_id: Option<StreamID>,
    pub(crate) webtransport_session_id: StreamID,
    ctrl_buf: ShortBuf<1024>,
    /// is none if setup is not complete
    pub(crate) selected_version: Option<Version>,
    // next request_id to send
    next_request_id: RequestId,
    // next expected request_id to receive
    next_expected_request_id: RequestId,
    /// max request_id allowed to send
    max_request_id: RequestId,
    // max request_id allowed to recv
    out_max_request_id: RequestId,
    pub(crate) in_streams: HashMap<StreamID, InStream>,
    in_tracks: HashMap<TrackAlias, InTrack>,
    /// Egress tracks.
    pub(crate) out_tracks: HashMap<TrackAlias, OutTrack>,
    /// only used by draft 12 and newer
    next_out_track_alias: TrackAlias,
    /// Subscribe requests the peer has not responded to.
    pending_subscribe: HashMap<RequestId, PendingSubscribe>,
    /// Received subscribe responses not yet polled by upper layer
    pending_subscribe_responses: HashMap<
        RequestId,
        core::result::Result<(TrackAlias, SubscribeOkMessage), RequestErrorMessage>,
    >,
    /// Streams that cannot be associated with a track yet because the SUBSCRIBE_OK is not received yet.
    /// https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-13#name-subgroup-header
    pending_streams: HashMap<TrackAlias, StreamID>,
    /// Received subscriptions that have not been answered
    pending_received_subscriptions: HashMap<RequestId, SubscribeMessage>,
    pending_received_publish_namespace: HashMap<RequestId, PublishNamespaceMessage>,
    pub(crate) out_streams: HashMap<StreamID, OutStream>,
    config: Config,
    closed: bool,
    /// Namespaces this connection is subscribed to from the peer.
    subscribed_namespaces: HashSet<Namespace>,
    pending_sent_publish_namespace: HashMap<RequestId, PublishNamespaceMessage>,
}

impl MoqTransportSession {
    /// Control stream is opened and the setup message has been exchanged.
    pub fn initialized(&self) -> bool {
        self.selected_version.is_some()
    }

    /// connect to server
    pub fn connect(
        session_id: StreamID,
        h3_conn: &mut h3::Connection,
        quich_conn: &mut quiche::Connection,
        wt: &mut quiche_webtransport::Connection,
        config: Config,
    ) -> MoqTransportSession {
        let control_stream_id = wt
            .open_stream(session_id.into(), h3_conn, quich_conn, true)
            .unwrap();
        let s = Self {
            server: false,
            control_stream_id: Some(control_stream_id.into()),
            webtransport_session_id: session_id,
            ctrl_buf: ShortBuf::new(),
            selected_version: None,
            next_request_id: INITIAL_CLIENT_REQUEST_ID,
            next_expected_request_id: INITIAL_SERVER_REQUEST_ID,
            max_request_id: 0,
            out_max_request_id: 100, //TODO make configurable
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            pending_received_publish_namespace: HashMap::new(),
            out_streams: HashMap::new(),
            config: config.clone(),
            closed: false,
            subscribed_namespaces: HashSet::new(),
            pending_sent_publish_namespace: HashMap::new(),
        };
        s.send_control_message(
            quich_conn,
            wt,
            &ControlMessageEnum::ClientSetup(ClientSetupMessage {
                supported_versions: config.supported_versions,
                setup_parameters: SetupParameters {
                    path: None,
                    max_request_id: Some(100),
                    role: Some(Role::PubSub),
                    extra_parameters: vec![],
                },
            }),
        );
        s
    }

    /// accept client
    pub fn accept(session_id: StreamID, config: Config) -> MoqTransportSession {
        Self {
            server: true,
            control_stream_id: None,
            webtransport_session_id: session_id,
            ctrl_buf: ShortBuf::new(),
            selected_version: None,
            next_request_id: INITIAL_SERVER_REQUEST_ID,
            next_expected_request_id: INITIAL_CLIENT_REQUEST_ID,
            max_request_id: 0,
            out_max_request_id: 0,
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            pending_received_publish_namespace: HashMap::new(),
            out_streams: HashMap::new(),
            config,
            closed: false,
            subscribed_namespaces: HashSet::new(),
            pending_sent_publish_namespace: HashMap::new(),
        }
    }

    /// Returns the request_id
    pub fn subscribe(
        &mut self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        namespace_trackname: &NamespaceTrackname,
    ) -> Result<RequestId> {
        if self.next_request_id > self.max_request_id && !self.config.ignore_max_request_quota {
            return Err(Error::RequestBlocked);
            //todo send request blocked control message
        }
        let request_id = self.next_request_id;
        let track_alias = Some(request_id);
        self.send_control_message(
            conn,
            wt,
            &ControlMessageEnum::Subscribe(SubscribeMessage {
                request_id,
                track_alias,
                namespace_trackname: namespace_trackname.clone(),
                subscriber_priority: 1,
                group_order: 2,
                forward: Some(0),
                filter_type: FilterType::NextGroupStart,
                start_location: None,
                end_group: None,
                parameters: Parameters(vec![]),
            }),
        );
        self.pending_subscribe
            .insert(request_id, PendingSubscribe::new(track_alias));
        self.next_request_id += 2;
        debug!("moq subscribe {}", &namespace_trackname);
        Ok(request_id)
    }

    fn send_control_message(
        &self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        cm: &ControlMessageEnum,
    ) {
        Self::_send_control_message(self.as_ref(), conn, wt, cm);
    }

    fn _send_control_message(
        s: &partial!(MoqTransportSession const control_stream_id selected_version config, ! *),
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        cm: &ControlMessageEnum,
    ) {
        let Some(control_stream_id) = *s.control_stream_id else {
            panic!("control stream not opened yet")
        };
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        cm.to_bytes(&mut o, s.selected_version.unwrap_or(s.config.setup_version))
            .unwrap();
        let len = o.off();
        let n = wt
            .stream_send(control_stream_id.into(), quic, &b[..len], false)
            .unwrap();
        assert_eq!(n, len);
        debug!(
            "moq send control message on stream {}: {:?}",
            control_stream_id, &cm
        );
        #[cfg(feature = "qlog")]
        if let Some(qlog) = quic.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
                name: "moqt:control_message_created".into(),
                data: serde_json::json!({
                    "stream_id": control_stream_id.into_u64(),
                    "message": { "message_type": cm.qlog_type_name() },
                }),
            })
            .ok();
        }
    }

    pub fn poll(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) {
        trace!("poll moq");

        if self.closed {
            return;
        }

        let control_stream_id = if let Some(id) = self.control_stream_id {
            id
        } else {
            let id = wt
                .readable_streams(self.webtransport_session_id.into())
                .iter()
                .find(|&&stream_id| StreamID::from(stream_id).is_bidi())
                .copied();

            let Some(id) = id else { return };
            let id = id.into();
            self.control_stream_id = Some(id);
            id
        };

        for stream_id in wt.readable_streams(self.webtransport_session_id.into()) {
            if stream_id == control_stream_id.into_u64() {
                let cm = match self.next_control_message(quic, h3, wt) {
                    Ok(v) => v,
                    Err(Error::Unimplemented) => {
                        error!("unimplemented");
                        continue;
                    }
                    Err(Error::WT(quiche_webtransport::Error::Done)) => continue,
                    Err(Error::Wire(quiche_moq_wire::Error::ProtocolViolation(_))) => {
                        wt.close_session(
                            self.webtransport_session_id.into_u64(),
                            PROTOCOL_VIOLATION,
                            "",
                            quic,
                            h3,
                        )
                        .unwrap();
                        self.closed = true;
                        return;
                    }
                    Err(e) => unimplemented!("{:?}", e),
                };
                #[cfg(feature = "qlog")]
                if let Some(qlog) = quic.qlog_streamer() {
                    qlog.add_event_now(qlog::events::JsonEvent {
                        time: 0.0,
                        importance: qlog::events::EventImportance::Core,
                        name: "moqt:control_message_parsed".into(),
                        data: serde_json::json!({
                            "stream_id": control_stream_id.into_u64(),
                            "message": { "message_type": cm.qlog_type_name() },
                        }),
                    })
                    .ok();
                }
                match cm {
                    ControlMessageEnum::ServerSetup(cm) => {
                        assert!(!self.server);
                        self.selected_version = Some(cm.selected_version);
                        self.max_request_id = cm
                            .setup_parameters
                            .max_request_id
                            .unwrap_or(DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER);
                    }
                    ControlMessageEnum::RequestsBlocked(cm) => {
                        error!("{:?}", cm)
                    }
                    ControlMessageEnum::SubscribeOk(cm) => {
                        let req_id = cm.request_id();
                        let req = self.pending_subscribe.remove(&req_id).unwrap();
                        let track_alias = match self.selected_version.unwrap() {
                            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => {
                                req.track_alias().unwrap()
                            }
                            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => {
                                cm.track_alias().unwrap()
                            }
                            _ => unimplemented!(),
                        };
                        self.in_tracks
                            .insert(track_alias, InTrack::new(track_alias));
                        if let Some(stream_id) = self.pending_streams.remove(&track_alias) {
                            self.in_tracks
                                .get_mut(&track_alias)
                                .unwrap()
                                .mark_stream_readable(stream_id);
                        }
                        self.pending_subscribe_responses
                            .insert(req_id, Ok((track_alias, cm)));
                    }
                    ControlMessageEnum::RequestError(cm) => {
                        let req_id = cm.request_id();
                        let _req = self.pending_subscribe.remove(&req_id).unwrap();
                        self.pending_subscribe_responses.insert(req_id, Err(cm));
                    }
                    ControlMessageEnum::PublishDone(_cm) => {}
                    ControlMessageEnum::PublishNamespace(cm) => {
                        let request_id = cm.request_id().unwrap(); //todo
                        assert!(request_id <= self.out_max_request_id, "INVALID_REQUEST_ID");
                        assert_eq!(
                            request_id, self.next_expected_request_id,
                            "INVALID_REQUEST_ID"
                        );
                        self.next_expected_request_id += 2;
                        self.pending_received_publish_namespace
                            .insert(request_id, cm);
                    }
                    ControlMessageEnum::ClientSetup(cm) => {
                        assert!(self.server);
                        //todo make list of supported version configurable
                        assert!(cm.supported_versions.contains(&self.config.setup_version));
                        let version = self.config.setup_version;
                        self.selected_version = Some(version);
                        self.max_request_id = cm
                            .setup_parameters
                            .max_request_id
                            .unwrap_or(DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER);
                        self.send_control_message(
                            quic,
                            wt,
                            &ControlMessageEnum::ServerSetup(ServerSetupMessage::new(
                                version,
                                SetupParameters {
                                    path: None,
                                    max_request_id: Some(self.out_max_request_id),
                                    role: Some(Role::PubSub),
                                    extra_parameters: vec![],
                                },
                            )),
                        );
                    }
                    ControlMessageEnum::Subscribe(cm) => {
                        self.pending_received_subscriptions
                            .insert(cm.request_id, cm);
                    }
                    ControlMessageEnum::RequestOk(_cm) => {
                        // todo relate to announce and make info available to application
                    }
                    ControlMessageEnum::PublishOk(cm) => {
                        let pom = self
                            .pending_sent_publish_namespace
                            .remove(&cm.request_id())
                            .unwrap();
                        self.subscribed_namespaces
                            .insert(pom.take_track_namespace());
                    }
                    _ => unimplemented!(),
                }
            } else {
                // non-control stream
                let stream = match self.in_streams.get_mut(&stream_id.into()) {
                    Some(v) => v,
                    None => {
                        self.in_streams.insert(
                            stream_id.into(),
                            InStream::new(
                                stream_id.into(),
                                self.webtransport_session_id,
                                self.selected_version.unwrap(),
                            ),
                        );
                        self.in_streams.get_mut(&stream_id.into()).unwrap()
                    }
                };
                stream.read(quic, h3, wt).unwrap();
                stream.mark_readable();
                let Some(subgroup_header) = stream.subgroup_header() else {
                    continue;
                };
                let track_alias = subgroup_header.track_alias();
                match self.in_tracks.get_mut(&track_alias) {
                    Some(track) => {
                        track.mark_stream_readable(stream_id.into());
                    }
                    None => {
                        let prev = self.pending_streams.insert(track_alias, stream_id.into());
                        assert!(prev.is_none()); // todo maybe append list
                    }
                }
            }
        }
    }

    /// Returns `Error::Done` when no control message is available yet
    fn next_control_message(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) -> Result<ControlMessageEnum> {
        let Some(control_stream_id) = self.control_stream_id else {
            panic!("control stream not opened yet")
        };
        let cm = loop {
            let mut o = Octets::with_slice(self.ctrl_buf.buffer());
            match ControlMessageEnum::from_bytes(
                &mut o,
                self.selected_version.unwrap_or(self.config.setup_version),
            ) {
                Ok(v) => {
                    self.ctrl_buf.consume(o.off());
                    trace!("received control message {:?}", v);
                    break v;
                }
                Err(quiche_moq_wire::Error::Octets(octets::BufferTooShortError)) => {
                    self.ctrl_buf.fill(|b| {
                        wt.recv_stream(
                            control_stream_id.into(),
                            self.webtransport_session_id.into(),
                            h3,
                            quic,
                            b,
                        )
                    })?;
                    trace!("fill ctrl_buf {:?}", self.ctrl_buf.buffer())
                }
                Err(e) => unimplemented!("{:?}", e),
            };
        };
        Ok(cm)
    }

    /// Readable track aliases
    pub fn readable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.in_tracks {
            if track.readable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// Writable track aliases
    pub fn writable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.out_tracks {
            if track.writable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// Send a MoQ object on a track.
    /// This might open a new MoQ subgroup (unidirectional WebTransport/QUIC stream).
    /// do not send partially.
    pub fn send_obj(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream_id = match track.current_stream_id {
            Some(v) => v,
            None => {
                let stream_id = wt
                    .open_stream(self.webtransport_session_id.into(), h3, quic, false)
                    .unwrap()
                    .into();
                track.current_stream_id = Some(stream_id);
                self.out_streams.insert(
                    stream_id,
                    OutStream::new(stream_id, track_alias, self.selected_version.unwrap()),
                );
                stream_id
            }
        };
        let stream = self.out_streams.get_mut(&stream_id).unwrap();
        stream.send_obj(quic, wt, buf)
    }

    pub fn send_obj_hdr(
        &mut self,
        size: usize,
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<()> {
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream_id = match track.current_stream_id {
            Some(v) => v,
            None => {
                let stream_id = wt
                    .open_stream(self.webtransport_session_id.into(), h3, quic, false)
                    .unwrap()
                    .into();
                track.current_stream_id = Some(stream_id);
                self.out_streams.insert(
                    stream_id,
                    OutStream::new(stream_id, track_alias, self.selected_version.unwrap()),
                );
                stream_id
            }
        };
        let stream = self.out_streams.get_mut(&stream_id).unwrap();
        stream.send_obj_hdr(size, quic, wt)
    }

    pub fn send_obj_pld(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream = self
            .out_streams
            .get_mut(&track.current_stream_id.unwrap())
            .unwrap();
        stream.send_obj_pld(buf, wt, quic)
    }

    /// Get a pending subscription request from the peer if available.
    /// Use `accept_subscription` to accept it.
    /// Or `reject_subscription`.
    pub fn subscription_inbox_next(&self) -> Option<(&RequestId, &SubscribeMessage)> {
        self.pending_received_subscriptions.iter().next()
    }

    /// Pending subscription requests.
    /// Use `accept_subscription` to accept it.
    /// Or `reject_subscription`.
    pub fn pending_received_subscriptions(&self) -> &HashMap<RequestId, SubscribeMessage> {
        &self.pending_received_subscriptions
    }

    /// Process all pending subscription requests via a closure.
    /// Return `SubscriptionRequestAction::Accept` to accept, `Reject(error_code)` to reject,
    /// or `Keep` to defer the decision to a later call.
    pub fn process_subscriptions_requests<F>(
        &mut self,
        mut f: F,
        quic: &mut quiche::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) where
        F: FnMut(&RequestId, &SubscribeMessage) -> SubscriptionRequestAction,
    {
        let (s_iter, s_msg) = SplitOff::<
            partial!(MoqTransportSession mut pending_received_subscriptions, ! *),
        >::split_off_mut(self);
        s_iter
            .pending_received_subscriptions
            .retain(|id, sub| match f(id, sub) {
                SubscriptionRequestAction::Keep => true,
                SubscriptionRequestAction::Accept => {
                    Self::_accept_subscription(s_msg.as_mut(), sub, quic, wt);
                    false
                }
                SubscriptionRequestAction::Reject(error_code) => {
                    Self::_reject_subscription(s_msg.as_ref(), sub, error_code, quic, wt);
                    false
                }
            });
    }

    /// Must be removed from `Self::pending_received_subscriptions` manually
    #[allow(clippy::type_complexity)]
    pub fn _accept_subscription(
        s: &mut partial!(MoqTransportSession const control_stream_id config selected_version, mut next_out_track_alias out_tracks, ! *),
        subscribe_message: &SubscribeMessage,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
    ) -> TrackAlias {
        let (out_cm, track_alias) = match *s.selected_version {
            Some(MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11) => (
                ControlMessageEnum::SubscribeOk(SubscribeOkMessage::from(subscribe_message, None)),
                subscribe_message.track_alias.unwrap(),
            ),
            Some(MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13) => {
                let track_alias = *s.next_out_track_alias;
                *s.next_out_track_alias += 1;
                (
                    ControlMessageEnum::SubscribeOk(SubscribeOkMessage::from(
                        subscribe_message,
                        Some(track_alias),
                    )),
                    track_alias,
                )
            }
            Some(_) => unimplemented!(),
            None => unreachable!(),
        };
        Self::_send_control_message(s.as_ref(), quic, wt, &out_cm);
        s.out_tracks.insert(track_alias, OutTrack::new());
        track_alias
    }

    /// Accept a subscription received from the peer
    pub fn accept_subscription(
        &mut self,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        request_id: RequestId,
    ) -> TrackAlias {
        let cm = self
            .pending_received_subscriptions
            .remove(&request_id)
            .unwrap();
        Self::_accept_subscription(self.as_mut(), &cm, quic, wt)
    }

    /// Must be removed from `Self::pending_received_subscriptions` manually
    pub fn _reject_subscription(
        s: &partial!(MoqTransportSession const control_stream_id selected_version config, ! *),
        subscribe_message: &SubscribeMessage,
        error_code: u64,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
    ) {
        Self::_send_control_message(
            s,
            quic,
            wt,
            &ControlMessageEnum::RequestError(RequestErrorMessage::from(
                subscribe_message,
                error_code,
            )),
        )
    }

    pub fn reject_subscription(
        &mut self,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        request_id: RequestId,
        error_code: u64,
    ) {
        let cm = self
            .pending_received_subscriptions
            .remove(&request_id)
            .unwrap();
        Self::_reject_subscription(self.as_mut(), &cm, error_code, quic, wt);
    }

    /// Get next unanswered namespace publish
    pub fn next_pending_namespace_publish(
        &mut self,
    ) -> Option<(&RequestId, &PublishNamespaceMessage)> {
        self.pending_received_publish_namespace.iter().next()
    }

    /// Accept a namespace publish or announce message from the peer
    pub fn accept_namespace_publish(
        &mut self,
        request_id: RequestId,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
    ) {
        self.send_control_message(
            quic,
            wt,
            &ControlMessageEnum::PublishOk(PublishOkMessage::new(request_id, Parameters(vec![]))),
        );
        let cm = self
            .pending_received_publish_namespace
            .remove(&request_id)
            .unwrap();
        self.subscribed_namespaces.insert(cm.take_track_namespace());
    }

    pub fn remaining_object_payload(&self, track_alias: TrackAlias) -> Result<usize> {
        let track = self.in_tracks.get(&track_alias).unwrap();
        let stream_id = track.current_stream().unwrap();
        Ok(self
            .in_streams
            .get(&stream_id)
            .unwrap()
            .remaining_object_payload())
    }

    pub fn readable_streams(&self, track_alias: TrackAlias) -> &[StreamID] {
        self.in_tracks.get(&track_alias).unwrap().readable_streams()
    }

    pub fn read_obj_hdr(
        &mut self,
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<ObjectHeader> {
        let Some(track) = self.in_tracks.get_mut(&track_alias) else {
            return Err(Error::Done);
        };
        loop {
            let stream_id = track.current_stream().ok_or(Error::Done)?;
            let stream = self.in_streams.get_mut(&stream_id).unwrap();
            match stream.read_obj_hdr(quic, h3, wt) {
                Ok(v) => return Ok(v),
                Err(Error::Fin) => {
                    // remove stream and try next
                    self.in_streams.remove(&stream_id);
                    track.fin_stream(stream_id);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub fn read_obj_pld(
        &mut self,
        buf: &mut [u8],
        track_alias: TrackAlias,
        wt: &mut wt::Connection,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
    ) -> Result<usize> {
        let track = self.in_tracks.get_mut(&track_alias).unwrap();
        let stream_id = track.current_stream().unwrap();
        let stream = self.in_streams.get_mut(&stream_id).unwrap();
        stream.read_obj_pld(quic, h3, wt, buf)
    }

    /// Cancel sending on stream with Delivery Timeout
    pub fn timeout_stream(
        &mut self,
        track_alias: TrackAlias,
        _wt: &mut wt::Connection,
        quic: &mut quiche::Connection,
    ) {
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream_id = track.current_stream_id.unwrap();
        quic.stream_shutdown(
            stream_id.into(),
            Shutdown::Write,
            RESET_STREAM_CODE_DELIVERY_TIMEOUT,
        )
        .unwrap();
        track.current_stream_id = None;
        self.out_streams.remove(&stream_id);
        trace!("timeout stream {}", stream_id);
    }

    pub fn poll_subscribe_response(
        &mut self,
        request_id: RequestId,
    ) -> Option<core::result::Result<(TrackAlias, SubscribeOkMessage), RequestErrorMessage>> {
        self.pending_subscribe_responses.remove(&request_id)
    }

    pub fn publish_namespace(
        &mut self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        namespace: Vec<Vec<u8>>,
    ) -> Result<()> {
        let cm = ControlMessageEnum::PublishNamespace(PublishNamespaceMessage::new(
            Some(self.next_request_id),
            Namespace(Tuple(namespace)),
            Parameters(vec![]),
        ));
        self.send_control_message(conn, wt, &cm);
        let ControlMessageEnum::PublishNamespace(cm) = cm else {
            unreachable!()
        };
        self.pending_sent_publish_namespace
            .insert(self.next_request_id, cm);
        self.next_request_id += 2;
        Ok(())
    }

    pub fn publish_namespace_status(&self, namespace: &Namespace) -> PublishStatus {
        if self.subscribed_namespaces.contains(namespace) {
            Accepted
        } else if self
            .pending_sent_publish_namespace
            .values()
            .any(|n| n.track_namespace() == namespace)
        {
            Pending
        } else {
            Unknown
        }
    }
}

pub enum PublishStatus {
    Unknown,
    Pending,
    Accepted,
}

pub enum SubscriptionRequestAction {
    Keep,
    Accept,
    Reject(ErrorCode),
}
