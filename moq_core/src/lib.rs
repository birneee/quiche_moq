#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

/// What std's prelude adds over core's.
mod prelude {
    #[allow(unused_imports)]
    pub(crate) use alloc::{borrow::ToOwned, boxed::Box, format, string::{String, ToString}, vec, vec::Vec};
}

mod control_channel;
pub mod config;
pub mod error;
mod handles;
mod in_stream;
mod in_track;
mod out_stream;
mod out_track;
mod padding_stream;
mod pending_subscribe;
pub mod request;
pub mod session;
pub mod time;
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
pub mod transport;

pub use config::{Config, VersionNegotiation};
pub use error::{Error, Result};
pub use handles::{Announcement, FetchResponse, Fetching, NamespaceSubscription, Publication, Subscription, TracksSubscription};
pub use request::{DropQueue, Dropped, FetchOk, IncomingRequest, LocalDropQueue, PublishOk, Request, RequestKind, SubscribeOk};
#[cfg(feature = "std")]
pub use request::SharedDropQueue;
pub use time::Timestamp;
pub use session::{Event, FetchOptions, MoqTransportSession, RequestFailure, OutgoingDatagram, OutgoingObject, RecvObjHdr, SessionStats, SubscribeOptions};
pub use moq_wire::LocationFilter;
pub use transport::{StreamError, Transport};
pub use moq_wire as wire;

