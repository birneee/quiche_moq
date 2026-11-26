use crate::prelude::*;
use crate::config::{Config, VersionNegotiation};
use crate::control_channel::ControlChannel;
use crate::error::Error;
use crate::error::Result;
use crate::in_stream::{Handled, InStream};
use crate::in_track::{InTrack, ReceivedDatagram};
use crate::out_stream::OutStream;
use crate::out_track::{DeliveryTimeouts, OutTrack};
use crate::padding_stream::OutPaddingStream;
use crate::pending_subscribe::PendingSubscribe;
use crate::transport::{StreamError, Transport, object_urgency};
use crate::handles::{Announcement, FetchResponse, Fetching, NamespaceSubscription, Publication, Subscription, TracksSubscription};
use crate::request::{DropQueue, Dropped, DroppedKind, IncomingRequest, LocalDropQueue, Obligation, Request, RequestKind, SubscribeOk, sealed};
use log::{debug, error, trace};
use moq_wire::control_message::subscribe::{SubscribeMessage, LocationFilter};
use moq_wire::control_message::{ClientSetupMessage, ControlMessageEnum, RequestRef, TrackStatusErrorMessage, TrackStatusMessage, TrackStatusOkMessage, MaxRequestIdMessage, PublishDoneMessage, PublishMessage, PublishNamespaceDoneMessage, PublishNamespaceErrorMessage, PublishNamespaceMessage, PublishOkMessage, RequestErrorMessage, RequestOkMessage, RequestUpdateMessage, ServerSetupMessage, SetupMessage, SubscribeOkMessage, UnsubscribeMessage};
use moq_wire::datagram::ObjectDatagram;
use moq_wire::object::{ObjectHeader, ObjectStatus};
use moq_wire::range_filter::{ObjectFilter, RangeFilters};
use moq_wire::{PublishDoneStatus, DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER, KeyValuePair, KeyValuePairs, Location, SessionCloseCode, MOQ_VERSION_DRAFT_11, RequestErrorCode, Namespace, NamespaceTrackname, ResetCode, RequestId, Role, DEFAULT_SUBSCRIBER_PRIORITY, SetupOptions, SubgroupLocation, TrackAlias, GroupOrder, Version};
use smallvec::SmallVec;
use alloc::collections::{BTreeSet, VecDeque};
use hashbrown::{HashMap, HashSet};

use core::ops::ControlFlow;
use core::time::Duration;
use crate::time::Timestamp;

const INITIAL_CLIENT_REQUEST_ID: RequestId = RequestId(0);
const INITIAL_SERVER_REQUEST_ID: RequestId = RequestId(1);

/// What the peer did, in arrival order, see `MoqTransportSession::next_event`.
pub enum Event<Q: DropQueue = LocalDropQueue> {
    /// A request to answer.
    Request(IncomingRequest<Q>),
    /// The peer withdrew a request handed out before it was answered,
    /// answering it returns `Error::RequestCancelled`.
    RequestCancelled(RequestId),
    /// Our SUBSCRIBE was accepted.
    SubscribeOk { request_id: RequestId, track_alias: TrackAlias, message: SubscribeOkMessage },
    /// Our PUBLISH was accepted.
    PublishOk { request_id: RequestId, message: PublishOkMessage },
    /// Our PUBLISH_NAMESPACE was accepted.
    PublishNamespaceOk { request_id: RequestId },
    /// The peer withdrew a PUBLISH_NAMESPACE we accepted (draft-ietf-moq-transport-18 §8).
    NamespaceWithdrawn { request_id: RequestId, namespace: Namespace },
    /// The peer cancelled a SUBSCRIBE_NAMESPACE or SUBSCRIBE_TRACKS we accepted: sending on its handle fails.
    PrefixSubscriptionEnded { request_id: RequestId },
    /// Our FETCH was accepted, its objects end before `end_location`.
    FetchOk { request_id: RequestId, end_of_track: bool, end_location: Location },
    /// The subscriber cancelled a FETCH we answer: writing to its `FetchResponse` fails with `TrackEnded`.
    FetchCancelled { request_id: RequestId },
    /// Our TRACK_STATUS was answered: what a SUBSCRIBE_OK would name.
    TrackStatus { request_id: RequestId, largest_location: Option<Location>, track_properties: KeyValuePairs },
    /// Our REQUEST_UPDATE was accepted, with the track's LARGEST_OBJECT.
    RequestUpdateOk { request_id: RequestId, largest_location: Option<Location> },
    /// One of our requests failed, its state is gone.
    RequestFailed { request_id: RequestId, failure: RequestFailure },
    /// The subscriber ended one of our egress tracks: its undelivered streams
    /// are reset, `send_obj_hdr` fails with `TrackEnded`.
    Unsubscribed { request_id: RequestId, track_alias: TrackAlias },
    /// A REQUEST_UPDATE changed an egress track's FORWARD state and/or
    /// SUBSCRIPTION_FILTER (draft-16 §9.2.2.5/.8), e.g. a `#race` promotion.
    SubscriptionUpdated { request_id: RequestId, forward: Option<bool>, filter: Option<LocationFilter> },
}

/// Why one of our requests failed.
#[derive(Debug)]
pub enum RequestFailure {
    /// REQUEST_ERROR, or before draft 15 the error message of its request
    Rejected(RequestErrorMessage),
    /// From draft 17 cancelled by the peer without REQUEST_ERROR (draft-ietf-moq-transport-19 §3.3.3)
    Cancelled,
}

impl core::fmt::Display for RequestFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rejected(error) => error.fmt(f),
            Self::Cancelled => f.write_str("cancelled by the peer"),
        }
    }
}

/// What the session keeps of an incoming request until it is answered, see `Request`.
enum Unanswered {
    /// drafts 7–11 name the alias in SUBSCRIBE_ERROR
    Subscribe { track_alias: Option<TrackAlias> },
    Publish { track_alias: TrackAlias },
    /// drafts 7–10 end it by namespace
    PublishNamespace { namespace: Namespace },
    /// answered on the stream of the request it updates
    RequestUpdate { existing_request_id: RequestId },
    /// drafts 7 to 10 answer by the track's name
    TrackStatus { namespace_trackname: NamespaceTrackname },
    Fetch,
    SubscribeNamespace,
    SubscribeTracks,
}

/// What a session has open right now, see `MoqTransportSession::stats`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    pub received_tracks: usize,
    pub sent_tracks: usize,
    /// subgroup and fetch streams, both directions
    pub data_streams: usize,
}

