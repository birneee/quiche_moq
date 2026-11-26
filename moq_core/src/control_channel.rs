//! Control message I/O: the control channel (one bidi stream before draft
//! 17, a pair of uni streams from it) and draft 17+'s bidi stream per
//! SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE. Callers don't know which stream a
//! message takes.

use crate::prelude::*;

use crate::error::{Error, Result};
use crate::transport::{CONTROL_STREAM_URGENCY, REQUEST_STREAM_URGENCY, StreamError, Transport};
use log::{debug, trace};
use moq_wire::control_message::ControlMessageEnum;
#[cfg(feature = "qlog")]
use moq_wire::control_message::subscribe::LocationFilter;
use moq_wire::{FromBytes, RequestId, ResetCode, ToBytes, Version, SETUP_MESSAGE_ID};

use alloc::collections::VecDeque;
use hashbrown::{HashMap, HashSet};

/// `subscription_filter` parameter value per draft-pardue-moq-qlog-moq-events
/// §5.3 (`MOQTSubscriptionFilter`).
/// - `filter_type`: raw wire ID (`LocationFilter::wire_id`), not variant name — schema-decodable
/// - `end_group_delta` not emitted: unused here, End Group is always encoded absolute on the wire
#[cfg(feature = "qlog")]
fn filter_qlog_value(filter: &LocationFilter) -> serde_json::Value {
    let mut v = serde_json::json!({ "filter_type": filter.wire_id() });
    if let Some(loc) = filter.start_location() {
        v["start_location"] = serde_json::json!({ "group": loc.group, "object": loc.object });
    }
    if let Some(end_group) = filter.end_group() {
        v["end_group"] = end_group.into();
    }
    v
}

#[cfg(feature = "qlog")]
pub(crate) fn cm_qlog_message(cm: &ControlMessageEnum) -> serde_json::Value {
    let mut msg = serde_json::json!({ "type": cm.qlog_type_name() });
    if let ControlMessageEnum::Subscribe(sm) = cm {
        // lets analysis map a later SUBSCRIBE_OK's track_alias to its track via request_id
        msg["request_id"] = sm.request_id.0.into();
        msg["subscriber_priority"] = sm.subscriber_priority.into();
        msg["forward"] = u8::from(sm.forward).into();
        msg["track_namespace"] = sm.namespace_trackname.namespace().iter()
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .collect::<Vec<_>>().into();
        msg["track_name"] = String::from_utf8_lossy(sm.namespace_trackname.trackname()).into_owned().into();
    } else if let ControlMessageEnum::SubscribeOk(som) = cm {
        msg["request_id"] = som.request_id().0.into();
        msg["track_alias"] = som.track_alias().0.into();
        let mut params: Vec<serde_json::Value> = vec![];
        if som.expires() != 0 {
            params.push(serde_json::json!({ "name": "expires", "value": som.expires() }));
        }
        if let Some(order) = som.group_order() {
            params.push(serde_json::json!({ "name": "group_order", "value": u64::from(order) }));
        }
        if let Some(loc) = som.largest_location() {
            params.push(serde_json::json!({
                "name": "largest_object",
                "value": { "group": loc.group, "object": loc.object },
            }));
        }
        if let Some(mcd) = som.max_cache_duration() {
            params.push(serde_json::json!({ "name": "max_cache_duration", "value": mcd }));
        }
        msg["number_of_parameters"] = params.len().into();
        if !params.is_empty() {
            msg["parameters"] = params.into();
        }
    } else if let ControlMessageEnum::PublishDone(pdm) = cm {
        msg["request_id"] = pdm.request_id().0.into();
        msg["status_code"] = u64::from(pdm.status()).into();
        msg["stream_count"] = pdm.stream_count().into();
    } else if let ControlMessageEnum::RequestUpdate(rum) = cm {
        // draft-pardue-moq-qlog-moq-events §5.7.7 also has required_request_id_delta, always 0 here
        if let Some(request_id) = rum.request_id {
            msg["request_id"] = request_id.0.into();
        }
        msg["existing_request_id"] = rum.existing_request_id.0.into();
        let mut params: Vec<serde_json::Value> = vec![];
        if let Some(forward) = rum.forward {
            params.push(serde_json::json!({ "name": "forward", "value": u64::from(forward) }));
        }
        if let Some(dt) = rum.object_delivery_timeout {
            params.push(serde_json::json!({ "name": "delivery_timeout", "value": dt }));
        }
        if let Some(filter) = &rum.filter {
            params.push(serde_json::json!({
                "name": "subscription_filter",
                "value": filter_qlog_value(filter),
            }));
        }
        msg["number_of_parameters"] = params.len().into();
        if !params.is_empty() {
            msg["parameters"] = params.into();
        }
    }
    msg
}

