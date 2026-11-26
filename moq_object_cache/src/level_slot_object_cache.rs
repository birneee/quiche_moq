use std::collections::VecDeque;
use moq_wire::Location;
use crate::{BeginError, NewObject, ObjectCache, object_cache::Generation, object_ref::{CachedObjectMutRef, CachedObjectRef, ObjectMeta}};

pub struct Level {
    data: Box<[u8]>,
    num_slots: usize,
    slot_size: usize,
    /// Monotonically increasing write counter (next slot index to allocate).
    write: usize,
    /// Number of currently occupied slots.
    fill: usize,
}

impl Level {
    pub fn new(slot_size: usize, num_slots: usize) -> Self {
        assert!(slot_size > 0 && num_slots > 0);
        Self {
            data: vec![0u8; slot_size * num_slots].into_boxed_slice(),
            num_slots,
            slot_size,
            write: 0,
            fill: 0,
        }
    }

    fn is_full(&self) -> bool { self.fill == self.num_slots }

    fn alloc(&mut self) -> Option<usize> {
        if self.is_full() { return None; }
        let slot_index = self.write % self.num_slots;
        self.write += 1;
        self.fill += 1;
        Some(slot_index)
    }

    fn slot_data(&self, slot_index: usize) -> &[u8] {
        let start = slot_index * self.slot_size;
        &self.data[start..start + self.slot_size]
    }

    fn slot_data_mut(&mut self, slot_index: usize) -> &mut [u8] {
        let start = slot_index * self.slot_size;
        &mut self.data[start..start + self.slot_size]
    }

    fn reclaim(&mut self) {
        self.fill -= 1;
    }
}

struct Entry {
    meta: ObjectMeta,
    level: usize,
    slot_index: usize,
}

/// Multi-level fixed-slot cache. Objects are routed to the smallest level whose
/// `slot_size` fits the payload.
///
/// Levels must be provided sorted ascending by slot size.
pub struct LevelSlotObjectCache {
    levels: Vec<Level>,
    objects: VecDeque<Entry>,
    /// Bumped on every successful `begin()` or `restart()` — see `ObjectCache::generation`.
    generation: Generation,
}

impl LevelSlotObjectCache {
    pub fn new(levels: Vec<Level>) -> Self {
        assert!(!levels.is_empty(), "need at least one level");
        assert!(
            levels.windows(2).all(|w| w[0].slot_size < w[1].slot_size),
            "levels must be strictly ascending by slot size"
        );
        let total_capacity = levels.iter().map(|l| l.num_slots).sum();
        Self { levels, objects: VecDeque::with_capacity(total_capacity), generation: Generation::default() }
    }

    fn level_for_payload(&self, payload_len: usize) -> Option<usize> {
        self.levels.iter().position(|l| l.slot_size >= payload_len)
    }
}

impl ObjectCache for LevelSlotObjectCache {
    fn begin(&mut self, object: NewObject) -> Result<Vec<Location>, BeginError> {
        let (location, payload_len) = (object.location, object.payload_len);
        // Streams race independently and can be delivered out of order (e.g. after a network
        // stall/migration). Reject anything that doesn't advance past what's already cached
        // instead of caching a stale straggler after newer data has already gone out.
        if self.objects.back().is_some_and(|e| location <= e.meta.location) {
            return Err(BeginError::Stale);
        }
        let idx = self.level_for_payload(payload_len).ok_or(BeginError::TooLarge)?;

        // oldest first, also of other levels, until this level has a free slot
        let mut evicted = Vec::new();
        while self.levels[idx].is_full() {
            let oldest = self.objects.pop_front().expect("a full level holds objects");
            self.levels[oldest.level].reclaim();
            evicted.push(oldest.meta.location);
        }

        let slot_index = self.levels[idx].alloc().expect("evicted until the level has room");
        self.objects.push_back(Entry {
            meta: ObjectMeta::new(object),
            level: idx,
            slot_index,
        });
        self.generation.bump();
        Ok(evicted)
    }

    fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    fn latest(&self) -> Option<CachedObjectRef<'_>> {
        self.objects.back().map(|e| CachedObjectRef {
            meta: &e.meta,
            data: self.levels[e.level].slot_data(e.slot_index),
        })
    }

    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>> {
        let e = self.objects.back_mut()?;
        let data = self.levels[e.level].slot_data_mut(e.slot_index);
        Some(CachedObjectMutRef { meta: &mut e.meta, data })
    }

    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>> {
        self.objects.iter().find(|e| e.meta.location == loc)
            .map(|e| CachedObjectRef {
                meta: &e.meta,
                data: self.levels[e.level].slot_data(e.slot_index),
            })
    }

    fn find_mut(&mut self, loc: Location) -> Option<CachedObjectMutRef<'_>> {
        let e = self.objects.iter_mut().find(|e| e.meta.location == loc)?;
        let data = self.levels[e.level].slot_data_mut(e.slot_index);
        Some(CachedObjectMutRef { meta: &mut e.meta, data })
    }

    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>> {
        let e = match loc {
            None    => self.objects.front(),
            Some(l) => self.objects.iter().find(|e| e.meta.location > l),
        }?;
        Some(CachedObjectRef {
            meta: &e.meta,
            data: self.levels[e.level].slot_data(e.slot_index),
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

    fn loc(group: u64, object: u64) -> Location { Location { group, object } }

    fn write(cache: &mut dyn ObjectCache, loc: Location, payload_len: usize) {
        cache.begin(NewObject::new(loc, payload_len)).unwrap();
        if let Some(mut obj) = cache.latest_mut() {
            let buf = obj.unfilled();
            buf.fill(0xAB);
            let n = buf.len();
            obj.advance_write_pos(n);
        }
    }

    #[test]
    fn routes_by_size() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 4), Level::new(64, 4)]);
        cache.begin(NewObject::new(loc(0, 0), 4)).unwrap();
        cache.begin(NewObject::new(loc(0, 1), 32)).unwrap();
        assert!(cache.find(loc(0, 0)).is_some());
        assert!(cache.find(loc(0, 1)).is_some());
    }

    #[test]
    fn rejects_oversized_object() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 4), Level::new(64, 4)]);
        assert!(cache.begin(NewObject::new(loc(0, 0), 65)).is_err());
    }

    #[test]
    fn strict_fifo_eviction_across_levels() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 2), Level::new(64, 2)]);
        write(&mut cache, loc(0, 0), 4);
        write(&mut cache, loc(0, 1), 4);
        write(&mut cache, loc(0, 2), 32);
        write(&mut cache, loc(0, 3), 32);
        write(&mut cache, loc(0, 4), 4); // evicts loc(0,0)
        assert!(cache.find(loc(0, 0)).is_none());
        assert!(cache.find(loc(0, 1)).is_some());
        assert!(cache.find(loc(0, 2)).is_some());
        assert!(cache.find(loc(0, 3)).is_some());
        assert!(cache.find(loc(0, 4)).is_some());
    }

    /// Large level full but the front object is small: older objects are
    /// evicted until a large slot frees, instead of the level jamming.
    #[test]
    fn evicts_older_levels_until_slot_frees() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 2), Level::new(32, 1)]);
        write(&mut cache, loc(0, 0), 4);  // small (front)
        write(&mut cache, loc(0, 1), 4);
        write(&mut cache, loc(0, 2), 32); // large level full
        assert_eq!(cache.begin(NewObject::new(loc(0, 3), 32)), Ok(vec![loc(0, 0), loc(0, 1), loc(0, 2)]));
        assert!(cache.find(loc(0, 3)).is_some());
    }

    #[test]
    fn evicts_same_level_when_front_matches() {
        // Large level full, front object is also large → evict it, insert succeeds.
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 2), Level::new(32, 1)]);
        write(&mut cache, loc(0, 0), 32); // large (front)
        write(&mut cache, loc(0, 1), 4);
        write(&mut cache, loc(0, 2), 4);
        write(&mut cache, loc(0, 3), 32); // large level full, front is large → evict loc(0,0)
        assert!(cache.find(loc(0, 0)).is_none());
        assert!(cache.find(loc(0, 3)).is_some());
    }

    #[test]
    fn err_when_oversized_for_all_levels() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 2), Level::new(32, 1)]);
        assert!(cache.begin(NewObject::new(loc(0, 0), 33)).is_err());
    }

    #[test]
    fn wrap_around_within_level() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(4, 3)]);
        write(&mut cache, loc(0, 0), 4);
        write(&mut cache, loc(0, 1), 4);
        write(&mut cache, loc(0, 2), 4); // level full
        write(&mut cache, loc(0, 3), 4); // evicts 0, wraps write cursor
        assert!(cache.find(loc(0, 0)).is_none());
        assert_eq!(cache.find(loc(0, 3)).unwrap().readable_from(0), &[0xAB; 4]);
    }

    #[test]
    fn next_after_in_location_order() {
        let mut cache = LevelSlotObjectCache::new(vec![Level::new(8, 2), Level::new(64, 2)]);
        write(&mut cache, loc(0, 0), 32);
        write(&mut cache, loc(0, 1), 4);
        write(&mut cache, loc(0, 2), 32);
        write(&mut cache, loc(0, 3), 4);

        let mut cur = None;
        for expected in [loc(0,0), loc(0,1), loc(0,2), loc(0,3)] {
            let obj = cache.next_after(cur).unwrap();
            assert_eq!(obj.location(), expected);
            cur = Some(obj.location());
        }
        assert!(cache.next_after(cur).is_none());
    }
}
