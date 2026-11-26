use moq_wire::Location;
use std::collections::VecDeque;
use crate::{BeginError, NewObject, ObjectCache, object_cache::Generation, object_ref::{CachedObjectMutRef, CachedObjectRef, ObjectMeta}};

struct SlotMeta {
    inner: ObjectMeta,
    slot_index: usize,
}

/// Cache of MoQ objects backed by `max_objects` fixed-size slots of `slot_size` bytes each.
/// Slots are reused in strict FIFO order.
/// Eviction is by object count (oldest first).
/// Objects larger than `slot_size` are rejected with an assertion.
pub struct SlotObjectCache {
    data: Box<[u8]>,
    meta: VecDeque<SlotMeta>,
    head: usize,
    max_objects: usize,
    slot_size: usize,
    /// Bumped on every successful `begin()` or `restart()` — see `ObjectCache::generation`.
    generation: Generation,
}

impl SlotObjectCache {
    pub fn new(max_objects: usize, slot_size: usize) -> Self {
        Self {
            data: vec![0u8; max_objects * slot_size].into_boxed_slice(),
            meta: VecDeque::with_capacity(max_objects),
            head: 0,
            max_objects,
            slot_size,
            generation: Generation::default(),
        }
    }

    fn slot_data(&self, slot_index: usize) -> &[u8] {
        let start = slot_index * self.slot_size;
        &self.data[start..start + self.slot_size]
    }

}

impl ObjectCache for SlotObjectCache {
    fn begin(&mut self, object: NewObject) -> Result<Vec<Location>, BeginError> {
        let (location, payload_len) = (object.location, object.payload_len);
        // Streams race independently and can be delivered out of order (e.g. after a network
        // stall/migration). Reject anything that doesn't advance past what's already cached
        // instead of caching a stale straggler after newer data has already gone out.
        if self.meta.back().is_some_and(|m| location <= m.inner.location) {
            return Err(BeginError::Stale);
        }
        if payload_len > self.slot_size {
            return Err(BeginError::TooLarge);
        }
        let evicted = if self.meta.len() == self.max_objects {
            self.meta.pop_front().map(|m| m.inner.location)
        } else {
            None
        };
        let slot_index = self.head;
        self.head = (self.head + 1) % self.max_objects;
        self.meta.push_back(SlotMeta {
            inner: ObjectMeta::new(object),
            slot_index,
        });
        self.generation.bump();
        Ok(Vec::from_iter(evicted))
    }

    fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    fn latest(&self) -> Option<CachedObjectRef<'_>> {
        self.meta.back().map(|m| CachedObjectRef {
            meta: &m.inner,
            data: self.slot_data(m.slot_index),
        })
    }

    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>> {
        let m = self.meta.back_mut()?;
        let start = m.slot_index * self.slot_size;
        let data = &mut self.data[start..start + self.slot_size];
        Some(CachedObjectMutRef { meta: &mut m.inner, data })
    }

    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>> {
        self.meta
            .iter()
            .find(|m| m.inner.location == loc)
            .map(|m| CachedObjectRef {
                meta: &m.inner,
                data: self.slot_data(m.slot_index),
            })
    }

    fn find_mut(&mut self, loc: Location) -> Option<CachedObjectMutRef<'_>> {
        let m = self.meta.iter_mut().find(|m| m.inner.location == loc)?;
        let start = m.slot_index * self.slot_size;
        let data = &mut self.data[start..start + self.slot_size];
        Some(CachedObjectMutRef { meta: &mut m.inner, data })
    }

    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>> {
        let m = match loc {
            None => self.meta.front(),
            Some(l) => self.meta.iter().find(|m| m.inner.location > l),
        }?;
        Some(CachedObjectRef {
            meta: &m.inner,
            data: self.slot_data(m.slot_index),
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

    #[test]
    fn slot_reuse_no_gap() {
        let mut cache = SlotObjectCache::new(2, 4);
        for i in 0..4u64 {
            cache.begin(NewObject::new(loc(0, i), 4)).unwrap();
            let mut o = cache.latest_mut().unwrap();
            o.unfilled().copy_from_slice(&[i as u8; 4]);
            o.advance_write_pos(4);
        }
        assert!(cache.find(loc(0, 0)).is_none());
        assert!(cache.find(loc(0, 1)).is_none());
        assert_eq!(cache.find(loc(0, 2)).unwrap().readable_from(0), &[2; 4]);
        assert_eq!(cache.find(loc(0, 3)).unwrap().readable_from(0), &[3; 4]);
    }
}