/// draft-ietf-moq-transport-17 (#1510) replaces the single bidirectional control
/// stream with a pair of unidirectional streams, one opened by each peer.
fn uses_uni_pair(version: Version) -> bool {
    version.stream_per_request()
}

/// Returns true if `stream_id` is a QUIC bidirectional stream (bit 1 = 0).
fn is_bidi_stream(stream_id: u64) -> bool {
    (stream_id & 0x2) == 0
}

/// The stream(s) session-establishment and (pre-per-request-stream) control
/// messages flow over — `ControlChannel`'s topology, chosen once and fixed for
/// the life of the session.
///
/// - `Bidi`: draft < 17. A single shared bidirectional stream, opened by the
///   client; the server discovers it as the first bidi stream that becomes readable.
/// - `UniPair`: draft 17+. Each peer opens its own outbound uni stream; the peer's
///   is told from its data streams by the first byte, see `ControlChannel::probe_control_stream`.
#[derive(Debug, Clone, Copy)]
enum Topology {
    Bidi(u64),
    UniPair { out_id: u64, in_id: Option<u64> },
}

impl Topology {
    fn out_id(&self) -> u64 {
        match self {
            Topology::Bidi(id) => *id,
            Topology::UniPair { out_id, .. } => *out_id,
        }
    }

    fn in_id(&self) -> Option<u64> {
        match self {
            Topology::Bidi(id) => Some(*id),
            Topology::UniPair { in_id, .. } => *in_id,
        }
    }
}

pub(crate) struct ControlChannel {
    topology: Option<Topology>,
    /// Decides the topology before it is established, see `accept`
    setup_version: Version,
    /// Before the peer's control stream is known, the first byte of its data streams read telling them apart
    data_stream_first_bytes: HashMap<u64, u8>,
    ctrl_buf: MessageBuf,
    pending_control_messages: VecDeque<Vec<u8>>,
    /// Draft 17+: each request's own bidi stream, ours and the peer's.
    request_streams: HashMap<RequestId, u64>,
    /// Reverse of `request_streams`, for routing data that arrives on a request stream.
    request_stream_request_ids: HashMap<u64, RequestId>,
    /// Partial-read buffers of request streams, until their read side ends.
    request_stream_bufs: HashMap<u64, MessageBuf>,
    /// Request streams that began: ours, and the peer's once their first message was read. Until their read side ends.
    begun_request_streams: HashSet<u64>,
    /// Encoded messages queued per request stream (mirrors `pending_control_messages`).
    pending_request_messages: HashMap<u64, VecDeque<Vec<u8>>>,
    /// Ended request streams whose last messages are still queued, FIN'd once sent.
    finishing_request_streams: HashMap<u64, VecDeque<Vec<u8>>>,
    /// Requests whose stream the peer stopped, which cancels them (draft-ietf-moq-transport-19 §3.3.3).
    stopped_requests: Vec<RequestId>,
    /// Our REQUEST_UPDATEs per request stream, oldest first, until their REQUEST_OK or REQUEST_ERROR.
    outstanding_updates: HashMap<u64, VecDeque<RequestId>>,
    /// The peer's MAX_REQUEST_UPDATES per request stream, 0 for no limit (draft-ietf-moq-transport-19 §10.3.1.7).
    pub(crate) peer_max_request_updates: u64,
}

