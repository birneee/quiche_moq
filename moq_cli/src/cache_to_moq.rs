//! Per-object forwarding loop, cache → MoQ session, one per `LocalAliasState`
//! (any `--pub` track or relayed one). See `moq_object_cache::{begin_or_join,
//! fold}` for the mirror image (session → cache).
//!
//! - Subgroup ID / `end_of_subgroup` come from the cache — set by whoever
//!   knows: a local publisher's own Group boundary, or a relay observing
//!   `RecvObjHdr::SubgroupFinished` or `SubgroupReset` upstream. Reused as-is.
//! - Physical stream placement is `moq_core`'s problem (`send_obj_hdr`
//!   resolves it) — state here is keyed by `Location`, not stream identity.
//! - send progress is `moq_core`'s, see `MoqTransportSession::obj_pld_remaining`

use quiche_moq::{OutgoingDatagram, OutgoingObject};
use std::collections::{HashMap, HashSet};

use log::{debug, error};
use moq_object_cache::{CachedObjectRef, ObjectCache, ObjectState};

use quiche_moq::wire::{Location, ResetCode, TrackAlias};
use quiche_moq_webtransport_helper::MoqHandle;
use crate::local_track::FilterRange;

/// `Location`-keyed sets/maps on the per-tick hot path (`forward` rescans
/// the whole cache against these every tick, per subscriber), FxHash for speed.
// TODO FxHash isn't flood resistant and the Locations come from upstream:
//  measure a keyed hasher (hashbrown's default) on the testbed
type FastSet<T> = HashSet<T, rustc_hash::FxBuildHasher>;
type FastMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;

/// Per-alias/subscriber cache-to-session forwarding state.
#[derive(Default)]
pub(crate) struct CacheToMoq {
    /// Only forward Locations within the SUBSCRIPTION_FILTER, set at
    /// subscribe time or by a filter update (`refilter`). The start doesn't
    /// grow (unlike `completed`): out-of-order Subgroup discovery means a
    /// lower Location can still need sending after a higher one already went out.
    /// - an `AbsoluteRange` ends once delivered, see `range_delivered`
    pub(crate) range: FilterRange,
    /// Header sent, payload not fully out yet. Send progress itself lives in
    /// `moq_core` (`MoqHandle::obj_pld_remaining`) — this is just membership.
    pub(crate) in_flight: FastSet<Location>,
    /// Fully forwarded (or skipped as `ObjectExpired` or `ObjectFiltered`) — don't resend.
    /// Needed since `forward` rescans the whole cache each tick rather than
    /// a high-water mark (see `range`).
    pub(crate) completed: FastSet<Location>,
    /// Source evicted (`mark_evicted`) or forwarding turned off mid-send
    /// (see `crate::node::apply_subscription_update`) while still sending —
    /// reset (with the given RESET_STREAM code) next `forward` call.
    pub(crate) needs_reset: Vec<(Location, ResetCode)>,
    /// Forward=0 (see `crate::node::apply_subscription_update`): reset every
    /// not fully delivered stream on next `drain_resets`.
    /// - wider than `needs_reset`: covers objects still in the send buffer
    pub(crate) cancel_unfinished: bool,
    /// Fully sent while `end_of_subgroup` still `false` — `push_sample`'s
    /// multi-object groups only learn the boundary retroactively (next
    /// Group's first sample). Re-checked each `forward` call to FIN once
    /// confirmed, or the stream leaks.
    pub(crate) awaiting_fin: FastMap<Location, u64>,
    /// `cache.generation()` as of the last full header-send scan (see below) —
    /// `None` forces the first call to always scan. Together with
    /// `header_scan_needs_retry`, lets `forward` skip that scan (normally
    /// O(cache depth) per call) on ticks where nothing could have changed
    /// its outcome, which is the common case: object arrival is far slower
    /// than the tick rate.
    pub(crate) last_scanned_generation: Option<u64>,
    /// Set when the last header-send scan stopped early on backpressure
    /// (`Done`/`InsufficientCapacity`) or a send error — must retry even if
    /// the cache hasn't changed, since nothing else will prompt it. Cleared
    /// once a scan runs to completion.
    pub(crate) header_scan_needs_retry: bool,
    /// Reused scratch buffer for `forward`'s payload pass.
    drain_order: Vec<Location>,
}

