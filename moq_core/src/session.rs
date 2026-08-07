use crate::config::{Config, VersionNegotiation};
use crate::control_channel::ControlChannel;
use crate::error::Error;
use crate::error::Result;
use crate::in_stream::{Handled, InStream};
use crate::in_track::InTrack;
use crate::out_stream::OutStream;
use crate::out_track::OutTrack;
use crate::padding_stream::OutPaddingStream;
use crate::pending_subscribe::PendingSubscribe;
use crate::transport::{StreamError, Transport};
use log::{debug, error, trace};
use partial_borrow::SplitOff;
use partial_borrow::prelude::*;
use moq_wire::ErrorCode;
use moq_wire::control_message::subscribe::{FilterType, SubscribeMessage};
use moq_wire::control_message::{
    ClientSetupMessage, ControlMessageEnum, PublishDoneMessage, PublishMessage,
    PublishNamespaceDoneMessage, PublishNamespaceMessage, PublishOkMessage, RequestErrorMessage,
    RequestOkMessage, RequestUpdateMessage, ServerSetupMessage, SubscribeOkMessage, UnsubscribeMessage,
};
use moq_wire::object::ObjectHeader;
use moq_wire::subgroup::SubgroupHeader;
use moq_wire::{
    DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER, DELIVERY_TIMEOUT_PARAMETER_ID, KeyValuePairs,
    Location, MOQ_VERSION_DRAFT_07,
    Namespace, NamespaceTrackname, PROTOCOL_VIOLATION, Parameter, Parameters,
    RESET_STREAM_CODE_CANCELED, RESET_STREAM_CODE_DELIVERY_TIMEOUT, RequestId, Role,
    SetupParameters, TrackAlias, Tuple, Version,
};
use smallvec::SmallVec;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Duration;
use web_time::Instant;

const INITIAL_CLIENT_REQUEST_ID: RequestId = 0;
const INITIAL_SERVER_REQUEST_ID: RequestId = 1;

/// State snapshot after a call to [`MoqTransportSession::handle`].
#[derive(Default)]
pub struct PollResult {
    delivery_timeout_changed: bool,
    pending_subscriptions: bool,
    pending_subscribe_responses: bool,
    pending_namespace_publishes: bool,
    pending_track_publishes: bool,
    unsubscribed_tracks: Vec<TrackAlias>,
}

impl PollResult {
    /// A REQUEST_UPDATE was received from a subscriber, changing their delivery timeout.
    pub fn delivery_timeout_changed(&self) -> bool { self.delivery_timeout_changed }
    /// Incoming SUBSCRIBE messages are queued and waiting to be accepted or rejected.
    pub fn pending_subscriptions(&self) -> bool { self.pending_subscriptions }
    /// Responses to outgoing SUBSCRIBE requests (SUBSCRIBE_OK or REQUEST_ERROR) are queued.
    pub fn pending_subscribe_responses(&self) -> bool { self.pending_subscribe_responses }
    /// Incoming PUBLISH_NAMESPACE messages are queued and waiting to be processed.
    pub fn pending_namespace_publishes(&self) -> bool { self.pending_namespace_publishes }
    /// Incoming PUBLISH (track-level) messages are queued and waiting to be accepted or rejected.
    pub fn pending_track_publishes(&self) -> bool { self.pending_track_publishes }
    /// Track aliases an UNSUBSCRIBE arrived for this call — the subscriber no
    /// longer wants Objects. The caller owns per-connection subscriber
    /// bookkeeping this session type doesn't have, so it's responsible for
    /// actually stopping delivery (e.g. dropping the subscriber from whatever
    /// list drives its `send_obj`/`send_obj_hdr_with` calls for this alias).
    pub fn unsubscribed_tracks(&self) -> &[TrackAlias] { &self.unsubscribed_tracks }
}

#[derive(PartialBorrow)]
pub struct MoqTransportSession {
    server: bool,
    /// Owns the shared control channel and, for draft 17+, the per-request
    /// bidi streams — see `crate::control_channel`. Session code never has to
    /// reason about topology (bidi vs. uni-pair) itself; it just calls this.
    control_channel: ControlChannel,
    /// `None` until the setup handshake completes.
    pub(crate) selected_version: Option<Version>,
    next_request_id: RequestId,
    next_expected_request_id: RequestId,
    /// Max request_id we are allowed to send.
    max_request_id: RequestId,
    /// Max request_id we allow the peer to send.
    out_max_request_id: RequestId,
    pub(crate) in_streams: HashMap<u64, InStream>,
    in_tracks: HashMap<TrackAlias, InTrack>,
    pub(crate) out_tracks: HashMap<TrackAlias, OutTrack>,
    /// Only used by draft ≥ 12.
    next_out_track_alias: TrackAlias,
    /// Active subscriptions: request_id → track_alias. Routes PUBLISH_DONE to the right track.
    active_subscriptions: HashMap<RequestId, TrackAlias>,
    pending_subscribe: HashMap<RequestId, PendingSubscribe>,
    pending_subscribe_responses: HashMap<
        RequestId,
        core::result::Result<(TrackAlias, SubscribeOkMessage), RequestErrorMessage>,
    >,
    /// Streams that arrived before their SUBSCRIBE_OK, indexed by track_alias.
    pending_streams: HashMap<TrackAlias, Vec<u64>>,
    pending_received_subscriptions: HashMap<RequestId, SubscribeMessage>,
    received_subscription_tracks: HashMap<RequestId, TrackAlias>,
    pending_received_publish_namespace: HashMap<RequestId, PublishNamespaceMessage>,
    pending_received_publish: HashMap<RequestId, PublishMessage>,
    pending_sent_publish: HashMap<RequestId, PublishMessage>,
    pending_publish_responses:
        HashMap<RequestId, core::result::Result<PublishOkMessage, RequestErrorMessage>>,
    pub(crate) out_streams: HashMap<u64, OutStream>,
    config: Config,
    closed: bool,
    /// Namespaces we announced and that were accepted by the peer.
    sent_namespaces: HashMap<RequestId, Namespace>,
    /// Namespaces the peer announced and that we accepted.
    received_namespaces: HashMap<RequestId, Namespace>,
    pending_sent_publish_namespace: HashMap<RequestId, PublishNamespaceMessage>,
    out_padding_stream: Option<OutPaddingStream>,
    /// Set by `handle()` and state-modifying methods; cleared by `post_handle()`.
    dirty: bool,
}