pub struct MoqTransportSession<Q: DropQueue = LocalDropQueue> {
    server: bool,
    /// Owns the shared control channel and, for draft 17+, the per-request
    /// bidi streams — see `crate::control_channel`. Session code never has to
    /// reason about topology (bidi vs. uni-pair) itself; it just calls this.
    control_channel: ControlChannel,
    /// `None` until the setup handshake completes.
    pub(crate) selected_version: Option<Version>,
    next_request_id: RequestId,
    /// The peer's lowest Request ID not received yet
    next_expected_request_id: RequestId,
    /// From draft 17: the peer's Request IDs received above `next_expected_request_id`, its requests' streams may arrive in any order
    peer_request_ids_ahead: BTreeSet<RequestId>,
    /// IDs handed out by `local_request_id`
    local_request_ids: u64,
    /// Readable streams that arrived before SETUP, read once it completed
    deferred_streams: Vec<u64>,
    /// Max request_id we are allowed to send.
    max_request_id: RequestId,
    /// Max request_id we allow the peer to send.
    out_max_request_id: RequestId,
    pub(crate) in_streams: HashMap<u64, InStream>,
    in_tracks: HashMap<TrackAlias, InTrack>,
    /// Removed on UNSUBSCRIBE, or after PUBLISH_DONE once their streams are done.
    pub(crate) out_tracks: HashMap<TrackAlias, OutTrack>,
    /// Only used by draft ≥ 12.
    next_out_track_alias: TrackAlias,
    /// Active subscriptions: request_id → track_alias. Routes PUBLISH_DONE to the right track.
    active_subscriptions: HashMap<RequestId, TrackAlias>,
    pending_subscribe: HashMap<RequestId, PendingSubscribe>,
    /// Our unanswered TRACK_STATUS
    pending_track_status: HashSet<RequestId>,
    /// Our FETCHes
    fetches: HashMap<RequestId, fetches::InFetch>,
    /// The streams answering the peer's FETCHes
    out_fetches: HashMap<RequestId, fetches::OutFetch>,
    /// Answered FETCHes whose stream still sends what is pending before its FIN
    finishing_fetches: Vec<fetches::OutFetch>,
    /// Our subscriptions' current options, which SUBSCRIBE_UPDATE repeats in full before draft 15
    subscription_options: HashMap<RequestId, SubscribeOptions>,
    /// Streams that arrived before their SUBSCRIBE_OK or PUBLISH, by track_alias,
    /// with when the first arrived. Stopped after `STRAY_STREAM_TIMEOUT`.
    pending_streams: HashMap<TrackAlias, (Timestamp, Vec<u64>)>,
    /// Objects of datagrams that arrived before their SUBSCRIBE_OK or PUBLISH, likewise
    /// (draft-ietf-moq-transport-18 §11.3), see `park_datagram`.
    pending_datagrams: HashMap<TrackAlias, (Timestamp, VecDeque<ReceivedDatagram>)>,
    /// Streams of tracks no longer read, drained until they end: the
    /// transport ignores STOP_SENDING and keeps reporting them readable.
    dropped_in_streams: HashSet<u64>,
    /// Aliases of tracks no longer read, with when: streams arriving for them
    /// later are stopped right away. Forgotten after `STRAY_STREAM_TIMEOUT`,
    /// a later stream waits in `pending_streams` instead.
    dropped_aliases: HashMap<TrackAlias, Timestamp>,
    /// Our egress tracks by Request ID: accepted SUBSCRIBEs and sent PUBLISHes.
    out_requests: HashMap<RequestId, TrackAlias>,
    pending_sent_publish: HashMap<RequestId, PublishMessage>,
    pub(crate) out_streams: HashMap<u64, OutStream>,
    config: Config,
    closed: bool,
    /// Namespaces we announced and that were accepted by the peer.
    sent_namespaces: HashMap<RequestId, Namespace>,
    /// Namespaces the peer announced and that we accepted.
    received_namespaces: HashMap<RequestId, Namespace>,
    /// The peer's SUBSCRIBE_NAMESPACEs and SUBSCRIBE_TRACKS we accepted, until either side ends them.
    prefix_subscriptions: HashSet<RequestId>,
    pending_sent_publish_namespace: HashMap<RequestId, PublishNamespaceMessage>,
    out_padding_stream: Option<OutPaddingStream>,
    /// REQUEST_UPDATEs sent for an existing subscription (e.g.
    /// `request_update_forward`), awaiting their REQUEST_OK/REQUEST_ERROR —
    /// keyed by the REQUEST_UPDATE's own Request ID, not the subscription's.
    pending_request_updates: HashSet<RequestId>,
    unanswered_requests: HashMap<RequestId, Unanswered>,
    /// Not yet handed out by `next_event`.
    events: VecDeque<Event<Q>>,
    /// Values dropped unresolved, ended by the next `handle()`.
    dropped: Q,
    /// The `now` of the last `handle()`.
    now: Timestamp,
    /// When the open-subgroup sweep in `handle()` last ran.
    /// - rate-limited to `STREAM_SWEEP_INTERVAL`: unlimited ticks would make it O(conns x open streams) for no benefit
    last_stream_sweep: Option<Timestamp>,
    /// Earliest open-subgroup expiry as of the last sweep; backs `timeout()`.
    /// - lags reality by at most `STREAM_SWEEP_INTERVAL`, well below real deadline values
    earliest_deadline: Option<Timestamp>,
}

/// How long streams of an unknown track alias wait for their track
/// (SUBSCRIBE_OK or PUBLISH), and a dropped alias is remembered.
const STRAY_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum spacing between `handle()`'s open-subgroup sweeps.
/// - caps sweep rate at 200/s; real delivery_timeout/max_cache_duration values are tens of ms+, so the lag is not observable
const STREAM_SWEEP_INTERVAL: Duration = Duration::from_millis(5);

/// From draft 17: how many of the peer's Request IDs may arrive ahead of a missing one.
const MAX_PEER_REQUEST_IDS_AHEAD: usize = 1024;

/// One event from `MoqTransportSession::recv_new_obj_hdr`.
#[derive(Debug)]
pub enum RecvObjHdr {
    /// A new object's header, at `location` on Subgroup `subgroup_id` of `publisher_priority`,
    /// `end_of_group` if the Subgroup holds the Group's largest Object.
    Header { location: Location, subgroup_id: u64, publisher_priority: u8, end_of_group: bool, header: ObjectHeader },
    /// An Object received whole in a datagram (draft-ietf-moq-transport-18 §11.3): `publisher_priority` the
    /// track's default if the datagram has none, `status` Normal for one with payload, `end_of_group` if
    /// it is its Group's last Object.
    Datagram { location: Location, publisher_priority: u8, end_of_group: bool, status: ObjectStatus, properties: KeyValuePairs, payload: Vec<u8> },
    /// The Subgroup's stream just finished cleanly (all its Objects
    /// delivered) — `location` is the last Object it carried.
    SubgroupFinished { location: Location, subgroup_id: u64 },
    /// The Subgroup's stream was reset after `location`, its last complete
    /// Object: more Objects may exist (draft-16 §10.4.3).
    SubgroupReset { location: Location, subgroup_id: u64 },
}