impl CacheToMoq {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn filtered(range: FilterRange) -> Self {
        Self { range, ..Self::default() }
    }

    /// The `AbsoluteRange`'s end group is over and every cached object in
    /// range is forwarded, its streams FIN'd: time for PUBLISH_DONE.
    /// - the end group is over at its end of subgroup, or once a later group arrived
    pub(crate) fn range_delivered(&self, cache: &impl ObjectCache) -> bool {
        let Some(end_group) = self.range.end_group else { return false };
        if !self.in_flight.is_empty() || !self.awaiting_fin.is_empty() {
            return false;
        }
        for obj in cache.objects_after(None) {
            let loc = obj.location();
            if loc.group > end_group {
                return true;
            }
            if self.range.contains(loc) && !self.completed.contains(&loc) {
                return false;
            }
            if loc.group == end_group && obj.end_of_subgroup() {
                return true;
            }
        }
        false
    }

    /// REQUEST_UPDATE filter change (draft-16 §9.11.1): applies to objects
    /// not sent yet, resets those in flight outside the new range, keeps what
    /// was already delivered.
    pub(crate) fn refilter(&mut self, range: FilterRange) {
        let outside = self.in_flight.iter().filter(|&&loc| !range.contains(loc));
        self.needs_reset.extend(outside.map(|&loc| (loc, ResetCode::Canceled)));
        self.range = range;
        self.last_scanned_generation = None;
    }

    /// Marks `loc` as fully forwarded, ending its Subgroup's stream if it's
    /// already known to be the last object of that Subgroup — otherwise
    /// queues it in `awaiting_fin` to keep checking (see its docs).
    fn complete(&mut self, moq: &mut MoqHandle, alias: TrackAlias, loc: Location, subgroup_id: u64, end: SubgroupEnd) {
        self.completed.insert(loc);
        if !end.apply(moq, alias, loc.group, subgroup_id) {
            self.awaiting_fin.insert(loc, subgroup_id);
        }
    }
}

/// A later object of `loc`'s Subgroup was forwarded: the stream went on, its end is that object's.
fn later_in_subgroup_forwarded(cache: &impl ObjectCache, completed: &FastSet<Location>, loc: Location, subgroup_id: u64) -> bool {
    cache.objects_after(Some(loc))
        .take_while(|obj| obj.location().group == loc.group)
        .any(|obj| obj.subgroup_id() == subgroup_id && completed.contains(&obj.location()))
}

/// How a forwarded object's Subgroup ends, as far as the cache knows.
#[derive(Clone, Copy)]
enum SubgroupEnd {
    Open,
    Finished,
    /// upstream reset: a FIN would claim the Subgroup is complete
    Reset,
}

impl SubgroupEnd {
    fn of(obj: &CachedObjectRef) -> Self {
        if obj.subgroup_reset() {
            Self::Reset
        } else if obj.end_of_subgroup() {
            Self::Finished
        } else {
            Self::Open
        }
    }

    /// Ends the Subgroup's stream if known, returns whether it did.
    fn apply(self, moq: &mut MoqHandle, alias: TrackAlias, group: u64, subgroup_id: u64) -> bool {
        match self {
            Self::Open => false,
            Self::Finished => {
                moq.session.fin_send_subgroup(alias, group, subgroup_id, &mut moq.t);
                true
            }
            Self::Reset => {
                moq.session.reset_send_subgroup(alias, group, subgroup_id, ResetCode::Canceled, &mut moq.t);
                true
            }
        }
    }
}