impl MoqTransportSession {
    /// Connect to server. Opens the control channel and sends CLIENT_SETUP.
    ///
    /// Fails if `config.version_negotiation` can't actually be conveyed on
    /// the wire (see `VersionNegotiation::validate`).
    pub fn connect(t: &mut impl Transport, config: Config) -> crate::error::Result<MoqTransportSession> {
        config.version_negotiation.validate()?;
        let control_channel = ControlChannel::connect(t, config.version_negotiation.preferred());
        let mut s = Self {
            server: false,
            control_channel,
            selected_version: None,
            next_request_id: INITIAL_CLIENT_REQUEST_ID,
            next_expected_request_id: INITIAL_SERVER_REQUEST_ID,
            max_request_id: 0,
            out_max_request_id: 100,
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            active_subscriptions: HashMap::new(),
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            received_subscription_tracks: HashMap::new(),
            pending_received_publish_namespace: HashMap::new(),
            pending_received_publish: HashMap::new(),
            pending_sent_publish: HashMap::new(),
            pending_publish_responses: HashMap::new(),
            out_streams: HashMap::new(),
            config: config.clone(),
            closed: false,
            sent_namespaces: HashMap::new(),
            received_namespaces: HashMap::new(),
            pending_sent_publish_namespace: HashMap::new(),
            out_padding_stream: None,
            dirty: false,
        };
        s.apply_config();
        // `Pinned`: nothing to offer, the version was already settled below
        // this layer — see `ClientSetupMessage::to_body_bytes`, which drops
        // this field from the wire entirely for draft >= 15 regardless.
        let supported_versions = match &config.version_negotiation {
            VersionNegotiation::InBand { candidates } => candidates.clone(),
            VersionNegotiation::Pinned(_) => vec![],
        };
        s.send_control_message(
            t,
            &ControlMessageEnum::ClientSetup(ClientSetupMessage {
                supported_versions,
                setup_parameters: SetupParameters {
                    path: None,
                    max_request_id: Some(100),
                    role: Some(Role::PubSub),
                    extra_parameters: vec![],
                },
            }),
        );
        Ok(s)
    }

    /// Accept an incoming client connection (server side). No I/O performed here;
    /// call `handle()` once packets arrive.
    ///
    /// Fails if `config.version_negotiation` can't actually be conveyed on
    /// the wire (see `VersionNegotiation::validate`).
    pub fn accept(config: Config) -> crate::error::Result<MoqTransportSession> {
        config.version_negotiation.validate()?;
        let mut s = Self {
            server: true,
            control_channel: ControlChannel::accept(),
            selected_version: None,
            next_request_id: INITIAL_SERVER_REQUEST_ID,
            next_expected_request_id: INITIAL_CLIENT_REQUEST_ID,
            max_request_id: 0,
            out_max_request_id: 100,
            in_streams: HashMap::new(),
            in_tracks: HashMap::new(),
            out_tracks: HashMap::new(),
            next_out_track_alias: 0,
            active_subscriptions: HashMap::new(),
            pending_subscribe: HashMap::new(),
            pending_subscribe_responses: HashMap::new(),
            pending_streams: HashMap::new(),
            pending_received_subscriptions: HashMap::new(),
            received_subscription_tracks: HashMap::new(),
            pending_received_publish_namespace: HashMap::new(),
            pending_received_publish: HashMap::new(),
            pending_sent_publish: HashMap::new(),
            pending_publish_responses: HashMap::new(),
            out_streams: HashMap::new(),
            config,
            closed: false,
            sent_namespaces: HashMap::new(),
            received_namespaces: HashMap::new(),
            pending_sent_publish_namespace: HashMap::new(),
            out_padding_stream: None,
            dirty: false,
        };
        s.apply_config();
        Ok(s)
    }

    pub fn initialized(&self) -> bool {
        self.selected_version.is_some()
    }

    pub fn version(&self) -> Option<Version> {
        self.selected_version
    }

    fn apply_config(&mut self) {
        if self.config.enable_padding_stream {
            self.out_padding_stream = Some(OutPaddingStream::new());
        }
    }

    /// Subscribes to a track. Returns the request ID.
    pub fn subscribe(
        &mut self,
        namespace_trackname: &NamespaceTrackname,
        subscriber_priority: Option<u8>,
        delivery_timeout: Option<u64>,
        filter_type: FilterType,
        t: &mut impl Transport,
    ) -> Result<RequestId> {
        self.dirty = true;
        // `start_location`/`end_group` are not exposed by this API, so only filters
        // that need neither can be encoded.
        if filter_type.has_start_location() || filter_type.has_end_group() {
            return Err(Error::Unimplemented);
        }
        if self.next_request_id >= self.max_request_id && !self.config.ignore_max_request_quota {
            return Err(Error::RequestBlocked);
        }
        let request_id = self.next_request_id;
        let track_alias = Some(request_id);
        let mut parameters = vec![];
        if let Some(ms) = delivery_timeout {
            parameters.push(Parameter::new_varint(DELIVERY_TIMEOUT_PARAMETER_ID, ms));
        }
        let cm = ControlMessageEnum::Subscribe(SubscribeMessage {
            request_id,
            track_alias,
            namespace_trackname: namespace_trackname.clone(),
            subscriber_priority: subscriber_priority.unwrap_or(128),
            group_order: 2,
            // 0 ("don't forward") needs a later REQUEST_UPDATE to flip to 1,
            // which this crate doesn't implement — always request forwarding.
            forward: Some(1),
            filter_type,
            start_location: None,
            end_group: None,
            parameters: Parameters(parameters),
        });
        self.send_request(request_id, &cm, t);
        self.pending_subscribe
            .insert(request_id, PendingSubscribe::new(track_alias));
        self.next_request_id += 2;
        debug!("moq subscribe {}", &namespace_trackname);
        Ok(request_id)
    }