impl<Q: DropQueue> MoqTransportSession<Q> {
    /// Connect to server. Opens the control channel and sends CLIENT_SETUP.
    ///
    /// Fails if `config.version_negotiation` can't actually be conveyed on
    /// the wire (see `VersionNegotiation::validate`).
    pub fn connect(t: &mut impl Transport, config: Config) -> crate::error::Result<Self> {
        config.version_negotiation.validate()?;
        let control_channel = ControlChannel::connect(t, config.version_negotiation.preferred())?;
        let mut s = Self {
            server: false,
            control_channel,
            selected_version: None,
            next_request_id: INITIAL_CLIENT_REQUEST_ID,
            next_expected_request_id: INITIAL_SERVER_REQUEST_ID,
            peer_request_ids_ahead: BTreeSet::new(),
            local_request_ids: 0,
            deferred_streams: Vec::new(),
            max_request_id: RequestId(0),
            out_max_request_id: RequestId(config.out_max_request_id),
            in_streams: HashMap::default(),
            in_tracks: HashMap::default(),
            out_tracks: HashMap::default(),
            next_out_track_alias: TrackAlias(0),
            active_subscriptions: HashMap::new(),
            pending_subscribe: HashMap::new(),
            pending_track_status: HashSet::new(),
            fetches: HashMap::new(),
            out_fetches: HashMap::new(),
            finishing_fetches: Vec::new(),
            subscription_options: HashMap::new(),
            pending_streams: HashMap::default(),
            pending_datagrams: HashMap::default(),
            dropped_in_streams: HashSet::new(),
            dropped_aliases: HashMap::default(),
            out_requests: HashMap::new(),
            pending_sent_publish: HashMap::new(),
            out_streams: HashMap::default(),
            config: config.clone(),
            closed: false,
            sent_namespaces: HashMap::new(),
            received_namespaces: HashMap::new(),
            prefix_subscriptions: HashSet::new(),
            pending_sent_publish_namespace: HashMap::new(),
            out_padding_stream: None,
            pending_request_updates: HashSet::new(),
            unanswered_requests: HashMap::new(),
            events: VecDeque::new(),
            dropped: Q::default(),
            now: Timestamp::default(),
            last_stream_sweep: None,
            earliest_deadline: None,
        };
        s.apply_config();
        // `Pinned`: nothing to offer, the version was already settled below
        // this layer — see `ClientSetupMessage::to_body_bytes`, which drops
        // this field from the wire entirely for draft >= 15 regardless.
        let supported_versions = match &config.version_negotiation {
            VersionNegotiation::InBand { candidates } => candidates.iter().map(|v| v.to_wire()).collect(),
            VersionNegotiation::Pinned(_) => vec![],
        };
        let setup = match s.effective_version().draft() {
            ..=16 => ControlMessageEnum::ClientSetup(ClientSetupMessage { supported_versions, options: s.setup_options() }),
            _ => ControlMessageEnum::Setup(SetupMessage { options: s.setup_options() }),
        };
        s.send_control_message(t, &setup);
        Ok(s)
    }

    /// Accept an incoming client connection (server side). No I/O performed here;
    /// call `handle()` once packets arrive.
    ///
    /// Fails if `config.version_negotiation` can't actually be conveyed on
    /// the wire (see `VersionNegotiation::validate`).
    pub fn accept(config: Config) -> crate::error::Result<Self> {
        config.version_negotiation.validate()?;
        let mut s = Self {
            server: true,
            control_channel: ControlChannel::accept(config.version_negotiation.preferred()),
            selected_version: None,
            next_request_id: INITIAL_SERVER_REQUEST_ID,
            next_expected_request_id: INITIAL_CLIENT_REQUEST_ID,
            peer_request_ids_ahead: BTreeSet::new(),
            local_request_ids: 0,
            deferred_streams: Vec::new(),
            max_request_id: RequestId(0),
            out_max_request_id: RequestId(config.out_max_request_id),
            in_streams: HashMap::default(),
            in_tracks: HashMap::default(),
            out_tracks: HashMap::default(),
            next_out_track_alias: TrackAlias(0),
            active_subscriptions: HashMap::new(),
            pending_subscribe: HashMap::new(),
            pending_track_status: HashSet::new(),
            fetches: HashMap::new(),
            out_fetches: HashMap::new(),
            finishing_fetches: Vec::new(),
            subscription_options: HashMap::new(),
            pending_streams: HashMap::default(),
            pending_datagrams: HashMap::default(),
            dropped_in_streams: HashSet::new(),
            dropped_aliases: HashMap::default(),
            out_requests: HashMap::new(),
            pending_sent_publish: HashMap::new(),
            out_streams: HashMap::default(),
            config,
            closed: false,
            sent_namespaces: HashMap::new(),
            received_namespaces: HashMap::new(),
            prefix_subscriptions: HashSet::new(),
            pending_sent_publish_namespace: HashMap::new(),
            out_padding_stream: None,
            pending_request_updates: HashSet::new(),
            unanswered_requests: HashMap::new(),
            events: VecDeque::new(),
            dropped: Q::default(),
            now: Timestamp::default(),
            last_stream_sweep: None,
            earliest_deadline: None,
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
            self.out_padding_stream = Some(OutPaddingStream::new(self.config.padding_stream_max_bytes_per_sec, self.now));
        }
    }

    /// Sends a session-level message (not tied to any one request) on the
    /// shared control channel — SETUP, REQUEST_UPDATE, PUBLISH_DONE, etc.
    fn send_control_message(&mut self, t: &mut impl Transport, cm: &ControlMessageEnum) {
        let version = self.effective_version();
        if let Err(e) = self.control_channel.send_control(cm, version, t) {
            self.control_stream_failed(e, t);
        }
    }

    /// The control stream must stay writable for the whole session.
    fn control_stream_failed(&mut self, e: Error, t: &mut impl Transport) {
        let code = match e {
            // our message, not the peer's fault
            Error::Wire(moq_wire::Error::Unencodable(_) | moq_wire::Error::Unsupported(_)) => SessionCloseCode::InternalError,
            _ => SessionCloseCode::ProtocolViolation,
        };
        let _ = self.close_session(code, &format!("control stream not writable: {e:?}"), t);
    }

    pub fn stats(&self) -> SessionStats {
        SessionStats {
            received_tracks: self.in_tracks.len(),
            sent_tracks: self.out_tracks.len(),
            data_streams: self.in_streams.len() + self.out_streams.len() + self.out_fetches.len() + self.finishing_fetches.len(),
        }
    }

    /// Whether the session ended, by us or the peer.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// `Error::SessionClosed` once the session is closed.
    fn ensure_open(&self) -> Result<()> {
        if self.closed { Err(Error::SessionClosed) } else { Ok(()) }
    }

    /// Sends a request-initiating message (SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE),
    /// letting the control channel decide (from the negotiated topology) whether
    /// it needs its own dedicated stream (draft 17+) or just the shared one.
    /// `Done` or `InsufficientCapacity` while no request stream can be opened (draft 17+).
    fn send_request(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) -> Result<()> {
        let version = self.effective_version();
        match self.control_channel.send_request(request_id, cm, version, t) {
            Ok(()) => Ok(()),
            Err(e @ (Error::Done | Error::InsufficientCapacity)) => Err(e),
            Err(e) => {
                self.control_stream_failed(e, t);
                Err(Error::SessionClosed)
            }
        }
    }

    /// Sends a response (SUBSCRIBE_OK/PUBLISH_OK/REQUEST_ERROR/...) for
    /// `request_id`, letting the control channel route it to the request's own
    /// stream if it has one (draft 17+) or the shared channel otherwise.
    fn send_response(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) {
        let version = self.effective_version();
        if let Err(e) = self.control_channel.send_response(request_id, cm, version, t) {
            self.control_stream_failed(e, t);
        }
    }

    /// The last message of a request: on draft 17+ its stream ends after it.
    fn send_final_response(&mut self, request_id: RequestId, cm: &ControlMessageEnum, t: &mut impl Transport) {
        self.send_response(request_id, cm, t);
        self.control_channel.end_request_stream(request_id, t);
    }

