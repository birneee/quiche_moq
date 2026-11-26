use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;

use moq_core::session::{MoqTransportSession, RecvObjHdr};
use moq_core::transport::Transport;
use moq_core::wire::{KeyValuePairs, Location, TrackAlias};
use moq_core::{DropQueue, Error};
use moq_object_cache::{begin_or_join, put_datagram, NewObject, ObjectCache, ObjectState, PackedObjectCache};

use crate::in_track::InTrack;

/// A complete, reassembled object — the payload may have arrived across
/// several network reads; `BufferedTrack` only hands this back once
/// `payload.len()` matches what the header declared.
pub struct CompleteObject {
    pub location: Location,
    pub properties: KeyValuePairs,
    pub payload: Vec<u8>,
}

/// Counters of a [`BufferedTrack`] and what its cache holds now.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TrackStats {
    /// Objects and payload bytes taken by the consumer.
    pub objects: u64,
    pub bytes: u64,
    /// Objects without room in the cache, their payload read and dropped.
    pub discarded: u64,
    /// Objects the playout passed over: reset mid-payload, evicted before they were taken, or complete only
    /// after a later one was taken.
    pub skipped: u64,
    pub cached: usize,
    /// Cached objects whose payload is still arriving.
    pub progressing: usize,
    /// Complete objects after the playout position, the buffer ahead of the consumer.
    pub ready: usize,
    /// The last object taken and the newest one cached.
    pub played: Option<Location>,
    pub newest: Option<Location>,
}

/// Complete objects of an [`InTrack`], reassembled in a `moq_object_cache`
/// that is also the consumer's playout buffer: [`advance`](Self::advance)
/// reads the wire into it, [`take_ready`](Self::take_ready) takes objects
/// out in location order. [`InTrack`] reads partial objects without buffering.
pub struct BufferedTrack {
    track: InTrack,
    cache: PackedObjectCache,
    /// Objects whose header we saw but couldn't fit in `cache` (capacity or
    /// ring-buffer-space conflict — see `PackedObjectCache::begin`) — their
    /// payload still has to be read off the wire even though there's
    /// nowhere to put it. Maps to bytes still owed.
    discarding: HashMap<Location, usize>,
    /// Scratch read target for `discarding` payload — `read_obj_pld` still
    /// needs a real, non-empty `dest` to know how many bytes it may read.
    discard_scratch: Vec<u8>,
    /// Cached objects whose payload is still arriving: evicting one strands its stream.
    incomplete: HashSet<Location>,
    /// The last object taken, playout only moves forward.
    played: Option<Location>,
    stats: TrackStats,
}

impl BufferedTrack {
    /// `max_objects`/`max_object_size`/`average_object_size` configure the
    /// underlying `PackedObjectCache` — see its own docs for what these
    /// trade off (this is exactly the buffering *policy* `moq_core` itself
    /// deliberately doesn't impose; callers who want a different one should
    /// use `InTrack` directly instead of this wrapper).
    pub fn new(track_alias: TrackAlias, max_objects: usize, max_object_size: usize, average_object_size: usize) -> Self {
        Self {
            track: InTrack::new(track_alias),
            cache: PackedObjectCache::new(max_objects, max_object_size, average_object_size),
            discarding: HashMap::new(),
            discard_scratch: vec![0u8; 64 << 10],
            incomplete: HashSet::new(),
            played: None,
            stats: TrackStats::default(),
        }
    }

    /// 96 objects, up to 4 MiB each, a buffer of 96 × 64 KiB: the cache is the playout buffer, 96 objects are
    /// 1.6 s of 60 fps video.
    pub fn with_defaults(track_alias: TrackAlias) -> Self {
        Self::new(track_alias, 96, 4 << 20, 64 << 10)
    }

