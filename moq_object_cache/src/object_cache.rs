use std::ops::ControlFlow;

use moq_wire::{KeyValuePairs, Location};
use moq_wire::object::ObjectStatus;

use crate::{CachedObjectMutRef, CachedObjectRef, ObjectState};

/// An Object to `begin` in a cache, its payload written after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewObject {
    pub location: Location,
    pub subgroup_id: u64,
    pub publisher_priority: u8,
    /// Known to be the last Object of its Subgroup, else `ObjectCache::mark_end_of_subgroup` later.
    pub end_of_subgroup: bool,
    pub properties: KeyValuePairs,
    pub payload_len: usize,
}

impl NewObject {
    /// In Subgroup 0, with publisher priority 0 and no properties.
    pub fn new(location: Location, payload_len: usize) -> Self {
        Self { location, subgroup_id: 0, publisher_priority: 0, end_of_subgroup: false, properties: KeyValuePairs::new(), payload_len }
    }
}

pub trait ObjectCache {
    /// Begin a new object. Evicts the oldest objects until it fits, returning
    /// their locations so callers can tear down anything still keyed to them
    /// (e.g. an in-flight send stream).
    /// Returns `Err` without changing the cache if the object is a duplicate, is
    /// larger than the maximum object size, or larger than the whole buffer.
    /// - ordering: `PackedObjectCache` accepts any location, `SlotObjectCache`
    ///   and `LevelSlotObjectCache` reject one not after the latest
    fn begin(&mut self, object: NewObject) -> Result<Vec<Location>, BeginError>;
    fn is_empty(&self) -> bool;
    fn latest(&self) -> Option<CachedObjectRef<'_>>;
    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>>;
    /// Find the cached object at exactly `loc`, if still present.
    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>>;
    /// Mutable version of `find`, for writing into an object that isn't
    /// necessarily the most recently `begin`'d one — needed when multiple
    /// objects (from independent subgroups/streams) are being filled
    /// concurrently, since only one of them can be `latest()`.
    fn find_mut(&mut self, loc: Location) -> Option<CachedObjectMutRef<'_>>;
    /// Oldest object with location strictly after `loc`, or the oldest object if `loc` is `None`.
    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>>;

    /// Objects after `loc` (all if `None`) in location order, see `next_after`.
    fn objects_after(&self, loc: Option<Location>) -> impl Iterator<Item = CachedObjectRef<'_>> where Self: Sized {
        std::iter::successors(self.next_after(loc), |obj| self.next_after(Some(obj.location())))
    }

    /// Bumped by every successful `begin()` or `restart()` — a cheap "did a
    /// new object appear" signal for callers (e.g. `cache_to_moq::forward`) that would
    /// otherwise have to rescan the whole cache on every tick just to find
    /// out nothing changed. Never bumped by anything else: `find_mut`/`fold`
    /// writes, `mark_end_of_subgroup`, and eviction only touch objects a
    /// scanner has already seen (or that have vanished, which a scan
    /// tolerates for free), not new ones a scanner would need telling about.
    fn generation(&self) -> u64;
    /// Receives the canceled `loc` again from the start, e.g. resent on a new
    /// stream after a reset (`CachedObjectMutRef::restart`). Bumps
    /// `generation`, so forwarders pick it up again.
    fn restart(&mut self, loc: Location);

    /// Confirms `loc` is the last Object of its Subgroup — see
    /// `CachedObjectRef::end_of_subgroup`. A no-op if `loc` isn't cached.
    fn mark_end_of_subgroup(&mut self, loc: Location) {
        if let Some(mut obj) = self.find_mut(loc) {
            obj.set_end_of_subgroup();
        }
    }
    /// Its Subgroup holds the Group's largest Object — see
    /// `CachedObjectRef::end_of_group`. A no-op if `loc` isn't cached.
    fn mark_end_of_group(&mut self, loc: Location) {
        if let Some(mut obj) = self.find_mut(loc) {
            obj.set_end_of_group();
        }
    }
    /// The Subgroup of `loc`, its last received Object, was reset upstream.
    fn mark_subgroup_reset(&mut self, loc: Location) {
        if let Some(mut obj) = self.find_mut(loc) {
            obj.set_subgroup_reset();
        }
    }
}

/// Backing counter for `ObjectCache::generation` — one per cache impl,
/// bumped on every successful `begin()`.
#[derive(Default, Clone, Copy)]
pub struct Generation(u64);

