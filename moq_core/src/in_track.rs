use alloc::collections::VecDeque;
use crate::prelude::*;
use hashbrown::HashMap;
use moq_wire::datagram::ObjectDatagram;
use crate::session::RecvObjHdr;


use core::time::Duration;

use log::debug;
use smallvec::SmallVec;
use moq_wire::{Location, TrackAlias};
use crate::time::Timestamp;

/// How long after PUBLISH_DONE a track waits for streams it can't account
/// for, e.g. reset before their header arrived (draft-16 §9.15).
pub(crate) const PUBLISH_DONE_STREAM_TIMEOUT: Duration = Duration::from_secs(5);

/// The stream an object header was handed out from, see
/// `InTrack::owned_locations`. A duplicate must match `subgroup_id` and
/// `payload_len` (draft-19 §9.1).
pub(crate) struct OwnedLocation {
    pub(crate) stream_id: u64,
    pub(crate) subgroup_id: u64,
    pub(crate) payload_len: usize,
}

pub(crate) struct InTrack {
    track_alias: TrackAlias,
    /// Readable stream ids, sorted ascending.
    readable_streams: SmallVec<u64, 10>,
    /// Number of streams fin'd so far.
    finned_streams: u64,
    /// Total streams expected and when PUBLISH_DONE announced them.
    pub(crate) expected_streams: Option<(u64, Timestamp)>,
    /// The stream that delivered each current object first, a second stream
    /// with the same `Location` (a Subgroup resent after a reset, draft-19
    /// §2.2) is checked against it instead of reaching the app twice (§9.1).
    /// - at most one entry per open stream, a later duplicate is left to the app's cache
    pub(crate) owned_locations: HashMap<Location, OwnedLocation>,
    /// Streams whose current object was a confirmed-consistent duplicate of
    /// one already in `owned_locations` — `moq_core` reads and discards
    /// their payload internally (never surfaced to the app) rather than
    /// delivering the same object twice. Maps stream_id to payload bytes
    /// still owed.
    pub(crate) duplicate_discard: HashMap<u64, usize>,
    /// Streams holding a duplicate of an object whose owner is still
    /// mid-payload, mapped to that object's `Location`. Left unread: if the
    /// owner completes they move to `duplicate_discard`, if it is reset one
    /// takes over (draft-16 §10.4.3: a reset subgroup is incomplete, so the
    /// truncated copy must not win).
    pub(crate) standby: HashMap<u64, Location>,
    /// Payload prefix a stream that took over an object must still drop,
    /// already delivered by the previous owner.
    pub(crate) handover_skip: HashMap<u64, usize>,
    /// Objects received in datagrams and their payloads, not yet handed out.
    datagrams: VecDeque<ReceivedDatagram>,
    /// What a datagram without its own inherits (draft-ietf-moq-transport-18 §12.4).
    default_publisher_priority: u8,
}

/// An Object received in a datagram, and its payload.
pub(crate) type ReceivedDatagram = (ObjectDatagram, Vec<u8>);

/// Datagrams of a track the app doesn't read: the oldest are dropped.
const MAX_QUEUED_DATAGRAMS: usize = 256;

impl InTrack {
    pub fn new(track_alias: TrackAlias, default_publisher_priority: u8) -> Self {
        Self {
            track_alias,
            readable_streams: SmallVec::new(),
            finned_streams: 0,
            expected_streams: None,
            owned_locations: HashMap::default(),
            duplicate_discard: HashMap::default(),
            standby: HashMap::default(),
            handover_skip: HashMap::default(),
            datagrams: VecDeque::new(),
            default_publisher_priority,
        }
    }

    pub(crate) fn push_datagram(&mut self, datagram: ObjectDatagram, payload: Vec<u8>) {
        if self.datagrams.len() == MAX_QUEUED_DATAGRAMS {
            debug!("track {:?} dropped a datagram not read", self.track_alias);
            self.datagrams.pop_front();
        }
        self.datagrams.push_back((datagram, payload));
    }

