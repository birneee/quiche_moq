use moq_wire::Location;
use std::collections::VecDeque;
use crate::{BeginError, NewObject, ObjectCache, object_cache::Generation, object_ref::{CachedObjectMutRef, CachedObjectRef, ObjectMeta}};

struct PackedMeta {
    inner: ObjectMeta,
    start: usize,
}

/// Cache of MoQ objects backed by a single flat buffer of `max_objects * average_object_size` bytes.
/// Objects are packed contiguously by actual payload size (no fixed-slot padding).
/// Eviction is oldest first, by object count and by the space a new object needs.
pub struct PackedObjectCache {
    data: Box<[u8]>,
    meta: VecDeque<PackedMeta>,
    write_cursor: usize,
    max_objects: usize,
    max_object_size: usize,
    /// Number of adjacent `meta` entries in descending location order.
    /// - zero enables binary search instead of linear scan (was ~20% CPU in `next_after`)
    /// - drops back to zero once out-of-order entries are evicted
    descents: usize,
    /// Bumped on every successful `begin()` or `restart()` — see `ObjectCache::generation`.
    generation: Generation,
}

impl PackedObjectCache {
    fn sorted(&self) -> bool {
        self.descents == 0
    }

    fn pop_front(&mut self) -> Option<Location> {
        if self.meta.len() >= 2 && self.meta[0].inner.location > self.meta[1].inner.location {
            self.descents -= 1;
        }
        self.meta.pop_front().map(|m| m.inner.location)
    }

    pub fn new(max_objects: usize, max_object_size: usize, average_object_size: usize) -> Self {
        Self {
            data: vec![0u8; max_objects * average_object_size].into_boxed_slice(),
            meta: VecDeque::with_capacity(max_objects),
            write_cursor: 0,
            max_objects,
            max_object_size,
            descents: 0,
            generation: Generation::default(),
        }
    }
}

impl PackedObjectCache {
    /// Index of `loc` in `meta`: binary search while the cache is in location
    /// order, linear scan otherwise.
    fn index_of(&self, loc: Location) -> Option<usize> {
        if self.sorted() {
            let i = self.meta.partition_point(|m| m.inner.location < loc);
            self.meta.get(i).filter(|m| m.inner.location == loc).map(|_| i)
        } else {
            self.meta.iter().position(|m| m.inner.location == loc)
        }
    }

    fn contains(&self, loc: Location) -> bool {
        self.index_of(loc).is_some()
    }
}

impl ObjectCache for PackedObjectCache {
    fn begin(&mut self, object: NewObject) -> Result<Vec<Location>, BeginError> {
        let (location, payload_len) = (object.location, object.payload_len);
        // subgroups race, so any location order; a duplicate would alias one object
        if self.contains(location) {
            return Err(BeginError::Stale);
        }
        if payload_len > self.max_object_size || payload_len > self.data.len() {
            return Err(BeginError::TooLarge);
        }
        let start = if self.write_cursor + payload_len > self.data.len() { 0 } else { self.write_cursor };
        let end = start + payload_len;
        // oldest first, until there is room and nothing overlaps
        let past_overlap = self.meta.iter().rposition(|m| m.start < end && m.start + m.inner.payload_len > start).map_or(0, |i| i + 1);
        let over_count = (self.meta.len() + 1).saturating_sub(self.max_objects);
        let evicted = (0..past_overlap.max(over_count)).filter_map(|_| self.pop_front()).collect();
        self.write_cursor = start;
        if self.meta.back().is_some_and(|m| m.inner.location > location) {
            self.descents += 1;
        }
        self.meta.push_back(PackedMeta {
            inner: ObjectMeta::new(object),
            start,
        });
        self.write_cursor += payload_len;
        self.generation.bump();
        Ok(evicted)
    }

    fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    // highest location, not the last inserted
    fn latest(&self) -> Option<CachedObjectRef<'_>> {
        let latest = if self.sorted() { self.meta.back() } else { self.meta.iter().max_by_key(|m| m.inner.location) };
        latest.map(|m| CachedObjectRef {
            meta: &m.inner,
            data: &self.data[m.start..m.start + m.inner.payload_len],
        })
    }

    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>> {
        let m = if self.sorted() { self.meta.back_mut()? } else { self.meta.iter_mut().max_by_key(|m| m.inner.location)? };
        let end = m.start + m.inner.payload_len;
        let data = &mut self.data[m.start..end];
        Some(CachedObjectMutRef { meta: &mut m.inner, data })
    }

    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>> {
        let idx = self.index_of(loc)?;
        let m = &self.meta[idx];
        Some(CachedObjectRef {
            meta: &m.inner,
            data: &self.data[m.start..m.start + m.inner.payload_len],
        })
    }

    fn find_mut(&mut self, loc: Location) -> Option<CachedObjectMutRef<'_>> {
        let idx = self.index_of(loc)?;
        let m = &mut self.meta[idx];
        let (start, payload_len) = (m.start, m.inner.payload_len);
        Some(CachedObjectMutRef { meta: &mut m.inner, data: &mut self.data[start..start + payload_len] })
    }

    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>> {
        // meta is in arrival order: binary search while that is location order, else the minimum past the bound
        let m = if self.sorted() {
            // Ascending order: the answer is the first entry past the bound.
            match loc {
                None => self.meta.front(),
                Some(l) => {
                    let i = self.meta.partition_point(|m| m.inner.location <= l);
                    self.meta.get(i)
                }
            }
        } else {
            match loc {
                None => self.meta.iter().min_by_key(|m| m.inner.location),
                Some(l) => self.meta.iter().filter(|m| m.inner.location > l).min_by_key(|m| m.inner.location),
            }
        }?;
        Some(CachedObjectRef {
            meta: &m.inner,
            data: &self.data[m.start..m.start + m.inner.payload_len],
        })
    }

    fn generation(&self) -> u64 {
        self.generation.get()
    }

    fn restart(&mut self, loc: Location) {
        let Some(mut obj) = self.find_mut(loc) else { return };
        obj.restart();
        self.generation.bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(group: u64, object: u64) -> Location {
        Location { group, object }
    }

    #[test]
    fn rejects_oversized_object() {
        let mut cache = PackedObjectCache::new(3, 10, 10);
        assert!(cache.begin(NewObject::new(loc(0, 0), 11)).is_err());
        assert!(cache.is_empty());
    }

    /// An object larger than average evicts as many old ones as it needs,
    /// instead of failing and jamming the cache.
    #[test]
    fn large_object_evicts_until_it_fits() {
        let mut cache = PackedObjectCache::new(2, 6, 4);
        cache.begin(NewObject::new(loc(0, 0), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 1), 4)).unwrap();
        // wraps to offset 0, overlapping both
        assert_eq!(cache.begin(NewObject::new(loc(0, 2), 6)), Ok(vec![loc(0, 0), loc(0, 1)]));
        assert!(cache.find(loc(0, 2)).is_some());
        assert_eq!(cache.begin(NewObject::new(loc(0, 3), 2)), Ok(vec![]), "fits after it");
    }

    #[test]
    fn packing_small_objects() {
        let mut cache = PackedObjectCache::new(3, 10, 10);
        for i in 0..3u64 {
            cache.begin(NewObject::new(loc(0, i), 2)).unwrap();
            {
                let mut o = cache.latest_mut().unwrap();
                let s = &[i as u8, i as u8];
                o.unfilled()[..s.len()].copy_from_slice(s);
                o.advance_write_pos(s.len());
            };
        }
        assert_eq!(cache.write_cursor, 6); // packed at 2 bytes each, not 10
    }

    #[test]
    fn wrap_around() {
        let mut cache = PackedObjectCache::new(2, 4, 4);
        cache.begin(NewObject::new(loc(0, 0), 4)).unwrap();
        {
            let mut o = cache.latest_mut().unwrap();
            let s = &[0; 4];
            o.unfilled()[..s.len()].copy_from_slice(s);
            o.advance_write_pos(s.len());
        };
        cache.begin(NewObject::new(loc(0, 1), 4)).unwrap();
        {
            let mut o = cache.latest_mut().unwrap();
            let s = &[1; 4];
            o.unfilled()[..s.len()].copy_from_slice(s);
            o.advance_write_pos(s.len());
        };
        cache.begin(NewObject::new(loc(0, 2), 4)).unwrap();
        {
            let mut o = cache.latest_mut().unwrap();
            let s = &[2; 4];
            o.unfilled()[..s.len()].copy_from_slice(s);
            o.advance_write_pos(s.len());
        }; // wraps, evicts obj0
        assert_eq!(cache.write_cursor, 4);
        assert!(cache.find(loc(0, 0)).is_none());
        assert_eq!(
            cache.find(loc(0, 2)).unwrap().readable_from(0),
            &[2, 2, 2, 2]
        );
    }

    #[test]
    fn object_larger_than_buffer_is_rejected() {
        let mut cache = PackedObjectCache::new(2, 10, 4);
        assert!(cache.begin(NewObject::new(loc(0, 0), 9)).is_err());
        assert!(cache.is_empty());
    }

    /// Independent subgroups can be *discovered* out of location order (e.g.
    /// several streams becoming readable in the same tick, in arbitrary
    /// order, after a stall clears) — `begin()` must accept a lower location
    /// arriving after a higher one instead of rejecting it as stale.
    #[test]
    fn begin_accepts_out_of_order_locations() {
        let mut cache = PackedObjectCache::new(4, 8, 8);
        cache.begin(NewObject::new(loc(0, 5), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 3), 4)).unwrap(); // arrives after 5, but is lower
        assert!(cache.find(loc(0, 3)).is_some());
        assert!(cache.find(loc(0, 5)).is_some());
    }

    #[test]
    fn begin_rejects_exact_duplicate() {
        let mut cache = PackedObjectCache::new(4, 8, 8);
        cache.begin(NewObject::new(loc(0, 5), 4)).unwrap();
        assert!(cache.begin(NewObject::new(loc(0, 5), 4)).is_err());
    }

    /// `next_after`/`latest` must reflect location order, not insertion
    /// (arrival) order, once entries can be inserted out of order.
    #[test]
    fn next_after_and_latest_use_location_order_not_arrival_order() {
        let mut cache = PackedObjectCache::new(4, 8, 8);
        cache.begin(NewObject::new(loc(0, 5), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 3), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 7), 4)).unwrap();

        assert_eq!(cache.next_after(None).unwrap().location(), loc(0, 3));
        assert_eq!(cache.next_after(Some(loc(0, 3))).unwrap().location(), loc(0, 5));
        assert_eq!(cache.next_after(Some(loc(0, 5))).unwrap().location(), loc(0, 7));
        assert!(cache.next_after(Some(loc(0, 7))).is_none());
        assert_eq!(cache.latest().unwrap().location(), loc(0, 7));
    }

    /// A publisher's own cache (or a relay whose subgroups never actually
    /// race) only ever inserts ascending locations — `sorted()` must stay
    /// `true` so `next_after`/`latest` take the binary-search path, and that
    /// path must still return the right answers.
    #[test]
    fn ascending_inserts_stay_sorted_and_binary_search_is_correct() {
        let mut cache = PackedObjectCache::new(4, 8, 8);
        cache.begin(NewObject::new(loc(0, 3), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 5), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 7), 4)).unwrap();
        assert!(cache.sorted(), "strictly ascending inserts must not clear the sorted flag");

        assert_eq!(cache.next_after(None).unwrap().location(), loc(0, 3));
        assert_eq!(cache.next_after(Some(loc(0, 3))).unwrap().location(), loc(0, 5));
        assert_eq!(cache.next_after(Some(loc(0, 5))).unwrap().location(), loc(0, 7));
        assert!(cache.next_after(Some(loc(0, 7))).is_none());
        assert_eq!(cache.latest().unwrap().location(), loc(0, 7));
        assert_eq!(cache.latest_mut().unwrap().as_ref().location(), loc(0, 7));
    }

    /// An out-of-order insert disables the binary search until it is evicted.
    #[test]
    fn out_of_order_insert_unsorted_until_evicted() {
        let mut cache = PackedObjectCache::new(3, 8, 8);
        cache.begin(NewObject::new(loc(0, 3), 4)).unwrap();
        assert!(cache.sorted());
        cache.begin(NewObject::new(loc(0, 1), 4)).unwrap(); // lower than the last insert
        assert!(!cache.sorted());
        cache.begin(NewObject::new(loc(0, 9), 4)).unwrap();
        assert!(!cache.sorted());
        assert_eq!(cache.next_after(Some(loc(0, 1))).unwrap().location(), loc(0, 3));
        cache.begin(NewObject::new(loc(0, 10), 4)).unwrap(); // evicts loc(0, 3), loc(0, 1) is still first
        assert!(cache.sorted());
    }

    /// Eviction only ever pops the front, so it must not itself disturb an
    /// otherwise-still-ascending run.
    #[test]
    fn eviction_of_oldest_preserves_sorted() {
        let mut cache = PackedObjectCache::new(2, 8, 8);
        cache.begin(NewObject::new(loc(0, 1), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 2), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 3), 4)).unwrap(); // evicts loc(0, 1)
        assert!(cache.sorted());
        assert!(cache.find(loc(0, 1)).is_none());
        assert_eq!(cache.next_after(None).unwrap().location(), loc(0, 2));
        assert_eq!(cache.latest().unwrap().location(), loc(0, 3));
    }
}