    /// Process new data from the transport layer.
    ///
    /// `readable` is a slice of stream IDs that have data available; the
    /// caller is responsible for collecting this from the transport before
    /// calling `handle()` (e.g. via `wt.readable_streams()` in quiche_moq, or
    /// a queue of arrived stream events in a browser WebTransport wrapper).
    ///
    /// `now` is the current time, calls until the next `handle()` use it too.
    pub fn handle(&mut self, t: &mut impl Transport, readable: &[u64], now: Timestamp) {
        self.now = now;
        trace!("poll moq");

        if self.closed {
            return;
        }

        let opening = self.control_channel.out_stream().is_none();
        if !self.control_channel.try_establish(readable, t) {
            // none is the control stream, they wait for it
            for &stream_id in readable {
                self.defer_stream(stream_id);
            }
            return;
        }
        // draft 17+: the server opens its control stream with SETUP, not waiting for the client's (draft-ietf-moq-transport-18 §3.3)
        if opening && self.control_channel.uses_per_request_streams() {
            self.send_control_message(t, &ControlMessageEnum::Setup(SetupMessage { options: self.setup_options() }));
        }
        self.resolve_dropped(t);
        if let Err(e) = self.control_channel.flush(t) {
            self.control_stream_failed(e, t);
            return;
        }
        self.flush_finishing_fetches(t);

        for &stream_id in readable {
            if self.process_readable(stream_id, t).is_break() {
                return;
            }
        }
        if self.selected_version.is_some() {
            for stream_id in core::mem::take(&mut self.deferred_streams) {
                if self.process_readable(stream_id, t).is_break() {
                    return;
                }
            }
            if self.recv_datagrams(t).is_break() {
                return;
            }
        }
        // after reading: a peer may stop our side along with its answer, the answer counts (draft-ietf-moq-transport-18 §3.3.2)
        for request_id in self.control_channel.take_stopped_requests() {
            self.peer_cancelled_request(request_id, t);
        }

        // Sweeps the open outgoing subgroups, FIN'd ones too: a FIN means no
        // more objects, not delivered.
        // - delivered: forgotten, expired: reset
        // - rate-limited to `STREAM_SWEEP_INTERVAL`, it is O(open subgroup streams)
        // - expiry: see `OutStream::deadline`
        let now = self.now;
        let sweep_due = self
            .last_stream_sweep
            .is_none_or(|last| now.saturating_duration_since(last) >= STREAM_SWEEP_INTERVAL);
        enum Disposition { Finished, Expired }
        let mut done: Vec<(TrackAlias, SubgroupLocation, u64, Disposition)> = Vec::new();
        // Earliest not-yet-reached expiry this sweep — cached so `timeout()`
        // (once per connection per tick) is O(1) instead of rescanning.
        let mut earliest_deadline: Option<Timestamp> = None;
        for (&track_alias, track) in self.out_tracks.iter().filter(|_| sweep_due) {
            for (&sg, stream_ids) in &track.open_streams {
                for &stream_id in stream_ids {
                    if t.stream_finished(stream_id) {
                        done.push((track_alias, sg, stream_id, Disposition::Finished));
                        continue;
                    }
                    let Some(deadline) = self.out_streams.get(&stream_id).and_then(|s| s.deadline(track.expiry_timeout(), track.fin_timeout())) else { continue };
                    if now > deadline {
                        done.push((track_alias, sg, stream_id, Disposition::Expired));
                    } else {
                        earliest_deadline =
                            Some(earliest_deadline.map_or(deadline, |e: Timestamp| e.min(deadline)));
                    }
                }
            }
        }
        if sweep_due {
            self.last_stream_sweep = Some(now);
            self.earliest_deadline = earliest_deadline;
        }
        for (track_alias, sg, stream_id, disposition) in done {
            if let Some(tr) = self.out_tracks.get_mut(&track_alias)
                && let Some(stream_ids) = tr.open_streams.get_mut(&sg)
            {
                stream_ids.retain(|&sid| sid != stream_id);
                if stream_ids.is_empty() {
                    tr.open_streams.remove(&sg);
                }
            }
            match disposition {
                Disposition::Finished => {
                    self.out_streams.remove(&stream_id);
                }
                Disposition::Expired => {
                    t.reset_stream(stream_id, ResetCode::DeliveryTimeout);
                    self.out_streams.remove(&stream_id);
                }
            }
        }
        if sweep_due {
            self.out_tracks.retain(|_, tr| !(tr.ended && tr.open_streams.is_empty()));
            self.expire_stray_streams(now, t);
        }
    }

    /// The next thing the peer did, `None` until `handle()` receives more.
    pub fn next_event(&mut self) -> Option<Event<Q>> {
        self.events.pop_front()
    }

    /// Woken whenever a request or handle of this session is dropped
    /// unresolved: `handle()` must run to resolve it.
    pub fn set_drop_waker(&self, waker: core::task::Waker) {
        self.dropped.set_waker(waker);
    }

    /// When `handle()` must run again even without I/O, `None` if nothing is
    /// pending: subgroup expiry (`delivery_timeout`/`max_cache_duration`),
    /// PUBLISH_DONE and stray stream timeouts, padding rate limit.
    pub fn timeout_at(&self) -> Option<Timestamp> {
        let padding = self.out_padding_stream.as_ref().and_then(|p| p.next_write_at());
        let publish_done = self.in_tracks.values().filter_map(InTrack::done_deadline).min();
        let stray = self.pending_streams.values().map(|(since, _)| *since)
            .chain(self.pending_datagrams.values().map(|(since, _)| *since))
            .map(|since| since + STRAY_STREAM_TIMEOUT);
        let sweep = self.earliest_deadline.into_iter().chain(stray).min().map(|deadline| self.sweep_at(deadline));
        [padding, publish_done, sweep].into_iter().flatten().min()
    }

    /// When the sweep in `handle()` acts on `deadline`: not before the rate
    /// limit lets it run again, else the event loop would spin until then.
    /// - O(1) for the stream expiries: `earliest_deadline` is cached by the sweep,
    ///   rescanning every open subgroup per connection per tick was quadratic
    fn sweep_at(&self, deadline: Timestamp) -> Timestamp {
        self.last_stream_sweep.map_or(deadline, |last| deadline.max(last + STREAM_SWEEP_INTERVAL))
    }

    /// Reads readable `stream_id`, `Break` once the session closed.
    /// - streams other than the control stream wait for SETUP (draft-ietf-moq-transport-19 §3.3)
    fn process_readable(&mut self, stream_id: u64, t: &mut impl Transport) -> ControlFlow<()> {
        if self.dropped_in_streams.contains(&stream_id) {
            self.discard_dropped_stream(stream_id, t);
            return ControlFlow::Continue(());
        }
        if self.selected_version.is_none() && !self.control_channel.is_control_stream(stream_id) {
            self.defer_stream(stream_id);
            return ControlFlow::Continue(());
        }
        // the control stream or, from draft 17, a request stream
        if self.control_channel.owns(stream_id) {
            return self.process_stream_message(stream_id, t);
        }
        self.process_data_stream(stream_id, t)
    }

    /// Reads readable `stream_id` once SETUP completed.
    fn defer_stream(&mut self, stream_id: u64) {
        if !self.deferred_streams.contains(&stream_id) {
            self.deferred_streams.push(stream_id);
        }
    }