    /// The next Object received in a datagram, its fields resolved.
    pub(crate) fn pop_datagram(&mut self) -> Option<RecvObjHdr> {
        let (datagram, payload) = self.datagrams.pop_front()?;
        Some(RecvObjHdr::Datagram {
            location: datagram.location,
            publisher_priority: datagram.publisher_priority.unwrap_or(self.default_publisher_priority),
            end_of_group: datagram.end_of_group,
            status: datagram.status.unwrap_or_default(),
            properties: datagram.properties,
            payload,
        })
    }

    pub(crate) fn mark_done(&mut self, stream_count: u64, now: Timestamp) {
        self.expected_streams = Some((stream_count, now));
    }

    pub(crate) fn publish_done_received(&self) -> bool {
        self.expected_streams.is_some()
    }

    /// Every announced stream finished, or `PUBLISH_DONE_STREAM_TIMEOUT` passed.
    pub(crate) fn is_fully_done(&self, now: Timestamp) -> bool {
        self.expected_streams.is_some_and(|(count, at)| {
            self.finned_streams >= count || now.saturating_duration_since(at) >= PUBLISH_DONE_STREAM_TIMEOUT
        })
    }

    /// When `is_fully_done` turns true by timeout.
    pub(crate) fn done_deadline(&self) -> Option<Timestamp> {
        self.expected_streams
            .filter(|&(count, _)| self.finned_streams < count)
            .map(|(_, at)| at + PUBLISH_DONE_STREAM_TIMEOUT)
    }

    pub fn readable(&self) -> bool {
        !self.readable_streams.is_empty()
    }

    pub fn readable_streams(&self) -> &[u64] {
        &self.readable_streams
    }

    /// Retires `stream_id`: drops it from the readable set and every per-stream
    /// map. Must be called whenever a stream is removed from `in_streams`.
    pub(crate) fn fin_stream(&mut self, stream_id: u64) {
        if let Some(pos) = self.readable_streams.iter().position(|&i| i == stream_id) {
            self.readable_streams.remove(pos);
            self.finned_streams += 1;
        }
        self.owned_locations.retain(|_, o| o.stream_id != stream_id);
        self.duplicate_discard.remove(&stream_id);
        self.standby.remove(&stream_id);
        self.handover_skip.remove(&stream_id);
    }

    fn standby_streams(&self, location: Location) -> SmallVec<u64, 2> {
        self.standby.iter().filter(|&(_, &l)| l == location).map(|(&s, _)| s).collect()
    }

    /// The owner of `location` completed it: its standby duplicates are
    /// discarded in full.
    pub(crate) fn discard_standby(&mut self, location: Location, payload_len: usize) {
        for stream_id in self.standby_streams(location) {
            self.standby.remove(&stream_id);
            self.duplicate_discard.insert(stream_id, payload_len);
        }
    }

    /// Removes and returns the standby duplicates of `location`.
    pub(crate) fn take_standby(&mut self, location: Location) -> SmallVec<u64, 2> {
        let streams = self.standby_streams(location);
        for stream_id in &streams {
            self.standby.remove(stream_id);
        }
        streams
    }

    /// Hands `location` over to one of its standby streams, which first
    /// skips the `delivered` bytes the old owner already read.
    pub(crate) fn hand_over(&mut self, location: Location, delivered: usize) -> Option<u64> {
        let stream_id = *self.standby_streams(location).first()?;
        self.owned_locations.get_mut(&location)?.stream_id = stream_id;
        self.standby.remove(&stream_id);
        if delivered > 0 {
            self.handover_skip.insert(stream_id, delivered);
        }
        Some(stream_id)
    }

    pub fn mark_stream_readable(&mut self, stream_id: u64) {
        debug!("track {} readable stream {}", self.track_alias, stream_id);
        match self.readable_streams.binary_search(&stream_id) {
            Ok(_) => {}
            Err(pos) => self.readable_streams.insert(pos, stream_id),
        }
    }

}