impl ControlChannel {
    /// Client-side: opens the channel (a bidi stream, or draft 17+'s uni-pair
    /// out-stream) immediately.
    pub(crate) fn connect(t: &mut impl Transport, setup_version: Version) -> Result<Self> {
        let topology = if uses_uni_pair(setup_version) {
            Topology::UniPair { out_id: t.open_uni_stream()?, in_id: None }
        } else {
            Topology::Bidi(t.open_bidi_stream()?)
        };
        // draft-19 §7.2: control above all object streams
        t.set_stream_priority(topology.out_id(), CONTROL_STREAM_URGENCY, false);
        Ok(Self { topology: Some(topology), ..Self::accept(setup_version) })
    }

    /// Server-side: no I/O yet, `try_establish` finds the client's control stream.
    /// - `setup_version` decides the topology: pinned by ALPN from draft 15, in-band negotiation is for drafts up to 14
    pub(crate) fn accept(setup_version: Version) -> Self {
        Self {
            topology: None,
            setup_version,
            data_stream_first_bytes: HashMap::new(),
            ctrl_buf: MessageBuf::default(),
            pending_control_messages: VecDeque::new(),
            request_streams: HashMap::new(),
            request_stream_request_ids: HashMap::new(),
            request_stream_bufs: HashMap::new(),
            begun_request_streams: HashSet::new(),
            pending_request_messages: HashMap::new(),
            finishing_request_streams: HashMap::new(),
            stopped_requests: Vec::new(),
            outstanding_updates: HashMap::new(),
            peer_max_request_updates: 0,
        }
    }

    /// Finds the peer's control stream among `readable`, the server opens its own then.
    /// Returns `false` until the topology is known, the server's bidi control stream arrived or it opened its uni one.
    pub(crate) fn try_establish(&mut self, readable: &[u64], t: &mut impl Transport) -> bool {
        if self.topology.is_none() {
            let topology = if uses_uni_pair(self.setup_version) {
                // stream limit reached: the next `handle()` retries
                let Ok(out_id) = t.open_uni_stream() else { return false };
                Topology::UniPair { out_id, in_id: None }
            } else {
                let Some(id) = readable.iter().copied().find(|&id| is_bidi_stream(id)) else { return false };
                Topology::Bidi(id)
            };
            t.set_stream_priority(topology.out_id(), CONTROL_STREAM_URGENCY, false);
            self.topology = Some(topology);
        }
        // draft 17+: request and data streams may arrive before the control stream (draft-ietf-moq-transport-19 §3.3)
        if let Some(Topology::UniPair { out_id, in_id: None }) = self.topology
            && let Some(id) = readable.iter().copied().filter(|&id| !is_bidi_stream(id)).find(|&id| self.probe_control_stream(id, t))
        {
            self.topology = Some(Topology::UniPair { out_id, in_id: Some(id) });
        }
        true
    }

    /// Draft 17+: tells from its first byte whether peer uni stream `stream_id` is its control stream, which begins with SETUP.
    /// A data stream's one stays for `take_data_stream_first_byte`, the control stream's goes to its buffer.
    /// - the first byte of SETUP's type differs from every other uni stream type's (draft-ietf-moq-transport-19 §3.4)
    fn probe_control_stream(&mut self, stream_id: u64, t: &mut impl Transport) -> bool {
        if self.data_stream_first_bytes.contains_key(&stream_id) {
            return false;
        }
        let mut first = [0u8];
        if !matches!(t.recv_stream(stream_id, &mut first), Ok(1)) {
            return false;
        }
        let mut setup_type = Vec::new();
        self.setup_version.writer(&mut setup_type).varint(SETUP_MESSAGE_ID).expect("SETUP's type is a varint");
        if first[0] != setup_type[0] {
            self.data_stream_first_bytes.insert(stream_id, first[0]);
            return false;
        }
        self.ctrl_buf.0.push(first[0]);
        true
    }

