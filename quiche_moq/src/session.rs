use crate::config::Config;
use crate::error::Error;
use crate::error::Result;
use crate::in_stream::InStream;
use crate::in_track::InTrack;
use crate::out_stream::OutStream;
use crate::out_track::OutTrack;
use crate::pending_subscribe::PendingSubscribe;
use log::{debug, error, trace};
use octets::{Octets, OctetsMut};
use quiche::{h3, Shutdown};
use quiche_webtransport as wt;
use short_buf::ShortBuf;
use smallvec::SmallVec;
use std::collections::HashMap;
use quiche_moq_wire::{FromBytes, Namespace, Parameters, RequestId, Role, SetupParameters, ToBytes, TrackAlias, Tuple, Version, DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER, MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, RESET_STREAM_CODE_DELIVERY_TIMEOUT};
use quiche_moq_wire::control_message::{AnnounceMessage, AnnounceOkMessage, ClientSetupMessage, ControlMessage, ServerSetupMessage, SubscribeErrorMessage, SubscribeOkMessage};
use quiche_moq_wire::control_message::subscribe::{FilterType, SubscribeMessage};
use quiche_moq_wire::object::ObjectHeader;
use quiche_utils::stream_id::StreamID;

pub struct MoqTransportSession {
    server: bool,
    /// Always `Some` for client
    /// Is `None` for server if the client has not opened the control stream yet
    control_stream_id: Option<StreamID>,
    pub(crate) webtransport_session_id: StreamID,
    ctrl_buf: ShortBuf<1024>,
    /// is none if setup is not complete
    pub(crate) selected_version: Option<Version>,
    next_request_id: RequestId,
    max_request_id: RequestId,
    pub(crate) in_streams: HashMap<StreamID, InStream>,
    in_tracks: HashMap<TrackAlias, InTrack>,
    /// Egress tracks.
    pub(crate) out_tracks: HashMap<TrackAlias, OutTrack>,
    /// only used by draft 12 and newer
    next_out_track_alias: TrackAlias,
    /// Subscribe requests the peer has not responded to.
    pending_subscribe: HashMap<RequestId, PendingSubscribe>,
    /// Received subscribe responses not yet polled by upper layer
    pending_subscribe_responses: HashMap<RequestId, core::result::Result<(TrackAlias, SubscribeOkMessage), SubscribeErrorMessage>>,
    /// Streams that cannot be associated with a track yet because the SUBSCRIBE_OK is not received yet.
    /// https://datatracker.ietf.org/doc/html/draft-ietf-moq-transport-13#name-subgroup-header
    pending_streams: HashMap<TrackAlias, StreamID>,
    /// Received subscriptions that have not been answered
    pending_received_subscriptions: HashMap<RequestId, SubscribeMessage>,
    pub(crate) out_streams: HashMap<StreamID, OutStream>,
    config: Config,
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
            webtransport_session_id: session_id.into(),
            ctrl_buf: ShortBuf::new(),
            selected_version: None,
            next_request_id: 1,
            max_request_id: 0,
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            out_streams: HashMap::new(),
            config: config.clone(),
        };
        s.send_control_message(
            quich_conn,
            wt,
            &ControlMessage::ClientSetup(ClientSetupMessage {
                supported_versions: config.supported_versions,
                setup_parameters: SetupParameters {
                    path: None,
                    max_request_id: Some(100),
                    role: Some(Role::PubSub),
                    extra_parameters: vec![
                    ],
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
            next_request_id: 0,
            max_request_id: 0,
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            out_streams: HashMap::new(),
            config,
        }
    }

    /// Returns the request_id
    pub fn subscribe(
        &mut self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        namespace: Vec<Vec<u8>>,
        trackname: Vec<u8>,
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
            &ControlMessage::Subscribe(SubscribeMessage {
                request_id,
                track_alias,
                track_namespace: namespace,
                track_name: trackname.clone(),
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
        debug!("moq subscribe {:?}", String::from_utf8_lossy(&trackname));
        Ok(request_id)
    }

    fn send_control_message(
        &self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        cm: &ControlMessage,
    ) {
        let Some(control_stream_id) = self.control_stream_id else {
            panic!("control stream not opened yet")
        };
        let mut b = [0u8; 100];
        let mut o = OctetsMut::with_slice(&mut b);
        cm.to_bytes(
            &mut o,
            self.selected_version.unwrap_or(self.config.setup_version),
        )
        .unwrap();
        let len = o.off();
        wt.stream_send(control_stream_id.into(), conn, &b[..len], false)
            .unwrap();
        debug!(
            "moq send control message on stream {}: {:?}",
            control_stream_id, &cm
        );
    }

    pub fn poll(
        &mut self,
        quic: &mut quiche::Connection,
        h3: &mut h3::Connection,
        wt: &mut quiche_webtransport::Connection,
    ) {
        trace!("poll moq");
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
                    Err(e) => unimplemented!("{:?}", e),
                };
                match cm {
                    ControlMessage::ServerSetup(cm) => {
                        assert!(!self.server);
                        self.selected_version = Some(cm.selected_version);
                        self.max_request_id = cm
                            .setup_parameters
                            .max_request_id
                            .unwrap_or(DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER);
                    }
                    ControlMessage::RequestBlocked(cm) => {
                        error!("{:?}", cm)
                    }
                    ControlMessage::SubscribeOk(cm) => {
                        let req_id = cm.request_id();
                        let req = self.pending_subscribe.remove(&req_id).unwrap();
                        let track_alias = match self.selected_version.unwrap() {
                            MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11 => req.track_alias().unwrap(),
                            MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13 => cm.track_alias().unwrap(),
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
                        self.pending_subscribe_responses.insert(req_id, Ok((track_alias, cm)));
                    }
                    ControlMessage::SubscribeError(cm) => {
                        error!("{:?}", cm);
                        let req_id = cm.request_id();
                        let _req = self.pending_subscribe.remove(&req_id).unwrap();
                        self.pending_subscribe_responses.insert(req_id, Err(cm));
                    }
                    ControlMessage::SubscribeDone(_cm) => {}
                    ControlMessage::Announce(cm) => {
                        //todo handle announcement
                        self.send_control_message(
                            quic,
                            wt,
                            &ControlMessage::AnnounceOk(AnnounceOkMessage::new(cm.request_id(), None)),
                        );
                    }
                    ControlMessage::ClientSetup(cm) => {
                        assert!(self.server);
                        //todo make list of supported version configurable
                        assert!(cm.supported_versions.contains(&self.config.setup_version));
                        let version = self.config.setup_version;
                        self.selected_version = Some(version);
                        self.send_control_message(
                            quic,
                            wt,
                            &ControlMessage::ServerSetup(ServerSetupMessage::new(
                                version,
                                SetupParameters {
                                    path: None,
                                    max_request_id: Some(100),
                                    role: Some(Role::PubSub),
                                    extra_parameters: vec![],
                                },
                            )),
                        );
                    }
                    ControlMessage::Subscribe(cm) => {
                        self.pending_received_subscriptions
                            .insert(cm.request_id, cm);
                    }
                    ControlMessage::AnnounceOk(_cm) => {
                        // todo relate to announce and make info available to application
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
                            InStream::new(stream_id.into(), self.webtransport_session_id, self.selected_version.unwrap()),
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
    ) -> Result<ControlMessage> {
        let Some(control_stream_id) = self.control_stream_id else {
            panic!("control stream not opened yet")
        };
        let cm = loop {
            let mut o = Octets::with_slice(self.ctrl_buf.buffer());
            match ControlMessage::from_bytes(&mut o, self.selected_version.unwrap_or(self.config.setup_version)) {
                Ok(v) => {
                    self.ctrl_buf.consume(o.off());
                    trace!("received control message {:?}", v);
                    break v
                },
                Err(quiche_moq_wire::Error::Octets(octets::BufferTooShortError)) => {
                    self.ctrl_buf.fill(|b| {
                        wt.recv_stream(
                            control_stream_id.into(),
                            self.webtransport_session_id.into(),
                            h3,
                            quic,
                            b,
                        )
                    })?
                },
                Err(e) => unimplemented!("{:?}", e)
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

    /// Get a pending request from the peer if available.
    /// use `accept_subscription` to create a track.
    pub fn next_pending_received_subscription(&self) -> Option<RequestId> {
        self.pending_received_subscriptions.keys().next().cloned()
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
        let (out_cm, track_alias) = match self.selected_version {
            Some(MOQ_VERSION_DRAFT_07..=MOQ_VERSION_DRAFT_11) => (ControlMessage::SubscribeOk(SubscribeOkMessage::from(&cm, None)), cm.track_alias.unwrap()),
            Some(MOQ_VERSION_DRAFT_12..=MOQ_VERSION_DRAFT_13) => {
                let track_alias = self.next_out_track_alias;
                self.next_out_track_alias += 1;
                (ControlMessage::SubscribeOk(SubscribeOkMessage::from(&cm, Some(track_alias))), track_alias)
            },
            Some(_) => unimplemented!(),
            None => unreachable!(),
        };
        self.send_control_message(
            quic,
            wt,
            &out_cm,
        );
        self.out_tracks.insert(track_alias, OutTrack::new());
        track_alias
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
        let track = self.in_tracks.get_mut(&track_alias).unwrap();
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

    pub fn pending_received_subscription(&mut self, request_id: RequestId) -> &SubscribeMessage {
        self.pending_received_subscriptions
            .get(&request_id)
            .unwrap()
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

    pub fn poll_subscribe_response(&mut self, request_id: RequestId) -> Option<core::result::Result<(TrackAlias, SubscribeOkMessage), SubscribeErrorMessage>> {
        self.pending_subscribe_responses.remove(&request_id)
    }

    pub fn announce(
        &mut self,
        conn: &mut quiche::Connection,
        wt: &mut wt::Connection,
        namespace: Vec<Vec<u8>>,
    ) -> Result<()> {
        self.send_control_message(
            conn,
            wt,
            &ControlMessage::Announce(AnnounceMessage::new(
                Some(0), //todo
                Namespace(Tuple(namespace)),
                Parameters(vec![]),
            )),
        );

        Ok(())
    }
}
