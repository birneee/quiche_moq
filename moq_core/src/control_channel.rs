//! `ControlChannel`: version-aware control-message I/O. Owns the shared
//! control channel (a single bidirectional stream for draft < 17, a pair of
//! unidirectional streams for draft 17+) and, for draft 17+, the per-request
//! bidi streams SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE each get.
//!
//! This is a self-contained type with its own state so that
//! `MoqTransportSession` doesn't need to hold half a dozen closely-related
//! fields or re-derive the topology at every call site — it just calls
//! `send_request`/`send_response`/`send_control`/`next_message` and this type
//! decides (from its own state, set once) which stream(s) to use.

use crate::error::{Error, Result};
use crate::transport::{StreamError, Transport};
use log::{debug, trace};
use moq_wire::control_message::ControlMessageEnum;
use moq_wire::{FromBytes, RequestId, ToBytes, Version, MOQ_VERSION_DRAFT_17, MOQ_VERSION_DRAFT_18};
use octets::{Octets, OctetsMut};
use short_buf::ShortBuf;
use std::collections::{HashMap, VecDeque};

#[cfg(feature = "qlog")]
pub(crate) fn cm_qlog_message(cm: &ControlMessageEnum) -> serde_json::Value {
    use moq_wire::control_message::GroupOrder;
    let mut msg = serde_json::json!({ "type": cm.qlog_type_name() });
    if let ControlMessageEnum::SubscribeOk(som) = cm {
        msg["request_id"] = som.request_id().into();
        if let Some(ta) = som.track_alias() {
            msg["track_alias"] = ta.into();
        }
        let mut params: Vec<serde_json::Value> = vec![];
        if som.expires() != 0 {
            params.push(serde_json::json!({ "name": "expires", "value": som.expires() }));
        }
        params.push(serde_json::json!({
            "name": "group_order",
            "value": match som.group_order() { GroupOrder::Ascending => 1u64, GroupOrder::Descending => 2u64 },
        }));
        if let Some(loc) = som.largest_location() {
            params.push(serde_json::json!({
                "name": "largest_object",
                "value": { "group": loc.group, "object": loc.object },
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
/// stream with a pair of unidirectional streams, one opened by each peer. Matched
/// by exact version, not `>=`: MOQ_VERSION_LITE_01_BY_KIXELATED is numerically
/// larger than the draft constants despite not being a newer draft.
fn uses_uni_pair(version: Version) -> bool {
    matches!(version, MOQ_VERSION_DRAFT_17 | MOQ_VERSION_DRAFT_18)
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
///   inbound uni stream is discovered from `readable` once it arrives.
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
    ctrl_buf: ShortBuf<1024>,
    pending_control_messages: VecDeque<Vec<u8>>,
    /// draft 17+ only: dedicated bidi stream per SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE
    /// request, keyed by request_id. Populated for requests we sent (`send_request`)
    /// and requests we received (`record_request_stream_if_needed`).
    /// REQUEST_UPDATE/PUBLISH_DONE follow-ups on these streams are not yet
    /// implemented — see `uses_per_request_streams()` call sites in session.rs.
    request_streams: HashMap<RequestId, u64>,
    /// Reverse of `request_streams`, for routing data that arrives on a request stream.
    request_stream_request_ids: HashMap<u64, RequestId>,
    /// Partial-read buffers for `request_streams`' streams.
    request_stream_bufs: HashMap<u64, ShortBuf<1024>>,
    /// Encoded messages queued per request stream (mirrors `pending_control_messages`).
    pending_request_messages: HashMap<u64, VecDeque<Vec<u8>>>,
}

impl ControlChannel {
    /// Client-side: opens the channel (a bidi stream, or draft 17+'s uni-pair
    /// out-stream) immediately.
    pub(crate) fn connect(t: &mut impl Transport, setup_version: Version) -> Self {
        let topology = if uses_uni_pair(setup_version) {
            Topology::UniPair { out_id: t.open_uni_stream().unwrap(), in_id: None }
        } else {
            Topology::Bidi(t.open_bidi_stream().unwrap())
        };
        Self {
            topology: Some(topology),
            ctrl_buf: ShortBuf::new(),
            pending_control_messages: VecDeque::new(),
            request_streams: HashMap::new(),
            request_stream_request_ids: HashMap::new(),
            request_stream_bufs: HashMap::new(),
            pending_request_messages: HashMap::new(),
        }
    }

    /// Server-side: no I/O yet — the topology depends on what the client sends,
    /// discovered later via `try_establish`.
    pub(crate) fn accept() -> Self {
        Self {
            topology: None,
            ctrl_buf: ShortBuf::new(),
            pending_control_messages: VecDeque::new(),
            request_streams: HashMap::new(),
            request_stream_request_ids: HashMap::new(),
            request_stream_bufs: HashMap::new(),
            pending_request_messages: HashMap::new(),
        }
    }

    /// Server-side: establish the channel's topology by observing what the
    /// client actually sent, not a local version guess. No-op once established
    /// (including for the client, which already did this in `connect`).
    /// Returns `false` if nothing establishing it has arrived yet.
    pub(crate) fn try_establish(&mut self, readable: &[u64], t: &mut impl Transport) -> bool {
        if self.topology.is_none() {
            if let Some(id) = readable.iter().copied().find(|&id| is_bidi_stream(id)) {
                self.topology = Some(Topology::Bidi(id));
            } else if let Some(id) = readable.iter().copied().find(|&id| !is_bidi_stream(id)) {
                self.topology = Some(Topology::UniPair {
                    out_id: t.open_uni_stream().unwrap(),
                    in_id: Some(id),
                });
            } else {
                return false;
            }
        }
        // For UniPair, discover the peer's inbound stream once it arrives (both
        // client and server start out not knowing it).
        if let Some(Topology::UniPair { in_id: in_id @ None, .. }) = &mut self.topology
            && let Some(id) = readable.iter().copied().find(|&id| !is_bidi_stream(id))
        {
            *in_id = Some(id);
        }
        true
    }

    /// True if `stream_id` should be routed through `next_message` (this
    /// channel's inbound stream, or — draft 17+ only — any bidi stream, since
    /// those only ever carry per-request control traffic, never media data)
    /// rather than treated as a media data stream.
    pub(crate) fn owns(&self, stream_id: u64) -> bool {
        self.topology.and_then(|c| c.in_id()) == Some(stream_id) || is_bidi_stream(stream_id)
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
    /// Per-request-stream queues flush inline as part of `send_request`/
    /// `send_response` instead — see `encode_and_queue`.
    pub(crate) fn flush(&mut self, t: &mut impl Transport) -> Result<()> {
        let Some(topology) = self.topology else { return Ok(()) };
        Self::flush_queue(&mut self.pending_control_messages, topology.out_id(), t)
    }

    fn flush_queue(queue: &mut VecDeque<Vec<u8>>, stream_id: u64, t: &mut impl Transport) -> Result<()> {
        while let Some(msg) = queue.front() {
            match t.send_stream(stream_id, msg, false) {
                Ok(n) => {
                    assert_eq!(n, msg.len());
                    queue.pop_front();
                }
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
        let mut b = [0u8; 1024];
        let mut o = OctetsMut::with_slice(&mut b);
        cm.to_bytes(&mut o, version).unwrap();
        let len = o.off();
        queue.push_back(b[..len].to_vec());
        Self::flush_queue(queue, stream_id, t)?;
        #[cfg(feature = "qlog")]
        if let Some(qlog) = t.qlog_streamer() {
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
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
    pub(crate) fn send_control(&mut self, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) {
        let Some(topology) = self.topology else { panic!("control channel not opened yet") };
        Self::encode_and_queue(&mut self.pending_control_messages, topology.out_id(), version, cm, t).unwrap();
    }

    /// Sends a request-initiating message (SUBSCRIBE/PUBLISH/PUBLISH_NAMESPACE):
    /// on draft 17+ this opens a dedicated bidi stream for it and remembers the
    /// stream<->request_id mapping (so the response can be routed back), matching
    /// the "requests begin their own stream" model; on older drafts it's just
    /// queued on the shared control channel like any other message.
    pub(crate) fn send_request(&mut self, request_id: RequestId, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) {
        if self.uses_per_request_streams() {
            let stream_id = t.open_bidi_stream().unwrap();
            self.request_streams.insert(request_id, stream_id);
            self.request_stream_request_ids.insert(stream_id, request_id);
            let queue = self.pending_request_messages.entry(stream_id).or_default();
            Self::encode_and_queue(queue, stream_id, version, cm, t).unwrap();
        } else {
            self.send_control(cm, version, t);
        }
    }

    /// Sends a response (SUBSCRIBE_OK/PUBLISH_OK/REQUEST_ERROR/...) for
    /// `request_id`, routing it to the request's dedicated stream if one is
    /// known (draft 17+), or the shared control channel otherwise.
    pub(crate) fn send_response(&mut self, request_id: RequestId, cm: &ControlMessageEnum, version: Version, t: &mut impl Transport) {
        match self.request_streams.get(&request_id) {
            Some(&stream_id) => {
                let queue = self.pending_request_messages.entry(stream_id).or_default();
                Self::encode_and_queue(queue, stream_id, version, cm, t).unwrap();
            }
            None => self.send_control(cm, version, t),
        }
    }

    /// draft 17+ only: if `stream_id` isn't the control channel, remember it as
    /// `request_id`'s dedicated stream (for routing the eventual response, or a
    /// response we're about to send).
    pub(crate) fn record_request_stream_if_needed(&mut self, stream_id: u64, request_id: RequestId) {
        if self.topology.and_then(|c| c.in_id()) != Some(stream_id) {
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
        let control_in_id = self.topology.and_then(|c| c.in_id());
        let buf = if Some(stream_id) == control_in_id {
            &mut self.ctrl_buf
        } else {
            self.request_stream_bufs.entry(stream_id).or_default()
        };
        loop {
            let version = selected_version.unwrap_or_else(|| bootstrap_version_guess(buf.buffer(), setup_version));
            let mut o = Octets::with_slice(buf.buffer());
            match ControlMessageEnum::from_bytes(&mut o, version) {
                Ok(v) => {
                    buf.consume(o.off());
                    trace!("received message on stream {}: {:?}", stream_id, v);
                    return Ok(v);
                }
                Err(moq_wire::Error::Octets(octets::BufferTooShortError)) => {
                    match buf.fill(|b| t.recv_stream(stream_id, b)) {
                        Ok(_) => {}
                        Err(StreamError::Done) => return Err(Error::Done),
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => unimplemented!("{:?}", e),
            }
        }
    }
}

/// Before the handshake completes, `next_message` doesn't know the real
/// version yet — only `setup_version`, a local default. SETUP's Type field is
/// a fixed, version-independent constant (0x40/0x41 for draft 07-10 vs
/// 0x20/0x21 for draft 11+), so peeking it tells us definitively which header
/// shape (variable-length vs. fixed `u16` Length) to expect — no guessing.
fn bootstrap_version_guess(buf: &[u8], setup_version: Version) -> Version {
    let mut o = Octets::with_slice(buf);
    match moq_wire::vi64::peek_varint(&mut o, moq_wire::MOQ_VERSION_DRAFT_07) {
        Ok(ty)
            if ty == moq_wire::CLIENT_SETUP_MESSAGE_ID_VERSION_UNTIL_10
                || ty == moq_wire::SERVER_SETUP_MESSAGE_ID_VERSION_UNTIL_10 =>
        {
            moq_wire::MOQ_VERSION_DRAFT_07
        }
        _ => setup_version,
    }
}
