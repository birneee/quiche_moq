use moq_wire::{KeyValuePairs, Location};
use moq_wire::object::ObjectStatus;
use crate::NewObject;
use web_time::Instant;

pub(crate) struct ObjectMeta {
    pub(crate) location: Location,
    /// Subgroup ID this object was received on — a publisher's own semantic
    /// grouping decision (draft-ietf-moq-transport-19 §2.2) that also affects send
    /// scheduling priority (§7), so a relay forwarding this object should
    /// reuse it rather than assign its own.
    pub(crate) subgroup_id: u64,
    /// The Subgroup's, kept by a relay forwarding it (draft-ietf-moq-transport-18 §7.3).
    pub(crate) publisher_priority: u8,
    pub(crate) properties: KeyValuePairs,
    pub(crate) payload_len: usize,
    pub(crate) write_pos: usize,
    pub(crate) received_at: Instant,
    /// Whether this is confirmed to be the last Object of its Subgroup — set
    /// after the fact via `ObjectCache::mark_end_of_subgroup` once whoever
    /// filled the cache actually knows (a local publisher detecting its own
    /// Group boundary, or a relay observing the upstream Subgroup's stream
    /// FIN). `false` just means "not confirmed," not "definitely more
    /// coming."
    pub(crate) end_of_subgroup: bool,
    /// Its Subgroup holds the Group's largest Object, END_OF_GROUP of
    /// draft-ietf-moq-transport-18 §11.4.2. `false` if not known.
    pub(crate) end_of_group: bool,
    /// Its Object Forwarding Preference is Datagram (draft-ietf-moq-transport-18 §11.3): it
    /// belongs to no Subgroup, and `end_of_group` says it is the Group's last Object.
    pub(crate) datagram: bool,
    /// The upstream Subgroup was reset after this, its last received Object:
    /// more Objects may exist (draft-ietf-moq-transport-19 §11.4.3), so it must not be FIN'd.
    pub(crate) subgroup_reset: bool,
    /// Set by `cancel()`: the source stream was reset mid-object, it won't
    /// complete unless `restart()`ed. Readers and forwarders must skip it.
    pub(crate) canceled: bool,
    /// The Object Status of an object without payload, cached as received (draft-ietf-moq-transport-18 §11.2.1.1).
    pub(crate) status: ObjectStatus,
}

impl ObjectMeta {
    /// A Normal object begun now, nothing of its payload written.
    pub(crate) fn new(object: NewObject) -> Self {
        let NewObject { location, subgroup_id, publisher_priority, end_of_subgroup, properties, payload_len } = object;
        Self {
            location, subgroup_id, publisher_priority, properties, payload_len, write_pos: 0, received_at: Instant::now(), end_of_subgroup,
            end_of_group: false, datagram: false, subgroup_reset: false, canceled: false, status: ObjectStatus::Normal,
        }
    }

    fn state(&self) -> ObjectState {
        if self.canceled {
            ObjectState::Canceled
        } else if self.write_pos >= self.payload_len {
            ObjectState::Completed
        } else {
            ObjectState::Progressing
        }
    }
}

/// Where a cached object stands relative to its announced `payload_len`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectState {
    /// Fewer than `payload_len` bytes written so far; still waiting on more.
    Progressing,
    /// All `payload_len` bytes written.
    Completed,
    /// `cancel()`'d — its source will never deliver the rest. Distinct from
    /// `Completed`: readers must not treat its payload as the real object.
    Canceled,
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
    pub fn subgroup_id(&self) -> u64 {
        self.meta.subgroup_id
    }
    pub fn publisher_priority(&self) -> u8 {
        self.meta.publisher_priority
    }
    pub fn end_of_subgroup(&self) -> bool {
        self.meta.end_of_subgroup
    }
    pub fn status(&self) -> ObjectStatus {
        self.meta.status
    }
    pub fn end_of_group(&self) -> bool {
        self.meta.end_of_group
    }
    pub fn datagram(&self) -> bool {
        self.meta.datagram
    }
    pub fn subgroup_reset(&self) -> bool {
        self.meta.subgroup_reset
    }
    pub fn properties(&self) -> &KeyValuePairs {
        &self.meta.properties
    }
    pub fn payload_len(&self) -> usize {
        self.meta.payload_len
    }
    pub fn received_at(&self) -> Instant {
        self.meta.received_at
    }
    pub fn state(&self) -> ObjectState {
        self.meta.state()
    }
    pub fn write_pos(&self) -> usize {
        self.meta.write_pos
    }
    pub fn readable_from(&self, read_pos: usize) -> &[u8] {
        &self.data[read_pos.min(self.meta.write_pos)..self.meta.write_pos]
    }
}

impl CachedObjectMutRef<'_> {
    pub fn state(&self) -> ObjectState {
        self.meta.state()
    }
    pub fn write_pos(&self) -> usize {
        self.meta.write_pos
    }
    /// Cheap reborrow as an immutable view — no cache lookup.
    pub fn as_ref(&self) -> CachedObjectRef<'_> {
        CachedObjectRef { meta: self.meta, data: self.data }
    }
    pub fn unfilled(&mut self) -> &mut [u8] {
        &mut self.data[self.meta.write_pos..self.meta.payload_len]
    }
    pub fn advance_write_pos(&mut self, n: usize) {
        self.meta.write_pos += n;
    }
    /// `payload_len` is left as originally announced — `PackedObjectCache`
    /// still needs it to know the byte range this object physically
    /// reserved (see its wraparound collision check in `begin()`).
    /// `write_pos` resets to 0 so a caller that reads `readable_from(0)`
    /// without checking `state()` first sees an empty slice rather than a
    /// truncated fragment of the payload.
    pub fn cancel(&mut self) {
        self.meta.write_pos = 0;
        self.meta.canceled = true;
    }
    /// Receives a canceled object again from the start, e.g. resent on a
    /// new stream after its subgroup was reset.
    pub fn restart(&mut self) {
        self.meta.write_pos = 0;
        self.meta.canceled = false;
        self.meta.subgroup_reset = false;
    }
    pub fn set_end_of_subgroup(&mut self) {
        self.meta.end_of_subgroup = true;
    }
    pub fn set_end_of_group(&mut self) {
        self.meta.end_of_group = true;
    }
    pub fn set_datagram(&mut self) {
        self.meta.datagram = true;
    }
    pub fn set_subgroup_reset(&mut self) {
        self.meta.subgroup_reset = true;
    }
    pub fn set_status(&mut self, status: ObjectStatus) {
        self.meta.status = status;
    }
}
