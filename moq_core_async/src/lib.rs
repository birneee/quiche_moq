//! Portable async building blocks for driving `moq_core`'s synchronous,
//! sans-IO `MoqTransportSession` from an async caller. No platform
//! dependencies (no tokio, no wasm-bindgen/web-sys) — callers implement
//! [`AsyncTransport`] for their own transport (WASM WebTransport, a native
//! tokio socket, ...) and get `async fn`s like `subscribe()` in return.

mod actor;
mod buffered_track;
mod in_track;
mod moq_session;
mod notify;
mod session;
mod transport;

pub use actor::{AsyncMoqSessionActor, AsyncMoqSessionHandle};
pub use buffered_track::{BufferedTrack, CompleteObject, TrackStats};
pub use moq_session::{ConnectError, MoqSession, MoqTrack, TrackObjects};
pub use notify::{Notified, Notify};
pub use session::{AsyncMoqSession, SubscribeError, Subscribed};
pub use transport::AsyncTransport;
