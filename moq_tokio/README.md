# moq_tokio

Native MoQ-over-WebTransport client and server on top of `tokio` and
`tokio-quiche`.

Built on `moq_core_async`: `WebTransportHandle` implements its
`AsyncTransport` trait (using this workspace's `quiche_webtransport` crate
for the extended-CONNECT handshake and WT stream framing, driven via
tokio-quiche's `ApplicationOverQuic`), so `MoqSession`/`MoqTrack` here are
plain type aliases over `moq_core_async`'s generic types — the same
`connect()`/`subscribe()`/`next_object()` shape `moq_web` exposes in the
browser. `MoqServer` adds the server-side accept/reject/publish flow.