    /// Reads the next object header of data stream `stream_id`, `Break` once the session closed.
    fn process_data_stream(&mut self, stream_id: u64, t: &mut impl Transport) -> ControlFlow<()> {
        let Some(version) = self.selected_version else { return ControlFlow::Continue(()) };
        let control_channel = &mut self.control_channel;
        let stream = self.in_streams.entry(stream_id).or_insert_with(|| match control_channel.take_data_stream_first_byte(stream_id) {
            Some(byte) => InStream::with_first_byte(stream_id, version, byte),
            None => InStream::new(stream_id, version),
        });
        let track_alias = match stream.handle(t) {
            Ok(Handled::Subgroup(h)) => h.track_alias(),
            Ok(Handled::Fetch(request_id)) => {
                self.attach_fetch_stream(stream_id, request_id, t);
                return ControlFlow::Continue(());
            }
            Ok(Handled::Padding) | Err(Error::Done) => return ControlFlow::Continue(()),
            Err(Error::Fin) | Err(Error::Transport(StreamError::StreamReset(_) | StreamError::InvalidStreamState(_))) => {
                self.in_streams.remove(&stream_id);
                return ControlFlow::Continue(());
            }
            Err(Error::Wire(e)) => {
                return self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed data stream {stream_id}: {e:?}"), t);
            }
            Err(e) => {
                error!("dropping data stream {stream_id}: {e:?}");
                self.in_streams.remove(&stream_id);
                return ControlFlow::Continue(());
            }
        };
        match self.in_tracks.get_mut(&track_alias) {
            Some(track) => track.mark_stream_readable(stream_id),
            None if self.dropped_aliases.contains_key(&track_alias) => {
                self.in_streams.remove(&stream_id);
                self.stop_in_stream(stream_id, t);
            }
            None => {
                let now = self.now;
                let (_, pending) = self.pending_streams.entry(track_alias).or_insert_with(|| (now, Vec::new()));
                if !pending.contains(&stream_id) {
                    pending.push(stream_id);
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Reads and dispatches all buffered messages from `stream_id` (control
    /// channel or, for draft 17+, a per-request bidi stream), `Break` once
    /// the session closed.
    /// - all, not one: no further readable event may follow
    fn process_stream_message(
        &mut self,
        stream_id: u64,
        t: &mut impl Transport,
    ) -> ControlFlow<()> {
        loop {
            let setup_version = self.effective_version();
            let read = self.control_channel.next_message(stream_id, self.selected_version, setup_version, t);
            let cm = match read {
                Ok(v) => v,
                Err(Error::Unimplemented) => {
                    error!("unimplemented");
                    return ControlFlow::Continue(());
                }
                Err(Error::Done) => return ControlFlow::Continue(()),
                // draft-ietf-moq-transport-18 §3.3: a request stream of a type we don't support MAY be reset
                Err(Error::Wire(moq_wire::Error::UnknownMessage(ty)))
                    if !self.control_channel.is_control_stream(stream_id) && self.control_channel.request_id_for_stream(stream_id).is_none() =>
                {
                    debug!("request stream {stream_id} of unknown type 0x{ty:x}, resetting it");
                    t.reset_stream(stream_id, ResetCode::InternalError);
                    self.stop_in_stream(stream_id, t);
                    self.control_channel.request_stream_read_ended(stream_id);
                    return ControlFlow::Continue(());
                }
                Err(Error::Wire(moq_wire::Error::Unsupported(what))) => {
                    return self.close_session(SessionCloseCode::InternalError, &format!("{what} not implemented"), t);
                }
                Err(Error::Wire(e)) => {
                    return self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed control message: {e:?}"), t);
                }
                // the control stream must stay open for the whole session
                Err(e) if self.control_channel.is_control_stream(stream_id) => {
                    return self.close_session(SessionCloseCode::ProtocolViolation, &format!("control stream ended: {e:?}"), t);
                }
                Err(Error::Transport(StreamError::StreamReset(code))) => {
                    debug!("request stream {stream_id} reset with {code}");
                    match self.control_channel.request_id_for_stream(stream_id) {
                        Some(request_id) => self.peer_cancelled_request(request_id, t),
                        None => self.control_channel.request_stream_read_ended(stream_id),
                    }
                    return ControlFlow::Continue(());
                }
                Err(e) => {
                    // a FIN ends only the peer's direction (draft-ietf-moq-transport-19 §3.3.2), ours ends with the request
                    debug!("request stream {stream_id} ended: {e:?}");
                    self.control_channel.request_stream_read_ended(stream_id);
                    return ControlFlow::Continue(());
                }
            };
            #[cfg(feature = "qlog")]
            if let Some(qlog) = t.qlog_streamer() {
                qlog.add_event_now(qlog::events::JsonEvent {
                    time: 0.0,
                    importance: qlog::events::EventImportance::default(),
                    name: "moqt:control_message_parsed".into(),
                    data: serde_json::json!({
                        "stream_id": stream_id,
                        "message": crate::control_channel::cm_qlog_message(&cm),
                    }),
                }).ok();
            }
            if let Err((code, reason)) = self.validate_received(&cm) {
                return self.close_session(code, &reason, t);
            }
            self.dispatch_received_message(stream_id, cm, t);
            if self.closed {
                return ControlFlow::Break(());
            }
            self.grant_request_ids(t);
        }
    }

    /// The negotiated version, the preferred one until SETUP completes.
    fn effective_version(&self) -> Version {
        self.selected_version.unwrap_or_else(|| self.config.version_negotiation.preferred())
    }

    /// Our unanswered PUBLISH_NAMESPACE for `namespace`, how drafts 7–10 name it in responses.
    fn pending_sent_publish_namespace_id(&self, namespace: &Namespace) -> Option<RequestId> {
        self.pending_sent_publish_namespace.iter()
            .find(|(_, pom)| pom.track_namespace() == namespace)
            .map(|(&request_id, _)| request_id)
    }

    /// The peer's PUBLISH_NAMESPACE for `namespace`, accepted or unanswered, how drafts 7–15 name it in PUBLISH_NAMESPACE_DONE.
    fn peer_publish_namespace_id(&self, namespace: &Namespace) -> Option<RequestId> {
        let accepted = self.received_namespaces.iter().find(|(_, ns)| *ns == namespace);
        let unanswered = || self.unanswered_requests.iter()
            .find(|(_, u)| matches!(u, Unanswered::PublishNamespace { namespace: ns } if ns == namespace));
        accepted.map(|(&request_id, _)| request_id).or_else(|| unanswered().map(|(&request_id, _)| request_id))
    }

    /// Sets the Request ID of a reply, end or update that names its request by namespace or stream, and what else
    /// the draft leaves out, `false` if it names none we know.
    fn resolve_request(&mut self, stream_id: u64, cm: &mut ControlMessageEnum) -> bool {
        let request_id = match cm.request_ref(self.effective_version()) {
            None => return true,
            Some(RequestRef::Id(request_id)) => Some(request_id),
            Some(RequestRef::Stream) if matches!(cm, ControlMessageEnum::RequestOk(_) | ControlMessageEnum::RequestError(_)) => self.control_channel.take_reply_request_id(stream_id),
            Some(RequestRef::Stream) => self.control_channel.request_id_for_stream(stream_id),
            Some(RequestRef::Namespace(namespace)) if matches!(cm, ControlMessageEnum::PublishNamespaceDone(_)) => self.peer_publish_namespace_id(namespace),
            Some(RequestRef::Namespace(namespace)) => self.pending_sent_publish_namespace_id(namespace),
        };
        let Some(request_id) = request_id else { return false };
        cm.set_request_id(request_id);
        self.resolve_track_alias(cm)
    }

    /// Drafts 7 to 11 don't repeat the subscriber's track alias in SUBSCRIBE_OK: the pending SUBSCRIBE's, `false` without one.
    fn resolve_track_alias(&self, cm: &mut ControlMessageEnum) -> bool {
        match cm {
            ControlMessageEnum::SubscribeOk(m) if !self.effective_version().publisher_assigns_track_alias() => {
                let Some(track_alias) = self.pending_subscribe.get(&m.request_id()).and_then(PendingSubscribe::track_alias) else { return false };
                m.set_track_alias(track_alias);
                true
            }
            _ => true,
        }
    }

    fn close_session(&mut self, code: SessionCloseCode, reason: &str, t: &mut impl Transport) -> ControlFlow<()> {
        if self.closed {
            return ControlFlow::Break(());
        }
        error!("closing session: {reason}");
        t.close_session(code, reason);
        self.closed = true;
        ControlFlow::Break(())
    }

    /// Session-level checks of a received message, the error code and reason
    /// to close the session with otherwise.
    fn validate_received(&mut self, cm: &ControlMessageEnum) -> core::result::Result<(), (SessionCloseCode, String)> {
        let setup = matches!(cm, ControlMessageEnum::ClientSetup(_) | ControlMessageEnum::ServerSetup(_) | ControlMessageEnum::Setup(_));
        // draft 17 on both send SETUP
        let from_peer_role = match cm {
            ControlMessageEnum::ClientSetup(_) => self.server,
            ControlMessageEnum::ServerSetup(_) => !self.server,
            _ => true,
        };
        if setup && (!from_peer_role || self.selected_version.is_some()) {
            return Err((SessionCloseCode::ProtocolViolation, format!("unexpected {}", cm.qlog_type_name())));
        }
        // the control stream carries SETUP first, other streams wait for it
        if self.selected_version.is_none() && !setup {
            return Err((SessionCloseCode::ProtocolViolation, format!("{} before SETUP", cm.qlog_type_name())));
        }
        if let ControlMessageEnum::MaxRequestId(cm) = cm
            && cm.max_request_id < self.max_request_id
        {
            return Err((SessionCloseCode::ProtocolViolation, format!("MAX_REQUEST_ID decreased to {}", cm.max_request_id)));
        }
        peer_request_id(cm).map_or(Ok(()), |request_id| self.accept_peer_request_id(request_id))
    }

    /// Raises the peer's Request ID limit by the configured window once half
    /// of it is used (MAX_REQUEST_ID exists in drafts 11–16).
    fn grant_request_ids(&mut self, t: &mut impl Transport) {
        let window = self.config.out_max_request_id;
        let has_message = self.selected_version.is_some_and(|v| (11..=16).contains(&v.draft()));
        if !has_message || window == 0 || self.out_max_request_id.0 - self.next_expected_request_id.0 > window / 2 {
            return;
        }
        self.out_max_request_id = RequestId(self.next_expected_request_id.0 + window);
        let cm = ControlMessageEnum::MaxRequestId(MaxRequestIdMessage { max_request_id: self.out_max_request_id });
        self.send_control_message(t, &cm);
    }

    /// Request IDs of a peer's requests must be consecutive and below the
    /// limit we announced (draft-16 §9.1). Advances the expectation if valid.
    fn accept_peer_request_id(&mut self, request_id: RequestId) -> core::result::Result<(), (SessionCloseCode, String)> {
        // draft 07–10 predate Request IDs
        if self.selected_version.is_none_or(|v| v < MOQ_VERSION_DRAFT_11) {
            return Ok(());
        }
        if self.effective_version().limits_requests() && request_id >= self.out_max_request_id {
            return Err((SessionCloseCode::TooManyRequests, format!("request id {request_id} over the limit")));
        }
        let invalid = || Err((SessionCloseCode::InvalidRequestId, format!("unexpected request id {request_id}")));
        if !self.effective_version().stream_per_request() {
            // the next in sequence until draft 16
            if request_id != self.next_expected_request_id {
                return invalid();
            }
            self.next_expected_request_id = self.next_expected_request_id.next();
            return Ok(());
        }
        // from draft 17 the peer's parity and not a duplicate (draft-ietf-moq-transport-19 §10.1)
        let wrong_parity = request_id.0 % 2 != self.next_expected_request_id.0 % 2;
        if wrong_parity || request_id < self.next_expected_request_id || !self.peer_request_ids_ahead.insert(request_id) {
            return invalid();
        }
        if self.peer_request_ids_ahead.len() > MAX_PEER_REQUEST_IDS_AHEAD {
            return Err((SessionCloseCode::TooManyRequests, format!("request id {request_id}, {MAX_PEER_REQUEST_IDS_AHEAD} ahead of {}", self.next_expected_request_id)));
        }
        while self.peer_request_ids_ahead.remove(&self.next_expected_request_id) {
            self.next_expected_request_id = self.next_expected_request_id.next();
        }
        Ok(())
    }

    /// The next Request ID to use, if the peer's limit allows another request.
    /// Also `SessionClosed` once closed.
    fn available_request_id(&self) -> Result<RequestId> {
        self.ensure_open()?;
        let limited = self.effective_version().limits_requests() && !self.config.ignore_max_request_quota;
        if limited && self.next_request_id >= self.max_request_id {
            return Err(Error::RequestBlocked);
        }
        Ok(self.next_request_id)
    }

    fn dispatch_received_message(
        &mut self,
        stream_id: u64,
        mut cm: ControlMessageEnum,
        t: &mut impl Transport,
    ) {
        if !self.resolve_request(stream_id, &mut cm) {
            debug!("{} for no known request", cm.qlog_type_name());
            return;
        }
        match cm {
            ControlMessageEnum::ServerSetup(cm) => {
                self.selected_version = Some(cm.selected_version);
                self.apply_peer_setup_options(&cm.options);
            }
            ControlMessageEnum::Setup(cm) => {
                self.apply_peer_setup_options(&cm.options);
                // the version is settled below this layer from draft 15 (ALPN)
                self.selected_version = Some(self.config.version_negotiation.preferred());
            }
            ControlMessageEnum::RequestsBlocked(cm) => {
                debug!("peer blocked: {cm:?}")
            }
            ControlMessageEnum::MaxRequestId(cm) => {
                self.max_request_id = cm.max_request_id;
            }
            ControlMessageEnum::SubscribeOk(cm) => {
                let (req_id, track_alias) = (cm.request_id(), cm.track_alias());
                // unsubscribed while the SUBSCRIBE_OK was in flight
                if self.pending_subscribe.remove(&req_id).is_none() {
                    self.drop_in_track(track_alias, t);
                    return;
                }
                if !self.open_in_track(track_alias, cm.track_properties(), "SUBSCRIBE_OK", t) {
                    return;
                }
                let version = self.effective_version();
                if let Some(options) = self.subscription_options.get_mut(&req_id) {
                    options.filter = options.filter.resolved(cm.largest_location(), version);
                }
                self.active_subscriptions.insert(req_id, track_alias);
                self.events.push_back(Event::SubscribeOk { request_id: req_id, track_alias, message: cm });
            }
            ControlMessageEnum::RequestError(cm) => {
                let request_id = cm.request_id();
                self.control_channel.end_request_stream(request_id, t);
                self.fail_pending_request(request_id, RequestFailure::Rejected(cm), t);
            }
            ControlMessageEnum::PublishDone(cm) => {
                let req_id = cm.request_id();
                if let Some(&track_alias) = self.active_subscriptions.get(&req_id) {
                    if let Some(track) = self.in_tracks.get_mut(&track_alias) {
                        track.mark_done(cm.stream_count(), self.now);
                    }
                } else {
                    self.cancel_request(req_id, t);
                }
            }
            ControlMessageEnum::Unsubscribe(cm) => self.end_peer_request(cm.request_id, t),
            ControlMessageEnum::PublishNamespace(cm) => {
                let request_id = match cm.request_id() {
                    // validated by `accept_peer_request_id`
                    Some(request_id) => request_id,
                    // draft 07–10: ANNOUNCE has no Request ID at all (it predates
                    // the concept). Synthesize a local one purely for our own
                    // bookkeeping — never sent on the wire; see
                    // `accept_publish_namespace`'s version-gated reply.
                    None => self.local_request_id(),
                };
                self.control_channel.record_request_stream_if_needed(stream_id, request_id, t);
                let namespace = cm.track_namespace().clone();
                self.receive_request(request_id, Unanswered::PublishNamespace { namespace }, cm, IncomingRequest::PublishNamespace);
            }
            ControlMessageEnum::ClientSetup(cm) => {
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
                    VersionNegotiation::InBand { candidates } => {
                        let mutual = candidates.iter().copied()
                            .filter(|v| cm.supported_versions.contains(&v.to_wire()) && self.control_channel.matches_topology(*v))
                            .max();
                        let Some(version) = mutual else {
                            let _ = self.close_session(SessionCloseCode::VersionNegotiationFailed, "no mutually supported version", t);
                            return;
                        };
                        version
                    }
                };
                self.selected_version = Some(version);
                self.apply_peer_setup_options(&cm.options);
                let setup = ServerSetupMessage::new(version, self.setup_options());
                self.send_control_message(t, &ControlMessageEnum::ServerSetup(setup));
            }
            ControlMessageEnum::Subscribe(cm) => {
                self.control_channel.record_request_stream_if_needed(stream_id, cm.request_id, t);
                let unanswered = Unanswered::Subscribe { track_alias: cm.track_alias };
                if self.object_filter(&cm.range_filters).is_none() {
                    self.send_rejection(cm.request_id, unanswered, RequestErrorCode::InvalidFilter, t);
                    return;
                }
                self.receive_request(cm.request_id, unanswered, cm, IncomingRequest::Subscribe);
            }
            ControlMessageEnum::RequestUpdate(cm) => {
                // REQUEST_UPDATE is answered, SUBSCRIBE_UPDATE before draft 15 isn't
                let answered = cm.request_id.filter(|_| self.effective_version().request_update_has_parameters());
                let unanswered = Unanswered::RequestUpdate { existing_request_id: cm.existing_request_id };
                let track_alias = match self.out_requests.get(&cm.existing_request_id).copied() {
                    // TODO replacing Range Filters (draft-ietf-moq-transport-19 §5.1.3), until then an update with them is refused
                    Some(track_alias) if cm.range_filters.is_empty() => track_alias,
                    found => {
                        let error_code = if found.is_some() { RequestErrorCode::NotSupported } else { RequestErrorCode::DoesNotExist };
                        if let Some(request_id) = answered {
                            self.send_rejection(request_id, unanswered, error_code, t);
                        }
                        return;
                    }
                };
                if let Some(track) = self.out_tracks.get_mut(&track_alias) {
                    track.update_timeouts(DeliveryTimeouts::from_millis(cm.object_delivery_timeout, cm.subgroup_delivery_timeout));
                }
                if cm.forward.is_some() || cm.filter.is_some() {
                    self.events.push_back(Event::SubscriptionUpdated { request_id: cm.existing_request_id, forward: cm.forward, filter: cm.filter });
                }
                // draft-19 §10.2.7/§7.2: also re-prioritizes open streams
                if let Some(priority) = cm.subscriber_priority
                    && let Some(incremental) = self.out_tracks.get(&track_alias).map(|track| track.incremental)
                {
                    self.set_out_track_priority(track_alias, priority, incremental, t);
                }
                // The REQUEST_OK reply needs LARGEST_OBJECT (draft-16 §9.2.2.7),
                // which needs the caller's object cache
                if let Some(request_id) = answered {
                    self.receive_request(request_id, unanswered, cm, IncomingRequest::RequestUpdate);
                }
            }
            ControlMessageEnum::PublishNamespaceError(cm) => {
                self.fail_pending_request(cm.request_id(), RequestFailure::Rejected(cm.into_request_error()), t);
            }
            ControlMessageEnum::RequestOk(cm) => {
                let request_id = cm.request_id();
                if self.pending_request_updates.remove(&request_id) {
                    self.events.push_back(Event::RequestUpdateOk { request_id, largest_location: cm.largest_location() });
                } else if let Some(pom) = self.pending_sent_publish_namespace.remove(&request_id) {
                    self.sent_namespaces.insert(request_id, pom.take_track_namespace());
                    self.events.push_back(Event::PublishNamespaceOk { request_id });
                } else if self.pending_track_status.remove(&request_id) {
                    // the reply ends the request
                    self.control_channel.end_request_stream(request_id, t);
                    self.events.push_back(Event::TrackStatus { request_id, largest_location: cm.largest_location(), track_properties: cm.track_properties().clone() });
                } else if self.pending_sent_publish.remove(&request_id).is_some() {
                    // PUBLISH_OK is a REQUEST_OK from draft 18
                    match cm.into_publish_ok(self.effective_version()) {
                        Ok(message) => self.publish_accepted(request_id, message, t),
                        Err(e) => {
                            let _ = self.close_session(SessionCloseCode::ProtocolViolation, &format!("malformed PUBLISH_OK: {e:?}"), t);
                        }
                    }
                }
            }
            ControlMessageEnum::PublishNamespaceDone(cm) => self.end_peer_request(cm.request_id(), t),
            ControlMessageEnum::PublishOk(cm) => {
                let request_id = cm.request_id;
                if self.pending_sent_publish.remove(&request_id).is_some() {
                    self.publish_accepted(request_id, cm, t);
                }
            }
            ControlMessageEnum::Publish(cm) => {
                let request_id = cm.request_id;
                self.control_channel.record_request_stream_if_needed(stream_id, request_id, t);
                if !self.open_in_track(cm.track_alias, &cm.track_properties, "PUBLISH", t) {
                    return;
                }
                let unanswered = Unanswered::Publish { track_alias: cm.track_alias };
                self.receive_request(request_id, unanswered, cm, IncomingRequest::Publish);
            }
            ControlMessageEnum::TrackStatus(cm) => {
                // drafts 7 to 10 have no Request ID
                let request_id = cm.request_id.unwrap_or_else(|| self.local_request_id());
                self.control_channel.record_request_stream_if_needed(stream_id, request_id, t);
                let unanswered = Unanswered::TrackStatus { namespace_trackname: cm.namespace_trackname.clone() };
                self.receive_request(request_id, unanswered, cm, IncomingRequest::TrackStatus);
            }
            ControlMessageEnum::TrackStatusOk(cm) => {
                let request_id = cm.request_id();
                if self.pending_track_status.remove(&request_id) {
                    self.events.push_back(Event::TrackStatus { request_id, largest_location: cm.largest_location, track_properties: cm.track_properties });
                }
            }
            ControlMessageEnum::TrackStatusError(cm) => {
                self.fail_pending_request(cm.request_id(), RequestFailure::Rejected(cm.into_request_error()), t);
            }
            ControlMessageEnum::Fetch(cm) => {
                self.control_channel.record_request_stream_if_needed(stream_id, cm.request_id, t);
                if let Some(error_code) = self.fetch_refusal(&cm) {
                    self.send_rejection(cm.request_id, Unanswered::Fetch, error_code, t);
                    return;
                }
                self.receive_request(cm.request_id, Unanswered::Fetch, cm, IncomingRequest::Fetch);
            }
            ControlMessageEnum::FetchOk(cm) => {
                let request_id = cm.request_id();
                if self.fetches.contains_key(&request_id) {
                    self.events.push_back(Event::FetchOk { request_id, end_of_track: cm.end_of_track, end_location: cm.end_location });
                }
            }
            ControlMessageEnum::FetchError(cm) => {
                self.fail_pending_request(cm.request_id(), RequestFailure::Rejected(cm.into_request_error()), t);
            }
            ControlMessageEnum::FetchCancel(cm) => self.end_peer_request(cm.request_id, t),
            ControlMessageEnum::SubscribeNamespace(cm) => {
                self.control_channel.record_request_stream_if_needed(stream_id, cm.request_id, t);
                self.receive_request(cm.request_id, Unanswered::SubscribeNamespace, cm, IncomingRequest::SubscribeNamespace);
            }
            ControlMessageEnum::SubscribeTracks(cm) => {
                self.control_channel.record_request_stream_if_needed(stream_id, cm.request_id, t);
                self.receive_request(cm.request_id, Unanswered::SubscribeTracks, cm, IncomingRequest::SubscribeTracks);
            }
            // TODO UNSUBSCRIBE_NAMESPACE, with SUBSCRIBE_NAMESPACE of drafts 13 to 17
            cm => {
                let _ = self.close_session(SessionCloseCode::InternalError, &format!("{} not implemented", cm.qlog_type_name()), t);
            }
        }
    }

    /// The subscriber accepted our PUBLISH `request_id`, its timeouts and Range Filters apply to the track.
    fn publish_accepted(&mut self, request_id: RequestId, message: PublishOkMessage, t: &mut impl Transport) {
        let Some(object_filter) = self.object_filter(&message.range_filters) else {
            // a reply can't get INVALID_FILTER
            let _ = self.close_session(SessionCloseCode::ProtocolViolation, "invalid Range Filter in PUBLISH_OK", t);
            return;
        };
        if let Some(track) = self.out_requests.get(&request_id).and_then(|track_alias| self.out_tracks.get_mut(track_alias)) {
            track.update_timeouts(DeliveryTimeouts::from_millis(message.object_delivery_timeout, message.subgroup_delivery_timeout));
            track.object_filter = object_filter;
        }
        self.events.push_back(Event::PublishOk { request_id, message });
    }

    /// The filter `range_filters` describe, `None` if invalid or beyond our MAX_FILTER_RANGES (draft-ietf-moq-transport-19 §5.1.3).
    pub(super) fn object_filter(&self, range_filters: &RangeFilters) -> Option<ObjectFilter> {
        let filter = range_filters.parse(self.effective_version())?;
        (filter.range_count() as u64 <= self.config.max_filter_ranges).then_some(filter)
    }

    /// Our SETUP options, each draft carries those it has.
    fn setup_options(&self) -> SetupOptions {
        let (authority, path) = self.config.native_quic_uri.clone().unzip();
        SetupOptions { max_request_id: Some(self.out_max_request_id), max_filter_ranges: self.config.max_filter_ranges, role: Some(Role::PubSub), authority, path, ..SetupOptions::default() }
    }

    fn apply_peer_setup_options(&mut self, options: &SetupOptions) {
        self.max_request_id = options.max_request_id.unwrap_or(RequestId(DEFAULT_MAX_REQUEST_ID_SETUP_PARAMETER));
        self.control_channel.peer_max_request_updates = options.max_request_updates.unwrap_or(0);
    }

    /// Starts receiving `track_alias` of `track_properties`, with the streams that arrived for it before,
    /// `false` once the session closed for `message` reusing it.
    fn open_in_track(&mut self, track_alias: TrackAlias, track_properties: &KeyValuePairs, message: &str, t: &mut impl Transport) -> bool {
        if self.in_tracks.contains_key(&track_alias) {
            let _ = self.close_session(SessionCloseCode::DuplicateTrackAlias, &format!("{message} reuses track alias {track_alias}"), t);
            return false;
        }
        self.dropped_aliases.remove(&track_alias);
        // draft-ietf-moq-transport-18 §12.4
        let publisher_priority = track_properties.get_varint(moq_wire::property::DEFAULT_PUBLISHER_PRIORITY)
            .and_then(|priority| u8::try_from(priority).ok())
            .unwrap_or(moq_wire::DEFAULT_PUBLISHER_PRIORITY);
        let track = self.in_tracks.entry(track_alias).insert(InTrack::new(track_alias, publisher_priority)).into_mut();
        for stream_id in self.pending_streams.remove(&track_alias).map(|(_, stream_ids)| stream_ids).unwrap_or_default() {
            track.mark_stream_readable(stream_id);
        }
        for (datagram, payload) in self.pending_datagrams.remove(&track_alias).map(|(_, datagrams)| datagrams).unwrap_or_default() {
            track.push_datagram(datagram, payload);
        }
        true
    }

    /// Fills the remaining send capacity with padding, call after sending
    /// application data. No-op without a padding stream.
    pub fn send_padding(&mut self, t: &mut impl Transport) {
        let version = self.effective_version();
        if let Some(ps) = &mut self.out_padding_stream {
            ps.handle(t, version, self.now);
        }
    }

    pub fn start_padding_stream(&mut self) {
        if self.out_padding_stream.is_some() {
            return;
        }
        self.out_padding_stream = Some(OutPaddingStream::new(self.config.padding_stream_max_bytes_per_sec, self.now));
    }

    pub fn stop_padding_stream(&mut self, t: &mut impl Transport) {
        if let Some(ps) = self.out_padding_stream.take() {
            ps.stop(t);
        }
    }

    pub fn padding_stream_running(&self) -> bool {
        self.out_padding_stream.is_some()
    }
}


/// Request ID of a request the peer sent, `None` for any other message.
fn peer_request_id(cm: &ControlMessageEnum) -> Option<RequestId> {
    match cm {
        ControlMessageEnum::Subscribe(cm) => Some(cm.request_id),
        ControlMessageEnum::RequestUpdate(cm) => cm.request_id,
        ControlMessageEnum::TrackStatus(cm) => cm.request_id,
        ControlMessageEnum::Fetch(cm) => Some(cm.request_id),
        ControlMessageEnum::Publish(cm) => Some(cm.request_id),
        ControlMessageEnum::PublishNamespace(cm) => cm.request_id(),
        ControlMessageEnum::SubscribeNamespace(cm) => Some(cm.request_id),
        ControlMessageEnum::SubscribeTracks(cm) => Some(cm.request_id),
        _ => None,
    }
}

mod datagrams;
mod namespaces;
mod fetches;
pub use fetches::FetchOptions;
mod publications;
pub use datagrams::OutgoingDatagram;
pub use publications::OutgoingObject;
mod requests;
mod subscriptions;
pub use subscriptions::SubscribeOptions;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod fuzz_tests;