impl Generation {
    pub fn bump(&mut self) {
        self.0 += 1;
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

/// Scans `cache` in location order, calling `on_incomplete` for every object
/// still `Progressing` (to read more payload into it) and `on_complete` for
/// every `Completed` or `Canceled` one — the "drain every cached object"
/// loop every `ObjectCache` reader (e.g. `BufferedTrack`)
/// otherwise hand-rolls identically. `on_complete` gets a `Canceled` object
/// same as a `Completed` one; check `CachedObjectRef::state()` inside it if
/// canceled ones need different handling. Stops early if either closure
/// returns `ControlFlow::Break`; otherwise scans the whole cache and returns
/// `None`.
pub fn scan_objects<B>(
    cache: &mut impl ObjectCache,
    mut on_incomplete: impl FnMut(Location, &mut CachedObjectMutRef) -> ControlFlow<B>,
    mut on_complete: impl FnMut(CachedObjectRef) -> ControlFlow<B>,
) -> Option<B> {
    let mut scan_from = None;
    loop {
        let obj = cache.next_after(scan_from)?;
        let loc = obj.location();
        scan_from = Some(loc);
        if obj.state() != ObjectState::Progressing {
            if let ControlFlow::Break(b) = on_complete(obj) {
                return Some(b);
            }
        } else {
            let mut obj = cache.find_mut(loc).unwrap();
            if let ControlFlow::Break(b) = on_incomplete(loc, &mut obj) {
                return Some(b);
            }
            // `on_incomplete` may have just filled this object's last
            // remaining bytes (or canceled it) — handle it as complete in
            // this same pass (via the same borrow, no extra cache lookup)
            // rather than waiting for a future scan: nothing guarantees
            // another wake fires once a stream's available bytes have all
            // been read.
            if obj.state() != ObjectState::Progressing
                && let ControlFlow::Break(b) = on_complete(obj.as_ref())
            {
                return Some(b);
            }
        }
    }
}

/// Count of `Completed` objects cached strictly after `loc` — a "how far
/// behind is the sink" backlog signal for a reader pacing its output against
/// arrival. Stops at the first `Progressing` object (nothing past it is
/// known yet); `Canceled` objects are skipped over but not counted, since
/// they carry no payload for a reader to catch up on.
pub fn count_after(cache: &impl ObjectCache, loc: Option<Location>) -> usize {
    let mut count = 0;
    for obj in cache.objects_after(loc) {
        match obj.state() {
            ObjectState::Progressing => break,
            ObjectState::Completed => count += 1,
            ObjectState::Canceled => {}
        }
    }
    count
}

/// Why `ObjectCache::begin` refused an object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginError {
    /// Already cached, or not after what is cached for caches needing order.
    Stale,
    /// Larger than the cache can hold.
    TooLarge,
}

/// Error from `begin_or_join`.
#[derive(Debug, PartialEq)]
pub enum JoinError {
    /// `ObjectCache::begin` rejected the object: too large, no room, or out of
    /// order for that cache type.
    CacheFull,
    /// Another source began this Location with different
    /// payload_len/subgroup_id/properties — real disagreement, not the
    /// identical-content multi-source fill `fold` assumes.
    Mismatch,
}

/// Caches an Object received whole in a datagram (draft-ietf-moq-transport-18 §11.3), or joins
/// the copy cached already, see `begin_or_join`. Its Subgroup ID is 0, it belongs to none.
pub fn put_datagram(
    cache: &mut impl ObjectCache,
    loc: Location,
    publisher_priority: u8,
    end_of_group: bool,
    status: ObjectStatus,
    properties: KeyValuePairs,
    payload: &[u8],
) -> Result<Vec<Location>, JoinError> {
    let object = NewObject { publisher_priority, end_of_subgroup: true, properties, ..NewObject::new(loc, payload.len()) };
    let evicted = begin_or_join(cache, object)?;
    if let Some(mut object) = cache.find_mut(loc) {
        object.set_datagram();
        object.set_status(status);
        if end_of_group {
            object.set_end_of_group();
        }
    }
    fold(cache, loc, payload.len(), payload);
    Ok(evicted)
}

/// Starts `loc` in `cache`, or — if another source got there first —
/// verifies agreement and joins (`Ok(None)`) instead of erroring as a
/// duplicate. With `fold`, lets several sources concurrently fill the same
/// `Location` (e.g. multiple upstream publishers) as long as content matches.
/// A canceled object is restarted (`CachedObjectMutRef::restart`).
pub fn begin_or_join(cache: &mut impl ObjectCache, object: NewObject) -> Result<Vec<Location>, JoinError> {
    if let Some(existing) = cache.find(object.location) {
        if existing.payload_len() != object.payload_len || existing.subgroup_id() != object.subgroup_id || existing.properties() != &object.properties {
            return Err(JoinError::Mismatch);
        }
        if existing.state() == ObjectState::Canceled {
            cache.restart(object.location);
        }
        return Ok(Vec::new());
    }
    cache.begin(object).map_err(|_| JoinError::CacheFull)
}

/// Folds `chunk` (one source's next bytes for `loc`, ending at cumulative
/// position `received`) into `cache`: bytes already covered by the current
/// write position are discarded (a faster source already wrote them); only
/// the new tail is written. Assumes byte-identical content across sources —
/// never reconciles conflicts, only dedupes. No-op if `loc` was evicted.
pub fn fold(cache: &mut impl ObjectCache, loc: Location, received: usize, chunk: &[u8]) {
    let chunk_start = received - chunk.len();
    let Some(mut obj) = cache.find_mut(loc) else { return };
    let write_pos = obj.write_pos();
    if received <= write_pos {
        return; // fully covered by an earlier/faster source already
    }
    if chunk_start > write_pos {
        return; // gap before this chunk, e.g. `loc` was evicted and begun again
    }
    let skip = write_pos - chunk_start;
    let new_bytes = &chunk[skip..];
    let buf = obj.unfilled();
    let n = new_bytes.len().min(buf.len());
    buf[..n].copy_from_slice(&new_bytes[..n]);
    obj.advance_write_pos(n);
}
