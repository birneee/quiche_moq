use moq_wire::{KeyValuePairs, Location};

use crate::{CachedObjectMutRef, CachedObjectRef};

pub trait ObjectCache {
    /// Begin a new object. Evicts the oldest if full.
    /// Returns `Err` if the object would overflow the buffer (implementation-defined).
    fn begin(&mut self, location: Location, ext_hdrs: KeyValuePairs, payload_len: usize) -> Result<(), ()>;
    fn is_empty(&self) -> bool;
    fn latest(&self) -> Option<CachedObjectRef<'_>>;
    fn latest_mut(&mut self) -> Option<CachedObjectMutRef<'_>>;
    /// Find the cached object at exactly `loc`, if still present.
    fn find(&self, loc: Location) -> Option<CachedObjectRef<'_>>;
    /// Oldest object with location strictly after `loc`, or the oldest object if `loc` is `None`.
    fn next_after(&self, loc: Option<Location>) -> Option<CachedObjectRef<'_>>;
}