    /// The first byte of data stream `stream_id`, if read telling it from the control stream.
    pub(crate) fn take_data_stream_first_byte(&mut self, stream_id: u64) -> Option<u8> {
        self.data_stream_first_bytes.remove(&stream_id)
    }

    /// True if `stream_id` should be routed through `next_message` (this
    /// channel's inbound stream, or — draft 17+ only — any bidi stream, since
    /// those only ever carry per-request control traffic, never media data)
    /// rather than treated as a media data stream.
    pub(crate) fn owns(&self, stream_id: u64) -> bool {
        self.is_control_stream(stream_id) || is_bidi_stream(stream_id)
    }

    /// Our control stream, the client's bidi one before draft 17.
    pub(crate) fn out_stream(&self) -> Option<u64> {
        self.topology.map(|topology| topology.out_id())
    }

    #[cfg(test)]
    pub(crate) fn has_queued_control_messages(&self) -> bool {
        !self.pending_control_messages.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn request_stream(&self, request_id: RequestId) -> Option<u64> {
        self.request_streams.get(&request_id).copied()
    }

    /// The control channel's inbound stream, not a per-request one.
    pub(crate) fn is_control_stream(&self, stream_id: u64) -> bool {
        self.topology.and_then(|c| c.in_id()) == Some(stream_id)
    }

    /// True once the negotiated version is known to use draft 17+'s per-request
    /// bidi streams instead of multiplexing everything on this channel.
    pub(crate) fn uses_per_request_streams(&self) -> bool {
        matches!(self.topology, Some(Topology::UniPair { .. }))
    }

    /// True if `version`'s "official" topology (bidi vs. uni pair) matches what
    /// was actually established. Used when picking a version to negotiate to,
    /// since the wire format of responses follows the negotiated version.
    pub(crate) fn matches_topology(&self, version: Version) -> bool {
        self.uses_per_request_streams() == uses_uni_pair(version)
    }

    /// Flushes whatever queued bytes the transport now has capacity for.
    /// - a request stream the peer stopped drops its queue, `take_stopped_requests` reports its request
    pub(crate) fn flush(&mut self, t: &mut impl Transport) -> Result<()> {
        let Some(topology) = self.topology else { return Ok(()) };
        Self::flush_queue(&mut self.pending_control_messages, topology.out_id(), t)?;
        self.pending_request_messages.retain(|&stream_id, queue| {
            Self::flush_queue(queue, stream_id, t)
                .inspect_err(|e| debug!("request stream {stream_id} not writable: {e:?}"))
                .is_ok()
        });
        // also streams we have nothing to write to, e.g. the peer stopped after its FIN
        for (&stream_id, &request_id) in &self.request_stream_request_ids {
            if t.stream_stopped(stream_id) && !self.stopped_requests.contains(&request_id) {
                self.stopped_requests.push(request_id);
            }
        }
        self.flush_finishing_request_streams(t);
        Ok(())
    }

    /// draft 17+: ends `request_id`'s stream from our side and forgets it.
    /// Returns the stream, for the caller to also stop reading it if needed.
    /// - queued messages are sent first, then the FIN
    pub(crate) fn end_request_stream(&mut self, request_id: RequestId, t: &mut impl Transport) -> Option<u64> {
        let (stream_id, queue) = self.forget_request_stream(request_id)?;
        self.finishing_request_streams.insert(stream_id, queue);
        self.flush_finishing_request_streams(t);
        Some(stream_id)
    }

    /// draft 17+: cancels `request_id` (draft-ietf-moq-transport-19 §3.3.3), resets our direction of its stream and forgets it.
    /// Returns the stream, for the caller to also stop reading it.
    pub(crate) fn cancel_request_stream(&mut self, request_id: RequestId, t: &mut impl Transport) -> Option<u64> {
        let (stream_id, _) = self.forget_request_stream(request_id)?;
        t.reset_stream(stream_id, ResetCode::Canceled);
        self.request_stream_read_ended(stream_id);
        Some(stream_id)
    }

    /// Forgets `request_id`'s stream, returns it and what is still queued for it.
    /// - what it reads stays until its read side ends, see `request_stream_read_ended`
    fn forget_request_stream(&mut self, request_id: RequestId) -> Option<(u64, VecDeque<Vec<u8>>)> {
        let stream_id = self.request_streams.remove(&request_id)?;
        self.request_stream_request_ids.remove(&stream_id);
        self.outstanding_updates.remove(&stream_id);
        Some((stream_id, self.pending_request_messages.remove(&stream_id).unwrap_or_default()))
    }

    /// Sends REQUEST_UPDATE `cm` for `existing_request_id`, from draft 17 on that request's stream (draft-ietf-moq-transport-19 §10.9).
    /// - `Error::TrackEnded` once that stream ended
    /// - `Error::RequestBlocked` at the peer's MAX_REQUEST_UPDATES
    pub(crate) fn send_update(&mut self, existing_request_id: RequestId, update_request_id: RequestId, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) -> Result<()> {
        if !self.uses_per_request_streams() {
            return self.send_control(cm, version, t);
        }
        let &stream_id = self.request_streams.get(&existing_request_id).ok_or(Error::TrackEnded)?;
        let outstanding = self.outstanding_updates.get(&stream_id).map_or(0, VecDeque::len);
        if self.peer_max_request_updates != 0 && outstanding as u64 >= self.peer_max_request_updates {
            return Err(Error::RequestBlocked);
        }
        self.send_response(existing_request_id, cm, version, t)?;
        self.outstanding_updates.entry(stream_id).or_default().push_back(update_request_id);
        Ok(())
    }

    /// The request a REQUEST_OK or REQUEST_ERROR on request stream `stream_id` answers:
    /// our oldest outstanding REQUEST_UPDATE there, else the stream's request, which updates only follow.
    pub(crate) fn take_reply_request_id(&mut self, stream_id: u64) -> Option<RequestId> {
        self.outstanding_updates.get_mut(&stream_id).and_then(VecDeque::pop_front).or_else(|| self.request_id_for_stream(stream_id))
    }

    /// The requests whose stream the peer stopped since the last call.
    pub(crate) fn take_stopped_requests(&mut self) -> Vec<RequestId> {
        core::mem::take(&mut self.stopped_requests)
    }

    /// Nothing more to read on request stream `stream_id`: the peer ended its side, or we stopped reading.
    pub(crate) fn request_stream_read_ended(&mut self, stream_id: u64) {
        self.request_stream_bufs.remove(&stream_id);
        self.begun_request_streams.remove(&stream_id);
    }

    /// Sends what ended request streams still have queued, then their FIN.
    /// - a stream the peer stopped is dropped
    fn flush_finishing_request_streams(&mut self, t: &mut impl Transport) {
        self.finishing_request_streams.retain(|&stream_id, queue| {
            if Self::flush_queue(queue, stream_id, t).is_err() {
                return false;
            }
            if !queue.is_empty() {
                return true;
            }
            let fin = t.send_stream(stream_id, &[], true);
            // retried while the transport has no capacity
            matches!(fin, Err(StreamError::Done))
        });
    }

    fn flush_queue(queue: &mut VecDeque<Vec<u8>>, stream_id: u64, t: &mut impl Transport) -> Result<()> {
        while let Some(msg) = queue.front_mut() {
            match t.send_stream(stream_id, msg, false) {
                // partial write: keep the remainder for the next flush
                Ok(n) if n == msg.len() => { queue.pop_front(); }
                Ok(n) => { msg.drain(..n); break; }
                Err(StreamError::Done) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn encode_and_queue(
        queue: &mut VecDeque<Vec<u8>>,
        stream_id: u64,
        version: Version,
        cm: &ControlMessageEnum,
        t: &mut impl Transport,
    ) -> Result<()> {
        queue.push_back(encode(cm, version)?);
        Self::flush_queue(queue, stream_id, t)?;
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::default(),
                name: "moqt:control_message_created".into(),
                data: serde_json::json!({
                    "stream_id": stream_id,
                    "message": cm_qlog_message(cm),
                }),
            }).ok();
        }
        debug!("moq send message on stream {}: {:?}", stream_id, &cm);
        Ok(())
    }

    /// Sends a session-level message (not tied to any one request) on the
    /// shared control channel — SETUP, REQUEST_UPDATE, PUBLISH_DONE, etc.
    /// Fails if the control stream can't be written, e.g. the peer stopped it.
    pub(crate) fn send_control(&mut self, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) -> Result<()> {
        let Some(topology) = self.topology else { panic!("control channel not opened yet") };
        Self::encode_and_queue(&mut self.pending_control_messages, topology.out_id(), version, cm, t)
    }

    /// Sends a request-initiating message (SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE):
    /// on draft 17+ this opens a dedicated bidi stream for it and remembers the
    /// stream<->request_id mapping (so the response can be routed back), matching
    /// the "requests begin their own stream" model; on older drafts it's just
    /// queued on the shared control channel like any other message.
    pub(crate) fn send_request(&mut self, request_id: RequestId, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) -> Result<()> {
        if self.uses_per_request_streams() {
            let stream_id = t.open_bidi_stream()?;
            t.set_stream_priority(stream_id, REQUEST_STREAM_URGENCY, false);
            self.request_streams.insert(request_id, stream_id);
            self.request_stream_request_ids.insert(stream_id, request_id);
            self.begun_request_streams.insert(stream_id);
            let queue = self.pending_request_messages.entry(stream_id).or_default();
            Self::encode_and_queue(queue, stream_id, version, cm, t)
        } else {
            self.send_control(cm, version, t)
        }
    }

    /// Sends a response (SUBSCRIBE_OK/PUBLISH_OK/REQUEST_ERROR/...) for
    /// `request_id`, routing it to the request's dedicated stream if one is
    /// known (draft 17+), or the shared control channel otherwise.
    /// Fails if the control stream can't be written or `cm` encoded. A request
    /// stream the peer stopped is noticed by the next `flush`.
    pub(crate) fn send_response(&mut self, request_id: RequestId, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) -> Result<()> {
        match self.request_streams.get(&request_id) {
            Some(&stream_id) => {
                let queue = self.pending_request_messages.entry(stream_id).or_default();
                match Self::encode_and_queue(queue, stream_id, version, cm, t) {
                    Err(Error::Transport(_)) => Ok(()),
                    result => result,
                }
            }
            None => self.send_control(cm, version, t),
        }
    }

    /// draft 17+ only: if `stream_id` isn't the control channel, remember it as
    /// `request_id`'s dedicated stream (for routing the eventual response, or a
    /// response we're about to send).
    pub(crate) fn record_request_stream_if_needed(&mut self, stream_id: u64, request_id: RequestId, t: &mut impl Transport) {
        if self.topology.and_then(|c| c.in_id()) != Some(stream_id) {
            // peer-opened, but carries our responses
            t.set_stream_priority(stream_id, REQUEST_STREAM_URGENCY, false);
            self.request_streams.insert(request_id, stream_id);
            self.request_stream_request_ids.insert(stream_id, request_id);
        }
    }

    /// draft 17+: REQUEST_OK/SUBSCRIBE_OK/REQUEST_ERROR/PUBLISH_OK drop Request ID
    /// from the wire (correlated by stream instead); this recovers it from the
    /// stream a message arrived on, for streams we opened ourselves.
    pub(crate) fn request_id_for_stream(&self, stream_id: u64) -> Option<RequestId> {
        self.request_stream_request_ids.get(&stream_id).copied()
    }

    /// Reads and decodes the next full message from `stream_id`, buffering
    /// partial reads internally. `selected_version` is `None` before the
    /// handshake completes, in which case the version to decode with is itself
    /// ambiguous — see `bootstrap_version_guess`.
    pub(crate) fn next_message(
        &mut self,
        stream_id: u64,
        selected_version: Option<Version>,
        setup_version: Version,
        t: &mut impl Transport,
    ) -> Result<ControlMessageEnum> {
        let control_stream = self.is_control_stream(stream_id);
        let buf = if control_stream {
            &mut self.ctrl_buf
        } else {
            self.request_stream_bufs.entry(stream_id).or_default()
        };
        let message = loop {
            let version = selected_version.unwrap_or_else(|| bootstrap_version_guess(&buf.0, setup_version));
            let mut r = version.reader(&buf.0);
            match ControlMessageEnum::from_bytes(&mut r, version) {
                Ok(v) => {
                    buf.0.drain(..r.consumed());
                    trace!("received message on stream {}: {:?}", stream_id, v);
                    break v;
                }
                Err(moq_wire::Error::Incomplete { needed }) if needed > MAX_CONTROL_MESSAGE_LEN => {
                    return Err(moq_wire::Error::ProtocolViolation(format!("control message of {needed} bytes")).into());
                }
                Err(moq_wire::Error::Incomplete { .. }) => buf.fill(stream_id, t)?,
                Err(e) => return Err(e.into()),
            }
        };
        // the peer's request streams begin with a request (draft-ietf-moq-transport-19 §3.3)
        if !control_stream && self.begun_request_streams.insert(stream_id) && !message.begins_request_stream() {
            return Err(moq_wire::Error::ProtocolViolation(format!("request stream {stream_id} beginning with {}", message.qlog_type_name())).into());
        }
        Ok(message)
    }
}

fn encode(cm: &ControlMessageEnum, version: Version) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    cm.to_bytes(&mut version.writer(&mut buf), version)?;
    Ok(buf)
}