/// Flags every `state` still sending `evicted`, forgets it as `completed` —
/// keeps both bounded. Call with a cache `begin()`'s eviction result;
/// no-op if `None`.
pub(crate) fn mark_evicted<'a>(states: impl Iterator<Item = &'a mut CacheToMoq>, evicted: &[Location]) {
    if evicted.is_empty() { return }
    for state in states {
        for &loc in evicted {
            if state.in_flight.contains(&loc) {
                state.needs_reset.push((loc, ResetCode::DeliveryTimeout));
            }
            state.completed.remove(&loc);
            state.awaiting_fin.remove(&loc);
        }
    }
}

/// Resets every stream queued in `state.needs_reset` (cache eviction, or
/// Forward State flipping to 0 mid-send — `apply_subscription_update`). Split
/// out from `forward`: runs even when `forward` itself is skipped for a
/// non-forwarding alias (`forward_local_alias`).
pub(crate) fn drain_resets(state: &mut CacheToMoq, moq: &mut MoqHandle, alias: TrackAlias) -> bool {
    let cancel = std::mem::take(&mut state.cancel_unfinished);
    if cancel {
        moq.session.cancel_unfinished_streams(alias, ResetCode::Canceled, &mut moq.t);
        state.in_flight.clear();
        state.awaiting_fin.clear();
    }
    let had_resets = !state.needs_reset.is_empty();
    for (loc, code) in state.needs_reset.drain(..) {
        moq.session.cancel_send_obj(alias, loc, code, &mut moq.t);
        state.in_flight.remove(&loc);
    }
    cancel || had_resets
}