    /// Reads everything available into the cache, [`take_ready`](Self::take_ready) takes the objects.
    pub fn advance<Q: DropQueue>(&mut self, session: &mut MoqTransportSession<Q>, t: &mut impl Transport) {
        // Pick up every new object header available this tick — independent
        // Subgroups have no ordering relationship, so several may have
        // started at once.
        loop {
            match self.track.new_obj_hdr(session, t) {
                Ok(RecvObjHdr::Header { location: loc, subgroup_id, publisher_priority, header: hdr, .. }) => match begin_or_join(&mut self.cache, NewObject { location: loc, subgroup_id, publisher_priority, end_of_subgroup: false, properties: hdr.properties().clone(), payload_len: hdr.payload_len() }) {
                    Ok(evicted) => {
                        self.evicted(evicted, session, t);
                        // a copy of an object already complete
                        if hdr.payload_len() > 0 && self.cache.find(loc).is_some_and(|o| o.state() == ObjectState::Completed) {
                            self.discarding.insert(loc, hdr.payload_len());
                        } else if hdr.payload_len() > 0 {
                            self.incomplete.insert(loc);
                        }
                    }
                    Err(e) => {
                        log::warn!("discarding object at {loc:?}: {e:?}");
                        self.stats.discarded += 1;
                        if hdr.payload_len() > 0 {
                            self.discarding.insert(loc, hdr.payload_len());
                        }
                    }
                },
                Ok(RecvObjHdr::Datagram { location, publisher_priority, end_of_group, status, properties, payload }) => {
                    match put_datagram(&mut self.cache, location, publisher_priority, end_of_group, status, properties, &payload) {
                        Ok(evicted) => {
                            self.evicted(evicted, session, t);
                            if self.played.is_some_and(|played| location <= played) {
                                self.stats.skipped += 1;
                            }
                        }
                        Err(e) => {
                            log::warn!("discarding object at {location:?}: {e:?}");
                            self.stats.discarded += 1;
                        }
                    }
                }
                // Not used for straight playback — no forwarding to signal.
                Ok(RecvObjHdr::SubgroupFinished { .. } | RecvObjHdr::SubgroupReset { .. }) => continue,
                Err(Error::Done) | Err(Error::Fin) => break,
                Err(e) => {
                    log::warn!("track intake stopped: {e:?}");
                    break;
                }
            }
        }

        // Drain payload for anything we couldn't fit in the cache.
        let discarding: Vec<Location> = self.discarding.keys().copied().collect();
        for loc in discarding {
            let remaining = self.discarding[&loc];
            let want = remaining.min(self.discard_scratch.len());
            match self.track.obj_pld(loc, session, t, &mut self.discard_scratch[..want]) {
                Ok(n) => {
                    let left = self.discarding.get_mut(&loc).unwrap();
                    *left -= n;
                    if *left == 0 {
                        self.discarding.remove(&loc);
                    }
                }
                Err(Error::Fin) => {
                    self.discarding.remove(&loc);
                }
                Err(_) => {}
            }
        }

        // Drain payload for every cached object still incomplete.
        moq_object_cache::scan_objects::<()>(
            &mut self.cache,
            |loc, obj| {
                match self.track.obj_pld(loc, session, t, obj.unfilled()) {
                    Ok(n) => obj.advance_write_pos(n),
                    Err(Error::Fin) => obj.cancel(),
                    Err(_) => {}
                }
                ControlFlow::Continue(())
            },
            |obj| {
                let loc = obj.location();
                if self.incomplete.remove(&loc) && obj.state() == ObjectState::Completed && self.played.is_some_and(|played| loc <= played) {
                    self.stats.skipped += 1;
                }
                ControlFlow::Continue(())
            },
        );
    }

    /// Evicted objects: one still arriving would strand its stream, the session would wait for a payload read
    /// that never comes, so its stream is stopped. A complete one's stream may carry later objects of its Subgroup.
    fn evicted<Q: DropQueue>(&mut self, evicted: Vec<Location>, session: &mut MoqTransportSession<Q>, t: &mut impl Transport) {
        for loc in evicted {
            if self.incomplete.remove(&loc) {
                self.track.cancel_obj(loc, session, t);
                self.stats.skipped += 1;
            } else if self.played.is_none_or(|played| loc > played) {
                self.stats.skipped += 1;
            }
        }
    }

