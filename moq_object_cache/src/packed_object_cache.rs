use moq_wire::{KeyValuePairs, Location};
use std::collections::VecDeque;
use web_time::Instant;
use crate::{ObjectCache, object_ref::{CachedObjectMutRef, CachedObjectRef, ObjectMeta}};

struct PackedMeta {
    inner: ObjectMeta,
    start: usize,
}

/// Cache of MoQ objects backed by a single flat buffer of `max_objects * average_object_size` bytes.
/// Objects are packed contiguously by actual payload size (no fixed-slot padding).
/// Eviction is by object count (oldest first).
pub struct PackedObjectCache {
    data: Box<[u8]>,
    meta: VecDeque<PackedMeta>,
    write_cursor: usize,
    max_objects: usize,
    max_object_size: usize,
}

impl PackedObjectCache {
    pub fn new(max_objects: usize, max_object_size: usize, average_object_size: usize) -> Self {
        Self {
            data: vec![0u8; max_objects * average_object_size].into_boxed_slice(),
            meta: VecDeque::with_capacity(max_objects),
            write_cursor: 0,
            max_objects,
            max_object_size,
        }
    }
}

impl ObjectCache for PackedObjectCache {
    fn begin(&mut self, location: Location, ext_hdrs: KeyValuePairs, payload_len: usize) -> Result<(), ()> {
        // Streams race independently and can be delivered out of order (e.g. after a network
        // stall/migration). Reject anything that doesn't advance past what's already cached
        // instead of caching a stale straggler after newer data has already gone out.
        if self.meta.back().is_some_and(|m| location <= m.inner.location) {
            return Err(());
        }
        assert!(
            payload_len <= self.max_object_size,
            "object payload {payload_len} exceeds max object size {}",
            self.max_object_size
        );
        if self.meta.len() == self.max_objects {
            self.meta.pop_front();
        }
        if self.write_cursor + payload_len > self.data.len() {
            self.write_cursor = 0;
        }
        let end = self.write_cursor + payload_len;
        if end > self.data.len() {
            return Err(());
        }
        if self
            .meta
            .front()
            .is_some_and(|m| m.start < end && m.start + m.inner.payload_len > self.write_cursor)
        {
            return Err(());
        }
        let start = self.write_cursor;
        self.meta.push_back(PackedMeta {
            inner: ObjectMeta { location, ext_hdrs, payload_len, write_pos: 0, received_at: Instant::now() },
            start,
        });
        self.write_cursor += payload_len;
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    fn latest(&self) -> Option<CachedObjectRef<'_>> {
        self.meta.back().map(|m| CachedObjectRef {
            meta: &m.inner,
            data: &self.data[m.start..m.start + m.inner.payload_len],
        })
    }

    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>> {
        let m = self.meta.back_mut()?;
        let end = m.start + m.inner.payload_len;
        let data = &mut self.data[m.start..end];
        Some(CachedObjectMutRef { meta: &mut m.inner, data })
    }

    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>> {
        self.meta
            .iter()
            .find(|m| m.inner.location == loc)
            .map(|m| CachedObjectRef {
                meta: &m.inner,
                data: &self.data[m.start..m.start + m.inner.payload_len],
            })
    }

    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>> {
        let m = match loc {
            None => self.meta.front(),
            Some(l) => self.meta.iter().find(|m| m.inner.location > l),
        }?;
        Some(CachedObjectRef {
            meta: &m.inner,
            data: &self.data[m.start..m.start + m.inner.payload_len],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(group: u64, object: u64) -> Location {
        Location { group, object }
    }
    fn kv() -> KeyValuePairs {
        KeyValuePairs::new()
    }

    #[test]
    fn packing_small_objects() {
        let mut cache = PackedObjectCache::new(3, 10, 10);
        for i in 0..3u64 {
            cache.begin(loc(0, i), kv(), 2).unwrap();
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
        cache.begin(loc(0, 0), kv(), 4).unwrap();
        {
            let mut o = cache.latest_mut().unwrap();
            let s = &[0; 4];
            o.unfilled()[..s.len()].copy_from_slice(s);
            o.advance_write_pos(s.len());
        };
        cache.begin(loc(0, 1), kv(), 4).unwrap();
        {
            let mut o = cache.latest_mut().unwrap();
            let s = &[1; 4];
            o.unfilled()[..s.len()].copy_from_slice(s);
            o.advance_write_pos(s.len());
        };
        cache.begin(loc(0, 2), kv(), 4).unwrap();
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
    fn overflow_returns_error() {
        let mut cache = PackedObjectCache::new(2, 6, 4);
        assert!(cache.begin(loc(0, 0), kv(), 6).is_ok());
        assert!(cache.begin(loc(0, 1), kv(), 6).is_err());
    }
}
