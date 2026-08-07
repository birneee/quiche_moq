use moq_wire::{KeyValuePairs, Location};
use std::collections::VecDeque;
use web_time::Instant;
use crate::{ObjectCache, object_ref::{CachedObjectMutRef, CachedObjectRef, ObjectMeta}};

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
}

impl SlotObjectCache {
    pub fn new(max_objects: usize, slot_size: usize) -> Self {
        Self {
            data: vec![0u8; max_objects * slot_size].into_boxed_slice(),
            meta: VecDeque::with_capacity(max_objects),
            head: 0,
            max_objects,
            slot_size,
        }
    }

    fn slot_data(&self, slot_index: usize) -> &[u8] {
        let start = slot_index * self.slot_size;
        &self.data[start..start + self.slot_size]
    }

}

impl ObjectCache for SlotObjectCache {
    fn begin(&mut self, location: Location, ext_hdrs: KeyValuePairs, payload_len: usize) -> Result<(), ()> {
        // Streams race independently and can be delivered out of order (e.g. after a network
        // stall/migration). Reject anything that doesn't advance past what's already cached
        // instead of caching a stale straggler after newer data has already gone out.
        if self.meta.back().is_some_and(|m| location <= m.inner.location) {
            return Err(());
        }
        assert!(
            payload_len <= self.slot_size,
            "object payload {payload_len} exceeds slot size {}",
            self.slot_size
        );
        if self.meta.len() == self.max_objects {
            self.meta.pop_front();
        }
        let slot_index = self.head;
        self.head = (self.head + 1) % self.max_objects;
        self.meta.push_back(SlotMeta {
            inner: ObjectMeta { location, ext_hdrs, payload_len, write_pos: 0, received_at: Instant::now() },
            slot_index,
        });
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(group: u64, object: u64) -> Location { Location { group, object } }
    fn kv() -> KeyValuePairs { KeyValuePairs::new() }

    #[test]
    fn slot_reuse_no_gap() {
        let mut cache = SlotObjectCache::new(2, 4);
        for i in 0..4u64 {
            cache.begin(loc(0, i), kv(), 4).unwrap();
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