    /// The next object in location order: waits for one still arriving, passes over a reset one and doesn't wait
    /// for one never seen, so playout never goes back and never stalls on a lost object. An object that completes
    /// after a later one was taken is skipped.
    pub fn take_ready(&mut self) -> Option<CompleteObject> {
        loop {
            let obj = self.cache.next_after(self.played)?;
            let location = obj.location();
            match obj.state() {
                ObjectState::Progressing => return None,
                ObjectState::Canceled => {
                    self.played = Some(location);
                    self.stats.skipped += 1;
                }
                ObjectState::Completed => {
                    let object = CompleteObject { location, properties: obj.properties().clone(), payload: obj.readable_from(0).to_vec() };
                    self.played = Some(location);
                    self.stats.objects += 1;
                    self.stats.bytes += object.payload.len() as u64;
                    return Some(object);
                }
            }
        }
    }

    pub fn stats(&self) -> TrackStats {
        let mut stats = TrackStats { played: self.played, ..self.stats };
        let mut cursor = None;
        while let Some(obj) = self.cache.next_after(cursor) {
            let location = obj.location();
            cursor = Some(location);
            stats.cached += 1;
            stats.newest = Some(location);
            match obj.state() {
                ObjectState::Progressing => stats.progressing += 1,
                ObjectState::Completed if self.played.is_none_or(|played| location > played) => stats.ready += 1,
                _ => {}
            }
        }
        stats
    }
}

#[cfg(test)]
mod tests {
    use moq_core::test_utils::{init_mock_pipe, subscribed_track, MockPipe};
    use moq_core::wire::{Location, TrackAlias};
    use moq_core::{Config, OutgoingObject};

    use super::BufferedTrack;

    fn loc(group: u64, object: u64) -> Location {
        Location { group, object }
    }

    /// Sends `sent` of an object's `len` payload bytes on its own subgroup.
    fn send(p: &mut MockPipe, track_alias: TrackAlias, location: Location, len: usize, sent: usize) {
        p.server.send_obj_hdr(track_alias, OutgoingObject::new(location, location.object, len), &mut p.server_t).unwrap();
        p.server.send_obj_pld(track_alias, location, &vec![0; sent], &mut p.server_t).unwrap();
    }

    fn setup(max_objects: usize) -> (MockPipe, TrackAlias, BufferedTrack) {
        let mut p = init_mock_pipe(Config::default());
        let track_alias = subscribed_track(&mut p);
        (p, track_alias, BufferedTrack::new(track_alias, max_objects, 1 << 10, 1 << 10))
    }

    fn advance(p: &mut MockPipe, track: &mut BufferedTrack) {
        p.advance();
        track.advance(&mut p.client, &mut p.client_t);
    }

    #[test]
    fn playout_waits_for_an_earlier_object_still_arriving() {
        let (mut p, track_alias, mut track) = setup(8);
        send(&mut p, track_alias, loc(0, 0), 2, 1);
        send(&mut p, track_alias, loc(0, 1), 1, 1);
        advance(&mut p, &mut track);
        assert!(track.take_ready().is_none());
        p.server.send_obj_pld(track_alias, loc(0, 0), &[0], &mut p.server_t).unwrap();
        advance(&mut p, &mut track);
        assert_eq!(track.take_ready().map(|o| o.location), Some(loc(0, 0)));
        assert_eq!(track.take_ready().map(|o| o.location), Some(loc(0, 1)));
    }

    #[test]
    fn playout_does_not_wait_for_an_object_never_seen_and_skips_it_late() {
        let (mut p, track_alias, mut track) = setup(8);
        send(&mut p, track_alias, loc(0, 1), 1, 1);
        advance(&mut p, &mut track);
        assert_eq!(track.take_ready().map(|o| o.location), Some(loc(0, 1)));
        send(&mut p, track_alias, loc(0, 0), 1, 1);
        advance(&mut p, &mut track);
        assert!(track.take_ready().is_none());
        assert_eq!(track.stats().skipped, 1);
    }

    #[test]
    fn evicting_an_object_still_arriving_stops_its_stream() {
        let (mut p, track_alias, mut track) = setup(1);
        send(&mut p, track_alias, loc(0, 0), 2, 1);
        advance(&mut p, &mut track);
        assert!(p.client.obj_pld_received(track_alias, loc(0, 0)).is_some());
        send(&mut p, track_alias, loc(0, 1), 1, 1);
        advance(&mut p, &mut track);
        assert!(p.client.obj_pld_received(track_alias, loc(0, 0)).is_none());
        assert_eq!(track.stats().skipped, 1);
        assert_eq!(track.take_ready().map(|o| o.location), Some(loc(0, 1)));
    }
}