    /// Tells the publisher to stop sending Objects for a subscription this
    /// session previously sent via `subscribe` (draft-16 §9.12) and forgets
    /// its local bookkeeping. Sent on the control channel, not a per-request
    /// stream — UNSUBSCRIBE only ever references an existing subscription's
    /// Request ID, it doesn't allocate one of its own.
    pub fn unsubscribe(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.dirty = true;
        self.send_control_message(
            t,
            &ControlMessageEnum::Unsubscribe(UnsubscribeMessage { request_id }),
        );
        if let Some(track_alias) = self.active_subscriptions.remove(&request_id) {
            self.in_tracks.remove(&track_alias);
        }
        self.pending_subscribe.remove(&request_id);
        self.pending_subscribe_responses.remove(&request_id);
    }

    /// Send a REQUEST_UPDATE to change the DELIVERY_TIMEOUT on an existing upstream subscription.
    pub fn request_update_delivery_timeout(
        &mut self,
        existing_request_id: RequestId,
        delivery_timeout: Option<u64>,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.dirty = true;
        if self.next_request_id >= self.max_request_id && !self.config.ignore_max_request_quota {
            return Err(Error::RequestBlocked);
        }
        if self.control_channel.uses_per_request_streams() {
            // draft 17+: REQUEST_UPDATE is a follow-up on the original request's bidi
            // stream, not a new stream. Reading further messages on an already-completed
            // request stream isn't implemented yet.
            unimplemented!("REQUEST_UPDATE follow-ups on per-request streams (draft 17+)");
        }
        let request_id = self.next_request_id;
        let mut parameters = vec![];
        if let Some(ms) = delivery_timeout {
            parameters.push(Parameter::new_varint(DELIVERY_TIMEOUT_PARAMETER_ID, ms));
        }
        self.send_control_message(
            t,
            &ControlMessageEnum::RequestUpdate(RequestUpdateMessage {
                request_id,
                existing_request_id,
                parameters: Parameters(parameters),
            }),
        );
        self.next_request_id += 2;
        Ok(())
    }

    /// Sends a session-level message (not tied to any one request) on the
    /// shared control channel — SETUP, REQUEST_UPDATE, PUBLISH_DONE, etc.
    fn send_control_message(&mut self, t: &mut impl Transport, cm: &ControlMessageEnum) {
        let version = self.selected_version.unwrap_or_else(|| self.config.version_negotiation.preferred());
        self.control_channel.send_control(cm, version, t);
    }

    /// Sends a request-initiating message (SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE),
    /// letting the control channel decide (from the negotiated topology) whether
    /// it needs its own dedicated stream (draft 17+) or just the shared one.
    fn send_request(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) {
        let version = self.selected_version.unwrap_or_else(|| self.config.version_negotiation.preferred());
        self.control_channel.send_request(request_id, cm, version, t);
    }

    /// Sends a response (SUBSCRIBE_OK/PUBLISH_OK/REQUEST_ERROR/...) for
    /// `request_id`, letting the control channel route it to the request's own
    /// stream if it has one (draft 17+) or the shared channel otherwise.
    fn send_response(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) {
        let version = self.selected_version.unwrap_or_else(|| self.config.version_negotiation.preferred());
        self.control_channel.send_response(request_id, cm, version, t);
    }

    /// Process new data from the transport layer.
    ///
    /// `readable` is a slice of stream IDs that have data available; the
    /// caller is responsible for collecting this from the transport before
    /// calling `handle()` (e.g. via `wt.readable_streams()` in quiche_moq, or
    /// a queue of arrived stream events in a browser WebTransport wrapper).
    ///
    /// Panics if called while the session is still dirty — `post_handle` must
    /// be called after every `handle()` and after any state-modifying method.
    pub fn handle(&mut self, t: &mut impl Transport, readable: &[u64]) -> PollResult {
        assert!(!self.dirty, "post_handle must be called before handle() can be called again");
        self.dirty = true;

        let mut result = PollResult::default();
        trace!("poll moq");

        if self.closed {
            return result;
        }

        if !self.control_channel.try_establish(readable, t) {
            return result;
        }
        self.control_channel.flush(t).unwrap();

        for &stream_id in readable {
            if self.control_channel.owns(stream_id) {
                // Either the control channel, or (draft 17+ only) a per-request bidi
                // stream: one we opened (SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE awaiting a
                // response) or one the peer just opened with such a request.
                if self.process_stream_message(stream_id, t, &mut result).is_break() {
                    return result;
                }
            } else {
                // Non-control (data) stream.
                let stream = match self.in_streams.get_mut(&stream_id) {
                    Some(v) => v,
                    None => {
                        let Some(version) = self.selected_version else { continue };
                        self.in_streams.insert(stream_id, InStream::new(stream_id, version));
                        self.in_streams.get_mut(&stream_id).unwrap()
                    }
                };

                let track_alias = match stream.handle(t) {
                    Ok(Handled::Padding) => continue,
                    Ok(Handled::Subgroup(h)) => h.track_alias(),
                    Err(StreamError::Done) => continue,
                    Err(StreamError::Fin)
                    | Err(StreamError::StreamReset(_))
                    | Err(StreamError::InvalidStreamState(_)) => {
                        self.in_streams.remove(&stream_id);
                        continue;
                    }
                    Err(e) => unimplemented!("{:?}", e),
                };
                match self.in_tracks.get_mut(&track_alias) {
                    Some(track) => {
                        track.mark_stream_readable(stream_id);
                    }
                    None => {
                        self.pending_streams.entry(track_alias).or_default().push(stream_id);
                    }
                }
            }
        }

        result.pending_subscriptions = !self.pending_received_subscriptions.is_empty();
        result.pending_subscribe_responses = !self.pending_subscribe_responses.is_empty();
        result.pending_namespace_publishes = !self.pending_received_publish_namespace.is_empty();
        result.pending_track_publishes = !self.pending_received_publish.is_empty();
        result
    }