/// Type, Length and the longest body draft 11+ allows; drafts 7–10 are held to it too.
const MAX_CONTROL_MESSAGE_LEN: usize = 8 + 8 + u16::MAX as usize;

/// A stream's received bytes not decoded yet, at most one control message.
#[derive(Default)]
struct MessageBuf(Vec<u8>);

impl MessageBuf {
    /// Appends what `stream_id` has, `Done` if nothing.
    fn fill(&mut self, stream_id: u64, t: &mut impl Transport) -> Result<()> {
        let len = self.0.len();
        if len == MAX_CONTROL_MESSAGE_LEN {
            return Err(moq_wire::Error::ProtocolViolation(format!("control message longer than {MAX_CONTROL_MESSAGE_LEN} bytes")).into());
        }
        self.0.resize((len + 1024).min(MAX_CONTROL_MESSAGE_LEN), 0);
        let read = t.recv_stream(stream_id, &mut self.0[len..]);
        self.0.truncate(len + read.as_ref().map_or(0, |&n| n));
        match read {
            Ok(0) | Err(StreamError::Done) => Err(Error::Done),
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Before the handshake completes, `next_message` doesn't know the real
/// version yet — only `setup_version`, a local default. SETUP's Type field is
/// a fixed, version-independent constant (0x40/0x41 for draft 07-10 vs
/// 0x20/0x21 for draft 11+), so peeking it tells us definitively which header
/// shape (variable-length vs. fixed `u16` Length) to expect — no guessing.
fn bootstrap_version_guess(buf: &[u8], setup_version: Version) -> Version {
    match moq_wire::MOQ_VERSION_DRAFT_07.reader(buf).varint() {
        Ok(ty)
            if ty == moq_wire::CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10
                || ty == moq_wire::SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10 =>
        {
            moq_wire::MOQ_VERSION_DRAFT_07
        }
        _ => setup_version,
    }
}
