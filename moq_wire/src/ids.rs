//! Ids that must not be mixed up: distinct types around their wire value.

use core::fmt;

/// Identifies a request within a session (draft-16 §9.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// Identifies a track's objects on data streams, assigned per subscription.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrackAlias(pub u64);

impl RequestId {
    /// The next id of the same side: client ids are even, server ids odd.
    pub const fn next(self) -> Self {
        Self(self.0 + 2)
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for TrackAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