    /// Reads and dispatches one message from `stream_id` (control channel or, for
    /// draft 17+, a per-request bidi stream). `ControlFlow::Break` means the
    /// session was closed and `handle()` must return immediately.
    fn process_stream_message(
        &mut self,
        stream_id: u64,
        t: &mut impl Transport,
        result: &mut PollResult,
    ) -> ControlFlow<()> {
        let setup_version = self.selected_version.unwrap_or_else(|| self.config.version_negotiation.preferred());
        let read = self.control_channel.next_message(stream_id, self.selected_version, setup_version, t);
        let cm = match read {
            Ok(v) => v,
            Err(Error::Unimplemented) => {
                error!("unimplemented");
                return ControlFlow::Continue(());
            }
            Err(Error::Done) => return ControlFlow::Continue(()),
            Err(Error::Wire(moq_wire::Error::ProtocolViolation(reason))) => {
                error!("protocol violation, closing session: {reason}");
                t.close_session(PROTOCOL_VIOLATION as u64, "");
                self.closed = true;
                return ControlFlow::Break(());
            }
            Err(e) => unimplemented!("{:?}", e),
        };
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
                name: "moqt:control_message_parsed".into(),
                data: serde_json::json!({
                    "stream_id": stream_id,
                    "message": crate::control_channel::cm_qlog_message(&cm),
                }),
            }).ok();
        }
        self.dispatch_received_message(stream_id, cm, t, result);
        ControlFlow::Continue(())
    }

    fn dispatch_received_message(
        &mut self,
        stream_id: u64,
        cm: ControlMessageEnum,
        t: &mut impl Transport,
        result: &mut PollResult,
    ) {
        // draft 17+: REQUEST_OK/SUBSCRIBE_OK/REQUEST_ERROR/PUBLISH_OK drop Request ID
        // from the wire (correlated by stream instead); recover it from the stream
        // this message arrived on, for streams we opened ourselves.
        let req_id_override = self.control_channel.request_id_for_stream(stream_id);
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
                let req_id = req_id_override.unwrap_or_else(|| cm.request_id());
                let req = self.pending_subscribe.remove(&req_id).unwrap();
                let track_alias = if moq_wire::publisher_assigns_track_alias(self.selected_version.unwrap()) {
                    cm.track_alias().unwrap()
                } else {
                    req.track_alias().unwrap()
                };
                self.in_tracks
                    .insert(track_alias, InTrack::new(track_alias));
                if let Some(stream_ids) = self.pending_streams.remove(&track_alias) {
                    let track = self.in_tracks.get_mut(&track_alias).unwrap();
                    for stream_id in stream_ids {
                        track.mark_stream_readable(stream_id);
                    }
                }
                self.active_subscriptions.insert(req_id, track_alias);
                self.pending_subscribe_responses
                    .insert(req_id, Ok((track_alias, cm)));
            }
            ControlMessageEnum::RequestError(cm) => {
                let req_id = req_id_override.unwrap_or_else(|| cm.request_id());
                if self.pending_subscribe.remove(&req_id).is_some() {
                    self.pending_subscribe_responses.insert(req_id, Err(cm));
                } else if self.pending_sent_publish.remove(&req_id).is_some() {
                    self.pending_publish_responses.insert(req_id, Err(cm));
                }
            }
            ControlMessageEnum::PublishDone(cm) => {
                if let Some(&track_alias) = self.active_subscriptions.get(&cm.request_id())
                    && let Some(track) = self.in_tracks.get_mut(&track_alias)
                {
                    track.mark_done(cm.stream_count());
                }
            }
            ControlMessageEnum::Unsubscribe(cm) => {
                // Forget the request-id → track-alias mapping so a reused
                // request_id can't resolve to a stale track, and report it
                // via `PollResult::unsubscribed_tracks` — see that method's
                // doc comment for why this doesn't also touch `out_tracks`.
                if let Some(track_alias) = self.received_subscription_tracks.remove(&cm.request_id) {
                    result.unsubscribed_tracks.push(track_alias);
                }
            }
            ControlMessageEnum::PublishNamespace(cm) => {
                let request_id = match cm.request_id() {
                    Some(request_id) => {
                        assert!(request_id <= self.out_max_request_id, "INVALID_REQUEST_ID");
                        assert_eq!(
                            request_id,
                            self.next_expected_request_id,
                            "INVALID_REQUEST_ID"
                        );
                        self.next_expected_request_id += 2;
                        request_id
                    }
                    // draft 07–10: ANNOUNCE has no Request ID at all (it predates
                    // the concept). Synthesize a local one purely for our own
                    // bookkeeping — never sent on the wire; see
                    // `accept_namespace_publish`'s version-gated reply.
                    None => {
                        let id = self.next_expected_request_id;
                        self.next_expected_request_id += 2;
                        id
                    }
                };
                self.control_channel.record_request_stream_if_needed(stream_id, request_id);
                self.pending_received_publish_namespace.insert(request_id, cm);
            }
            ControlMessageEnum::ClientSetup(cm) => {
                assert!(self.server);
                let version = match &self.config.version_negotiation {
                    // Already settled below this layer (ALPN/WT-Available-Protocols)
                    // — nothing to negotiate, and nothing reliable to read from
                    // `cm.supported_versions` even if we wanted to (see
                    // `ClientSetupMessage::decode_body`).
                    VersionNegotiation::Pinned(v) => *v,
                    // The client already chose the control channel's topology (bidi
                    // vs. draft 17+'s uni pair) by which stream it opened, before
                    // negotiation completes — see `ControlChannel::try_establish`.
                    // We only accept versions whose topology matches what it
                    // actually used.
                    // TODO: this `.expect()` takes down the whole process on a
                    // version mismatch instead of just rejecting this connection
                    // (e.g. with VERSION_NEGOTIATION_FAILED).
                    VersionNegotiation::InBand { candidates } => moq_wire::MOQ_VERSION_PRIORITY
                        .iter()
                        .rev()
                        .find(|v| {
                            candidates.contains(v)
                                && cm.supported_versions.contains(v)
                                && self.control_channel.matches_topology(**v)
                        })
                        .copied()
                        .expect("no mutually supported MoQ version"),
                };
                self.selected_version = Some(version);
                self.max_request_id = cm
                    .setup_parameters
                    .max_request_id
                    .unwrap_or(DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER);
                self.control_channel.send_control(
                    &ControlMessageEnum::ServerSetup(ServerSetupMessage::new(
                        version,
                        SetupParameters {
                            path: None,
                            max_request_id: Some(self.out_max_request_id),
                            role: Some(Role::PubSub),
                            extra_parameters: vec![],
                        },
                    )),
                    version,
                    t,
                );
            }
            ControlMessageEnum::Subscribe(cm) => {
                self.control_channel.record_request_stream_if_needed(stream_id, cm.request_id);
                self.pending_received_subscriptions.insert(cm.request_id, cm);
            }
            ControlMessageEnum::RequestUpdate(cm) => {
                if self.control_channel.uses_per_request_streams() {
                    // draft 17+: REQUEST_UPDATE is a follow-up on the original
                    // request's bidi stream. Not yet implemented — see
                    // `request_update_delivery_timeout`'s matching guard.
                    unimplemented!("REQUEST_UPDATE follow-ups on per-request streams (draft 17+)");
                }
                if let Some(&track_alias) =
                    self.received_subscription_tracks.get(&cm.existing_request_id)
                {
                    if let Some(track) = self.out_tracks.get_mut(&track_alias) {
                        if let Some(ms) =
                            cm.parameters.get_varint(DELIVERY_TIMEOUT_PARAMETER_ID)
                        {
                            track.delivery_timeout = Some(Duration::from_millis(ms));
                            result.delivery_timeout_changed = true;
                        }
                    }
                }
                self.send_control_message(
                    t,
                    &ControlMessageEnum::RequestOk(RequestOkMessage::new(
                        Some(cm.request_id),
                        None,
                    )),
                );
            }
            ControlMessageEnum::RequestOk(_cm) => {}
            ControlMessageEnum::PublishNamespaceDone(cm) => {
                match (cm.request_id(), cm.namespace()) {
                    (Some(rid), None) => {
                        self.received_namespaces.remove(&rid);
                    }
                    (None, Some(ns)) => {
                        self.received_namespaces.retain(|_, v| v != ns);
                    }
                    _ => {}
                }
            }
            ControlMessageEnum::PublishOk(cm) => {
                let request_id = req_id_override.unwrap_or_else(|| cm.request_id());
                if let Some(pom) =
                    self.pending_sent_publish_namespace.remove(&request_id)
                {
                    self.sent_namespaces.insert(request_id, pom.take_track_namespace());
                } else if self.pending_sent_publish.contains_key(&request_id) {
                    self.pending_publish_responses.insert(request_id, Ok(cm));
                }
            }
            ControlMessageEnum::Publish(cm) => {
                let request_id = cm.request_id;
                self.control_channel.record_request_stream_if_needed(stream_id, request_id);
                self.in_tracks
                    .entry(cm.track_alias)
                    .or_insert_with(|| InTrack::new(cm.track_alias));
                if let Some(streams) = self.pending_streams.remove(&cm.track_alias) {
                    if let Some(track) = self.in_tracks.get_mut(&cm.track_alias) {
                        for sid in streams {
                            track.mark_stream_readable(sid);
                        }
                    }
                }
                self.pending_received_publish.insert(request_id, cm);
            }
            _ => unimplemented!(),
        }
    }

    /// Must be called after upper-layer work following every `handle()`.
    pub fn post_handle(&mut self, t: &mut impl Transport) {
        self.dirty = false;
        if let Some(ps) = &mut self.out_padding_stream {
            ps.handle(t);
        }
    }

    /// Readable track aliases.
    pub fn readable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.in_tracks {
            if track.readable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// Writable track aliases.
    pub fn writable(&self) -> SmallVec<TrackAlias, 8> {
        let mut v = SmallVec::new();
        for (track_alias, track) in &self.out_tracks {
            if track.writable() {
                v.push(*track_alias);
            }
        }
        v
    }

    /// Send a MoQ object on a track. May open a new subgroup stream.
    pub fn send_obj(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.dirty = true;
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream_id = match track.current_stream_id {
            Some(v) => v,
            None => {
                let stream_id = t.open_uni_stream().unwrap();
                let track = self.out_tracks.get_mut(&track_alias).unwrap();
                track.current_stream_id = Some(stream_id);
                self.out_streams.insert(
                    stream_id,
                    OutStream::new(stream_id, track_alias, 0, 0, self.selected_version.unwrap()),
                );
                stream_id
            }
        };
        let stream = self.out_streams.get_mut(&stream_id).unwrap();
        stream.send_obj(t, buf)
    }

    /// Like `send_obj` but with explicit group/object IDs and custom extension headers.
    #[allow(clippy::too_many_arguments)]
    pub fn send_obj_with(
        &mut self,
        buf: &[u8],
        group_id: Option<u64>,
        object_id: Option<u64>,
        extension_headers: &KeyValuePairs,
        track_alias: TrackAlias,
        received_at: Option<Instant>,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.send_obj_hdr_with(
            group_id,
            None,
            object_id,
            buf.len(),
            extension_headers,
            track_alias,
            received_at,
            t,
        )?;
        let n = self.send_obj_pld(buf, track_alias, t)?;
        assert_eq!(n, buf.len());
        Ok(())
    }

    pub fn send_obj_hdr(
        &mut self,
        size: usize,
        track_alias: TrackAlias,
        received_at: Option<Instant>,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.send_obj_hdr_with(
            None,
            None,
            None,
            size,
            &KeyValuePairs::new(),
            track_alias,
            received_at,
            t,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_obj_hdr_with(
        &mut self,
        group_id: Option<u64>,
        subgroup_id: Option<u64>,
        object_id: Option<u64>,
        size: usize,
        extension_headers: &KeyValuePairs,
        track_alias: TrackAlias,
        received_at: Option<Instant>,
        t: &mut impl Transport,
    ) -> Result<()> {
        self.dirty = true;
        if let Some(received_at) = received_at {
            let elapsed = received_at.elapsed();
            let track = &self.out_tracks[&track_alias];
            let expired_delivery = track.delivery_timeout.is_some_and(|d| elapsed > d);
            let expired_cache = track.max_cache_duration.is_some_and(|d| elapsed > d);
            if expired_delivery {
                if let Some(sid) = self.out_tracks[&track_alias].current_stream_id {
                    t.reset_stream(sid, RESET_STREAM_CODE_DELIVERY_TIMEOUT as u64);
                    self.out_tracks.get_mut(&track_alias).unwrap().current_stream_id = None;
                }
                return Err(Error::ObjectExpired);
            }
            if expired_cache {
                return Err(Error::ObjectExpired);
            }
        }

        let version = self.selected_version.unwrap();

        let need_new_stream = if let Some(sid) = self.out_tracks[&track_alias].current_stream_id {
            let stream = &self.out_streams[&sid];
            let new_group = group_id.unwrap_or(stream.group_id());
            assert!(new_group >= stream.group_id(), "group_id must be non-decreasing");
            let new_subgroup = subgroup_id.unwrap_or(stream.subgroup_id());
            new_group > stream.group_id() || new_subgroup != stream.subgroup_id()
        } else {
            true
        };

        if need_new_stream {
            if let Some(old_sid) = self.out_tracks[&track_alias].current_stream_id {
                self.out_streams.get_mut(&old_sid).unwrap().fin(t);
                self.out_tracks.get_mut(&track_alias).unwrap().current_stream_id = None;
            }
            let eff_group = group_id.unwrap_or(0);
            let eff_subgroup = subgroup_id.unwrap_or(0);
            let stream_id = match t.open_uni_stream() {
                Ok(s) => s,
                Err(StreamError::InsufficientCapacity) => return Err(Error::InsufficientCapacity),
                Err(e) => unimplemented!("{:?}", e),
            };
            let track = self.out_tracks.get_mut(&track_alias).unwrap();
            track.current_stream_id = Some(stream_id);
            t.set_stream_priority(stream_id, track.urgency, track.incremental);
            self.out_streams.insert(
                stream_id,
                OutStream::new(stream_id, track_alias, eff_group, eff_subgroup, version),
            );
        }

        let stream_id = self.out_tracks[&track_alias].current_stream_id.unwrap();
        self.out_streams
            .get_mut(&stream_id)
            .unwrap()
            .send_obj_hdr(object_id, size, extension_headers, t)
    }

    pub fn send_obj_pld(
        &mut self,
        buf: &[u8],
        track_alias: TrackAlias,
        t: &mut impl Transport,
    ) -> Result<usize> {
        self.dirty = true;
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream = self.out_streams.get_mut(&track.current_stream_id.unwrap()).unwrap();
        stream.send_obj_pld(buf, t)
    }

    pub fn reset_current_track_stream(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        self.dirty = true;
        let Some(stream_id) = self.out_tracks[&track_alias].current_stream_id else { return };
        t.reset_stream(stream_id, RESET_STREAM_CODE_DELIVERY_TIMEOUT as u64);
        self.out_streams.remove(&stream_id);
        self.out_tracks.get_mut(&track_alias).unwrap().current_stream_id = None;
    }

    pub fn set_out_track_priority(
        &mut self,
        track_alias: TrackAlias,
        urgency: u8,
        incremental: bool,
        t: &mut impl Transport,
    ) {
        self.dirty = true;
        let Some(track) = self.out_tracks.get_mut(&track_alias) else { return };
        track.urgency = urgency;
        track.incremental = incremental;
        if let Some(stream_id) = track.current_stream_id {
            t.set_stream_priority(stream_id, urgency, incremental);
        }
    }

    pub fn pending_received_subscriptions(&self) -> &HashMap<RequestId, SubscribeMessage> {
        &self.pending_received_subscriptions
    }

    pub fn subscription_delivery_timeout(&self, track_alias: TrackAlias) -> Option<Duration> {
        self.out_tracks.get(&track_alias)?.delivery_timeout
    }

    pub fn process_subscription_requests<F>(
        &mut self,
        mut f: F,
        t: &mut impl Transport,
    ) where
        F: FnMut(&RequestId, &SubscribeMessage, TrackAlias) -> SubscriptionRequestAction,
    {
        self.dirty = true;
        let (s_iter, s_msg) = SplitOff::<
            partial!(MoqTransportSession mut pending_received_subscriptions, ! *),
        >::split_off_mut(self);
        s_iter.pending_received_subscriptions.retain(|id, sub| {
            let candidate_alias = if moq_wire::publisher_assigns_track_alias(s_msg.selected_version.unwrap()) {
                *s_msg.next_out_track_alias
            } else {
                sub.track_alias.unwrap()
            };
            match f(id, sub, candidate_alias) {
                SubscriptionRequestAction::Keep => true,
                SubscriptionRequestAction::Accept => {
                    Self::_accept_subscription(s_msg.as_mut(), sub, None, None, t);
                    false
                }
                SubscriptionRequestAction::Reject(error_code) => {
                    Self::_reject_subscription(s_msg.as_mut(), sub, error_code, t);
                    false
                }
            }
        });
    }

    #[allow(clippy::type_complexity)]
    pub fn _accept_subscription(
        s: &mut partial!(MoqTransportSession const selected_version config, mut next_out_track_alias out_tracks received_subscription_tracks control_channel, ! *),
        subscribe_message: &SubscribeMessage,
        largest_location: Option<Location>,
        max_cache_duration: Option<u64>,
        t: &mut impl Transport,
    ) -> TrackAlias {
        let (out_cm, track_alias) = if moq_wire::publisher_assigns_track_alias(s.selected_version.unwrap()) {
            let track_alias = *s.next_out_track_alias;
            *s.next_out_track_alias += 1;
            (
                ControlMessageEnum::SubscribeOk(SubscribeOkMessage::from(
                    subscribe_message,
                    Some(track_alias),
                    largest_location,
                    max_cache_duration,
                )),
                track_alias,
            )
        } else {
            (
                ControlMessageEnum::SubscribeOk(SubscribeOkMessage::from(
                    subscribe_message,
                    None,
                    largest_location,
                    max_cache_duration,
                )),
                subscribe_message.track_alias.unwrap(),
            )
        };
        let version = s.selected_version.unwrap();
        s.control_channel.send_response(subscribe_message.request_id, &out_cm, version, t);
        let delivery_timeout = subscribe_message.delivery_timeout().map(Duration::from_millis);
        let max_cache_duration = max_cache_duration.map(Duration::from_millis);
        s.out_tracks.insert(track_alias, OutTrack::new(delivery_timeout, max_cache_duration));
        s.received_subscription_tracks
            .insert(subscribe_message.request_id, track_alias);
        track_alias
    }

    pub fn accept_subscription(
        &mut self,
        request_id: RequestId,
        largest_location: Option<Location>,
        max_cache_duration: Option<u64>,
        t: &mut impl Transport,
    ) -> TrackAlias {
        self.dirty = true;
        let cm = self.pending_received_subscriptions.remove(&request_id).unwrap();
        Self::_accept_subscription(self.as_mut(), &cm, largest_location, max_cache_duration, t)
    }

    pub fn _reject_subscription(
        s: &mut partial!(MoqTransportSession const selected_version config, mut control_channel, ! *),
        subscribe_message: &SubscribeMessage,
        error_code: u64,
        t: &mut impl Transport,
    ) {
        let out_cm = ControlMessageEnum::RequestError(RequestErrorMessage::from(
            subscribe_message,
            error_code,
        ));
        let version = s.selected_version.unwrap();
        s.control_channel.send_response(subscribe_message.request_id, &out_cm, version, t);
    }

    pub fn reject_subscription(
        &mut self,
        request_id: RequestId,
        error_code: u64,
        t: &mut impl Transport,
    ) {
        self.dirty = true;
        let cm = self.pending_received_subscriptions.remove(&request_id).unwrap();
        Self::_reject_subscription(self.as_mut(), &cm, error_code, t);
    }

    pub fn publish_track(
        &mut self,
        namespace_trackname: NamespaceTrackname,
        parameters: Parameters,
        t: &mut impl Transport,
    ) -> Result<(RequestId, TrackAlias)> {
        self.dirty = true;
        let request_id = self.next_request_id;
        self.next_request_id += 2;
        let track_alias = self.next_out_track_alias;
        self.next_out_track_alias += 1;
        let cm = PublishMessage::new(request_id, namespace_trackname, track_alias, parameters);
        self.send_request(request_id, &ControlMessageEnum::Publish(cm.clone()), t);
        self.out_tracks.insert(track_alias, OutTrack::new(None, None));
        self.pending_sent_publish.insert(request_id, cm);
        Ok((request_id, track_alias))
    }

    pub fn poll_publish_response(
        &mut self,
        request_id: RequestId,
    ) -> Option<core::result::Result<PublishOkMessage, RequestErrorMessage>> {
        self.pending_publish_responses.remove(&request_id)
    }

    pub fn pending_received_track_publishes(&self) -> &HashMap<RequestId, PublishMessage> {
        &self.pending_received_publish
    }

    pub fn process_track_publish_requests<F>(
        &mut self,
        mut f: F,
        t: &mut impl Transport,
    ) where
        F: FnMut(&RequestId, &PublishMessage) -> PublishRequestAction,
    {
        self.dirty = true;
        let decisions: Vec<(RequestId, PublishRequestAction)> = self
            .pending_received_publish
            .iter()
            .map(|(id, cm)| (*id, f(id, cm)))
            .collect();
        for (request_id, action) in decisions {
            match action {
                PublishRequestAction::Keep => {}
                PublishRequestAction::Accept(parameters) => {
                    self.accept_track_publish(request_id, parameters, t)
                }
                PublishRequestAction::Reject(error_code) => {
                    self.reject_track_publish(request_id, error_code, t)
                }
            }
        }
    }

    pub fn accept_track_publish(
        &mut self,
        request_id: RequestId,
        parameters: Parameters,
        t: &mut impl Transport,
    ) {
        self.dirty = true;
        self.send_response(
            request_id,
            &ControlMessageEnum::PublishOk(PublishOkMessage::new(request_id, parameters)),
            t,
        );
        self.pending_received_publish.remove(&request_id);
    }

    pub fn reject_track_publish(
        &mut self,
        request_id: RequestId,
        error_code: ErrorCode,
        t: &mut impl Transport,
    ) {
        self.dirty = true;
        let Some(cm) = self.pending_received_publish.remove(&request_id) else { return };
        self.in_tracks.remove(&cm.track_alias);
        self.send_response(
            request_id,
            &ControlMessageEnum::RequestError(RequestErrorMessage::new(request_id, error_code)),
            t,
        );
    }

    pub fn pending_received_namespace_publishes(
        &self,
    ) -> &HashMap<RequestId, PublishNamespaceMessage> {
        &self.pending_received_publish_namespace
    }

    pub fn process_namespace_publish_requests<F>(
        &mut self,
        mut f: F,
        t: &mut impl Transport,
    ) where
        F: FnMut(&RequestId, &PublishNamespaceMessage) -> NamespacePublishRequestAction,
    {
        self.dirty = true;
        let decisions: Vec<(RequestId, NamespacePublishRequestAction)> = self
            .pending_received_publish_namespace
            .iter()
            .map(|(id, cm)| (*id, f(id, cm)))
            .collect();
        for (request_id, action) in decisions {
            match action {
                NamespacePublishRequestAction::Keep => {}
                NamespacePublishRequestAction::Accept => {
                    self.accept_namespace_publish(request_id, t)
                }
                NamespacePublishRequestAction::Reject(error_code) => {
                    self.reject_namespace_publish(request_id, error_code, t)
                }
            }
        }
    }

    pub fn accept_namespace_publish(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.dirty = true;
        let cm = self.pending_received_publish_namespace.get(&request_id).unwrap();
        let out_cm = match self.selected_version.unwrap() {
            // draft 07–10: ANNOUNCE_OK carries the namespace, not a Request ID
            // (which doesn't exist yet at this draft) — PublishOkMessage's wire
            // shape doesn't cover this range at all (see its `to_body_bytes`).
            MOQ_VERSION_DRAFT_07..=moq_wire::MOQ_VERSION_DRAFT_10 => {
                ControlMessageEnum::RequestOk(RequestOkMessage::new(None, Some(cm.track_namespace().clone())))
            }
            _ => ControlMessageEnum::PublishOk(PublishOkMessage::new(request_id, Parameters(vec![]))),
        };
        self.send_response(request_id, &out_cm, t);
        let cm = self.pending_received_publish_namespace.remove(&request_id).unwrap();
        self.received_namespaces.insert(request_id, cm.take_track_namespace());
    }

    pub fn reject_namespace_publish(
        &mut self,
        request_id: RequestId,
        error_code: ErrorCode,
        t: &mut impl Transport,
    ) {
        self.dirty = true;
        let Some(_cm) = self.pending_received_publish_namespace.remove(&request_id) else {
            return;
        };
        if matches!(self.selected_version.unwrap(), MOQ_VERSION_DRAFT_07..=0xff00000d /* draft-13 */) {
            // ANNOUNCE_ERROR was a message distinct from SUBSCRIBE_ERROR before
            // draft-14 unified them into REQUEST_ERROR (see RequestErrorMessage's
            // docstring); not implemented here.
            unimplemented!("rejecting a PUBLISH_NAMESPACE/ANNOUNCE is only supported from draft-14 (REQUEST_ERROR)");
        }
        self.send_response(
            request_id,
            &ControlMessageEnum::RequestError(RequestErrorMessage::new(request_id, error_code)),
            t,
        );
    }

    pub fn remaining_object_payload(&self, track_alias: TrackAlias) -> Result<usize> {
        let track = self.in_tracks.get(&track_alias).unwrap();
        let stream_id = track.current_stream().unwrap();
        Ok(self.in_streams.get(&stream_id).unwrap().remaining_object_payload())
    }

    pub fn readable_streams(&self, track_alias: TrackAlias) -> &[u64] {
        self.in_tracks.get(&track_alias).unwrap().readable_streams()
    }

    pub fn subgroup_header(&self, track_alias: TrackAlias) -> Option<&SubgroupHeader> {
        let track = self.in_tracks.get(&track_alias)?;
        let stream_id = track.current_stream()?;
        self.in_streams.get(&stream_id)?.subgroup_header()
    }

    pub fn read_obj_hdr(
        &mut self,
        track_alias: TrackAlias,
        t: &mut impl Transport,
    ) -> Result<ObjectHeader> {
        self.dirty = true;
        let Some(track) = self.in_tracks.get_mut(&track_alias) else {
            return Err(Error::Done);
        };
        loop {
            let stream_id = match track.current_stream() {
                Some(id) => id,
                None if track.is_fully_done() => return Err(Error::Fin),
                None => return Err(Error::Done),
            };
            let stream = self.in_streams.get_mut(&stream_id).unwrap();
            match stream.read_obj_hdr(t) {
                Ok(v) => return Ok(v),
                Err(Error::Fin) => {
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
        t: &mut impl Transport,
    ) -> Result<usize> {
        self.dirty = true;
        let track = self.in_tracks.get_mut(&track_alias).unwrap();
        let stream_id = track.current_stream().unwrap();
        let stream = self.in_streams.get_mut(&stream_id).unwrap();
        match stream.read_obj_pld(t, buf) {
            Err(Error::Fin) => {
                self.in_streams.remove(&stream_id);
                let track = self.in_tracks.get_mut(&track_alias).unwrap();
                track.fin_stream(stream_id);
                Err(Error::Fin)
            }
            other => other,
        }
    }

    pub fn cancel_current_in_stream(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        self.dirty = true;
        let Some(track) = self.in_tracks.get_mut(&track_alias) else { return };
        let Some(stream_id) = track.current_stream() else { return };
        t.stop_sending(stream_id, RESET_STREAM_CODE_CANCELED as u64);
        self.in_streams.remove(&stream_id);
        track.fin_stream(stream_id);
    }

    pub fn timeout_stream(&mut self, track_alias: TrackAlias, t: &mut impl Transport) {
        self.dirty = true;
        let track = self.out_tracks.get_mut(&track_alias).unwrap();
        let stream_id = track.current_stream_id.unwrap();
        t.reset_stream(stream_id, RESET_STREAM_CODE_DELIVERY_TIMEOUT as u64);
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
        namespace: Vec<Vec<u8>>,
        t: &mut impl Transport,
    ) -> Result<RequestId> {
        self.dirty = true;
        let request_id = self.next_request_id;
        let cm = ControlMessageEnum::PublishNamespace(PublishNamespaceMessage::new(
            Some(request_id),
            Namespace(Tuple(namespace)),
            Parameters(vec![]),
        ));
        self.send_request(request_id, &cm, t);
        let ControlMessageEnum::PublishNamespace(cm) = cm else { unreachable!() };
        self.pending_sent_publish_namespace.insert(request_id, cm);
        self.next_request_id += 2;
        Ok(request_id)
    }

    pub fn publish_done(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.dirty = true;
        self.send_control_message(
            t,
            &ControlMessageEnum::PublishDone(PublishDoneMessage::new(request_id, 0)),
        )
    }

    pub fn publish_namespace_done(&mut self, request_id: RequestId, t: &mut impl Transport) {
        self.dirty = true;
        let Some(namespace) = self.sent_namespaces.remove(&request_id) else { return };
        self.send_control_message(
            t,
            &ControlMessageEnum::PublishNamespaceDone(PublishNamespaceDoneMessage::new(
                Some(request_id),
                Some(namespace),
            )),
        )
    }

    pub fn publish_namespace_status(&self, request_id: RequestId) -> PublishStatus {
        if self.sent_namespaces.contains_key(&request_id) {
            PublishStatus::Accepted
        } else if self.pending_sent_publish_namespace.contains_key(&request_id) {
            PublishStatus::Pending
        } else {
            PublishStatus::Unknown
        }
    }

    pub fn start_padding_stream(&mut self) {
        self.dirty = true;
        if self.out_padding_stream.is_some() {
            return;
        }
        self.out_padding_stream = Some(OutPaddingStream::new());
    }

    pub fn stop_padding_stream(&mut self, t: &mut impl Transport) {
        self.dirty = true;
        if let Some(ps) = self.out_padding_stream.take() {
            ps.stop(t);
        }
    }

    pub fn padding_stream_running(&self) -> bool {
        self.out_padding_stream.is_some()
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

pub enum PublishRequestAction {
    Keep,
    Accept(Parameters),
    Reject(ErrorCode),
}

pub enum NamespacePublishRequestAction {
    Keep,
    Accept,
    Reject(ErrorCode),
}
