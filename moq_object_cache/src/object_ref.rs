use moq_wire::{KeyValuePairs, Location};
use web_time::Instant;

pub(crate) struct ObjectMeta {
    pub(crate) location: Location,
    pub(crate) ext_hdrs: KeyValuePairs,
    pub(crate) payload_len: usize,
    pub(crate) write_pos: usize,
    pub(crate) received_at: Instant,
}

pub struct CachedObjectRef<'a> {
    pub(crate) meta: &'a ObjectMeta,
    /// Pre-sliced to the object's region of the backing buffer.
    pub(crate) data: &'a [u8],
}

pub struct CachedObjectMutRef<'a> {
    pub(crate) meta: &'a mut ObjectMeta,
    /// Pre-sliced to the object's region of the backing buffer.
    pub(crate) data: &'a mut [u8],
}

impl CachedObjectRef<'_> {
    pub fn location(&self) -> Location {
        self.meta.location
    }
    pub fn ext_hdrs(&self) -> &KeyValuePairs {
        &self.meta.ext_hdrs
    }
    pub fn payload_len(&self) -> usize {
        self.meta.payload_len
    }
    pub fn received_at(&self) -> Instant {
        self.meta.received_at
    }
    pub fn is_complete(&self) -> bool {
        self.meta.write_pos >= self.meta.payload_len
    }
    pub fn readable_from(&self, read_pos: usize) -> &[u8] {
        &self.data[read_pos.min(self.meta.write_pos)..self.meta.write_pos]
    }
}

impl CachedObjectMutRef<'_> {
    pub fn unfilled(&mut self) -> &mut [u8] {
        &mut self.data[self.meta.write_pos..self.meta.payload_len]
    }
    pub fn advance_write_pos(&mut self, n: usize) {
        self.meta.write_pos += n;
    }
    pub fn cancel(&mut self) {
        self.meta.payload_len = 0;
        self.meta.write_pos = 0;
    }
}