/// One forwarding round for `state` against `cache`: reset flagged evictions,
/// drain payload for headers already sent, send headers for new cached
/// objects. Returns whether anything happened. `label` is for log messages.
///
/// Several objects can be in flight at once — independent Subgroups have no
/// ordering in MoQT; `send_obj_hdr`/`send_obj_pld` (by `Location`, not
/// stream) resolve the physical stream.
pub(crate) fn forward(
    cache: &impl ObjectCache,
    state: &mut CacheToMoq,
    moq: &mut MoqHandle,
    alias: TrackAlias,
    label: impl std::fmt::Display,
) -> bool {
    let mut progress = drain_resets(state, moq, alias);
    // Whether anything left `in_flight` this tick — the only thing that can
    // *unblock* a not-yet-started object (`subgroup_busy` below), so the
    // only in-tick reason (besides new cache data) the header-send scan
    // could need redoing. `drain_resets` above already removes from
    // `in_flight` too.
    let mut in_flight_freed = progress;

    // Re-check objects finished before the cache knew their Subgroup ended.
    if !state.awaiting_fin.is_empty() {
        let completed = &state.completed;
        state.awaiting_fin.retain(|&loc, &mut subgroup_id| {
            let Some(obj) = cache.find(loc) else { return false };
            if later_in_subgroup_forwarded(cache, completed, loc, subgroup_id) {
                return false;
            }
            !SubgroupEnd::of(&obj).apply(moq, alias, loc.group, subgroup_id)
        });
    }

    // Drain payload for every object already header'd.
    // - lowest Location first (draft-19 §7.2): first writer takes `tx_cap`
    let mut drain_order = std::mem::take(&mut state.drain_order);
    drain_order.clear();
    drain_order.extend(state.in_flight.iter().copied());
    drain_order.sort_unstable();
    for &loc in &drain_order {
        let Some(obj) = cache.find(loc) else { continue };
        // The upstream source was reset/dropped after this object's header
        // was already sent downstream (see `ObjectMeta::cancel`) — its
        // payload will never actually reach `payload_len`, so there's
        // nothing left to drain that `moq_core`'s `remaining` count agrees
        // with. Abandon the downstream stream instead of trying to drain it.
        if obj.state() == ObjectState::Canceled {
            moq.session.cancel_send_obj(alias, loc, ResetCode::DeliveryTimeout, &mut moq.t);
            state.in_flight.remove(&loc);
            progress = true;
            in_flight_freed = true;
            continue;
        }
        // Own send progress, sourced from moq_core (which already tracks
        // it) rather than duplicating a counter here.
        let Some(remaining) = moq.session.obj_pld_remaining(alias, loc) else { continue };
        let read_pos = obj.payload_len() - remaining;
        let available = obj.readable_from(read_pos);
        if available.is_empty() { continue; }
        let end = SubgroupEnd::of(&obj);
        let subgroup_id = obj.subgroup_id();
        match moq.session.send_obj_pld(alias, loc, available, &mut moq.t) {
            Ok(n) => {
                if n == 0 { continue; }
                progress = true;
                if n == remaining {
                    state.in_flight.remove(&loc);
                    state.complete(moq, alias, loc, subgroup_id, end);
                    in_flight_freed = true;
                }
            }
            Err(moq_core::Error::Done | moq_core::Error::InsufficientCapacity) => {}
            // moq_core already tore this stream down (e.g. expiry scan) — stop tracking, don't retry forever.
            Err(moq_core::Error::SubgroupNotOpen) => {
                state.in_flight.remove(&loc);
                state.completed.insert(loc);
                in_flight_freed = true;
            }
            // e.g. STOP_SENDING from the subscriber: give up on this object,
            // else its subgroup stays busy
            Err(e) => {
                error!("{label} send_obj_pld: {e:?}");
                moq.session.cancel_send_obj(alias, loc, ResetCode::Canceled, &mut moq.t);
                state.in_flight.remove(&loc);
                state.completed.insert(loc);
                in_flight_freed = true;
            }
        }
    }
    state.drain_order = drain_order;

    // Headers for not yet started objects. A full rescan, not a high-water
    // mark: a lower Location can still need sending after a higher one.
    // - skipped while nothing could change its outcome: no new cache object
    //   (`cache.generation()`), nothing left `in_flight`, no retry pending
    let cache_gen = cache.generation();
    let can_skip_scan = !in_flight_freed
        && !state.header_scan_needs_retry
        && state.last_scanned_generation == Some(cache_gen);
    if can_skip_scan {
        return progress;
    }

    let mut scan_from = None;
    let mut needs_retry = false;
    while let Some(obj) = cache.next_after(scan_from) {
        let loc = obj.location();
        scan_from = Some(loc);
        if !state.range.contains(loc) { continue; }
        if state.in_flight.contains(&loc) || state.completed.contains(&loc) { continue; }
        // source reset mid-object, nothing to send unless it's restarted
        // (`ObjectCache::restart` bumps the generation, so a scan follows)
        if obj.state() == ObjectState::Canceled { continue; }
        let (subgroup_id, publisher_priority, status, payload_len, properties, received_at, end_of_group) =
            (obj.subgroup_id(), obj.publisher_priority(), obj.status(), obj.payload_len(), obj.properties(), obj.received_at(), obj.end_of_group());
        if obj.datagram() {
            // e.g. relayed from a fetch stream, sent once whole
            if obj.state() != ObjectState::Completed { needs_retry = true; continue; }
            let object = OutgoingDatagram { location: loc, publisher_priority, end_of_group, status, properties, received_at: Some(received_at.into()), payload: obj.readable_from(0) };
            match moq.session.send_datagram_obj(alias, object, &mut moq.t) {
                Ok(()) => {}
                // draft-ietf-moq-transport-18 §11.3: a datagram that doesn't fit is dropped
                Err(e @ (moq_core::Error::Done | moq_core::Error::ObjectExpired | moq_core::Error::ObjectFiltered)) => debug!("{label} datagram at {loc:?} dropped: {e:?}"),
                Err(moq_core::Error::TrackEnded) => break,
                // TODO the datagrams of drafts before 14, implement when needed
                Err(e) => error!("{label} datagram at {loc:?} not sent: {e:?}"),
            }
            state.completed.insert(loc);
            progress = true;
            continue;
        }
        // Same (group, subgroup)'s earlier object must finish payload first
        // (`OutStream` requires it) — skip, retry once it completes.
        let subgroup_busy = state.in_flight.iter()
            .any(|&l| l.group == loc.group && cache.find(l).is_some_and(|o| o.subgroup_id() == subgroup_id));
        if subgroup_busy { continue; }
        match moq.session.send_obj_hdr(alias, OutgoingObject { publisher_priority, status, end_of_group, properties: Some(properties), received_at: Some(received_at.into()), ..OutgoingObject::new(loc, subgroup_id, payload_len) }, &mut moq.t) {
            Ok(()) => {
                if payload_len == 0 {
                    state.complete(moq, alias, loc, subgroup_id, SubgroupEnd::of(&obj));
                } else {
                    state.in_flight.insert(loc);
                }
                progress = true;
            }
            // expired, or its subgroup already FIN'd downstream: can't be sent any more
            Err(moq_core::Error::ObjectExpired | moq_core::Error::ObjectFiltered | moq_core::Error::SubgroupFinished) => { state.completed.insert(loc); progress = true; }
            // Done: quiche itself can't write right now (e.g. mid path
            // migration) — same "try again later" as InsufficientCapacity,
            // just from a lower layer. Not a real error.
            Err(moq_core::Error::Done | moq_core::Error::InsufficientCapacity) => { debug!("{label} backpressure on stream open, retrying"); needs_retry = true; break; }
            // subgroup_busy should prevent this; if wrong, skip just this object.
            Err(moq_core::Error::UnfinishedPayload) => continue,
            // unsubscribed or ended, the caller drops this alias (`apply_unsubscribes`)
            Err(moq_core::Error::TrackEnded) => break,
            Err(e) => { error!("{label} send_obj_hdr: {e:?}"); needs_retry = true; break; }
        }
    }
    state.header_scan_needs_retry = needs_retry;
    state.last_scanned_generation = Some(cache_gen);

    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_object_cache::{fold, NewObject, PackedObjectCache};
    
    use moq_wire::MOQ_VERSION_DRAFT_16;
    use moq_core::test_utils::{next_subscribe, subscribe_response};
    use quiche_moq::test_utils::{init_moq_pipe, MoqPipe};
    use quiche_moq::{Config, VersionNegotiation};

    fn subscribed_track(p: &mut MoqPipe) -> TrackAlias {
        let mut client = p.client();
        let subscription = client.session.subscribe(&"n1--t1".parse().unwrap(), quiche_moq::SubscribeOptions::default(), &mut client.t).unwrap();
        p.advance().unwrap();
        let mut server = p.server();
        let request = next_subscribe(server.session);
        let publication = server.session.accept_request(request, quiche_moq::SubscribeOk::default(), &mut server.t).unwrap();
        let track_alias = publication.track_alias();
        p.advance().unwrap();
        subscribe_response(p.client().session, subscription.request_id()).unwrap().unwrap();
        p.subscriptions.push(subscription);
        p.publications.push(publication);
        track_alias
    }

    /// Regression test for the `cache.generation()` skip-scan optimization
    /// in `forward`: a newly `begin()`-ed object must still be discovered on
    /// the very next call, even though nothing else that could force a
    /// rescan (`in_flight_freed`, `header_scan_needs_retry`) changed. Only
    /// `cache_gen != state.last_scanned_generation` triggers it here — if
    /// that check is ever inverted or dropped, new objects would silently
    /// never get forwarded.
    #[test]
    fn forward_rescans_after_cache_generation_changes() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_moq_pipe(config);
        let track_alias = subscribed_track(&mut p);

        let mut cache = PackedObjectCache::new(4, 64, 64);
        let mut state = CacheToMoq::new();

        // Empty cache: nothing to send, but this establishes last_scanned_generation.
        let progress = forward(&cache, &mut state, &mut p.server(), track_alias, "test");
        assert!(!progress);
        assert_eq!(state.last_scanned_generation, Some(0));

        cache.begin(NewObject { end_of_subgroup: true, ..NewObject::new(Location { group: 0, object: 0 }, 1) }).unwrap();
        let progress = forward(&cache, &mut state, &mut p.server(), track_alias, "test");
        assert!(progress, "a newly begun object must not be silently skipped");
        assert_eq!(state.last_scanned_generation, Some(1));
    }

    /// A canceled object is skipped, not forgotten: once restarted by a
    /// resend it is forwarded.
    #[test]
    fn restarted_object_is_forwarded() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_moq_pipe(config);
        let track_alias = subscribed_track(&mut p);
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let mut state = CacheToMoq::new();
        let loc = Location { group: 0, object: 0 };

        moq_object_cache::begin_or_join(&mut cache, NewObject { end_of_subgroup: true, ..NewObject::new(loc, 1) }).unwrap();
        cache.find_mut(loc).unwrap().cancel();
        assert!(!forward(&cache, &mut state, &mut p.server(), track_alias, "test"));
        assert!(!state.completed.contains(&loc));

        moq_object_cache::begin_or_join(&mut cache, NewObject { end_of_subgroup: true, ..NewObject::new(loc, 1) }).unwrap();
        moq_object_cache::fold(&mut cache, loc, 1, b"x");
        assert!(forward(&cache, &mut state, &mut p.server(), track_alias, "test"));
        assert!(state.completed.contains(&loc) || state.in_flight.contains(&loc));
    }

    #[test]
    fn refilter_resets_only_in_flight_objects_outside_new_range() {
        let below = Location { group: 0, object: 0 };
        let start = Location { group: 1, object: 0 };
        let beyond = Location { group: 3, object: 0 };
        let mut state = CacheToMoq::new();
        state.in_flight.extend([below, start, beyond]);
        state.completed.insert(Location { group: 0, object: 1 });
        state.last_scanned_generation = Some(3);

        let range = FilterRange { start: Some(start), end_group: Some(2) };
        state.refilter(range);

        let mut reset = state.needs_reset.clone();
        reset.sort_by_key(|&(loc, _)| loc);
        assert_eq!(reset, vec![(below, ResetCode::Canceled), (beyond, ResetCode::Canceled)]);
        assert_eq!(state.range, range);
        assert_eq!(state.completed.len(), 1, "delivered objects stay delivered");
        assert_eq!(state.last_scanned_generation, None, "new range must trigger a rescan");
    }

    #[test]
    fn objects_after_end_group_are_not_forwarded() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_moq_pipe(config);
        let track_alias = subscribed_track(&mut p);
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let mut state = CacheToMoq::filtered(FilterRange { start: None, end_group: Some(0) });
        let inside = Location { group: 0, object: 0 };
        let beyond = Location { group: 1, object: 0 };
        for loc in [inside, beyond] {
            cache.begin(NewObject { end_of_subgroup: true, ..NewObject::new(loc, 1) }).unwrap();
        }

        forward(&cache, &mut state, &mut p.server(), track_alias, "test");
        assert!(state.in_flight.contains(&inside) || state.completed.contains(&inside));
        assert!(!state.in_flight.contains(&beyond) && !state.completed.contains(&beyond));
    }

    /// An `AbsoluteRange` is delivered once its end group ended and every object in it is forwarded.
    #[test]
    fn absolute_range_is_delivered_after_its_end_group() {
        let config = Config { version_negotiation: VersionNegotiation::Pinned(MOQ_VERSION_DRAFT_16), ..Config::default() };
        let mut p = init_moq_pipe(config);
        let track_alias = subscribed_track(&mut p);
        let mut cache = PackedObjectCache::new(4, 64, 64);
        let mut state = CacheToMoq::filtered(FilterRange { start: None, end_group: Some(0) });
        let first = Location { group: 0, object: 0 };
        cache.begin(NewObject::new(first, 1)).unwrap();
        fold(&mut cache, first, 1, b"x");
        forward(&cache, &mut state, &mut p.server(), track_alias, "test");
        assert!(!state.range_delivered(&cache), "group 0 may continue");

        let last = Location { group: 0, object: 1 };
        cache.begin(NewObject { end_of_subgroup: true, ..NewObject::new(last, 1) }).unwrap();
        assert!(!state.range_delivered(&cache), "its last object isn't forwarded");
        fold(&mut cache, last, 1, b"y");
        while forward(&cache, &mut state, &mut p.server(), track_alias, "test") {}
        assert!(state.awaiting_fin.is_empty(), "object 1 carried the FIN");
        assert!(state.range_delivered(&cache));
    }
}
