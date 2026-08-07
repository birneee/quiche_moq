# moq_core_async

Portable async building blocks for driving `moq_core`'s synchronous, sans-IO
`MoqTransportSession` from an async caller.

No platform dependencies — no tokio, no wasm-bindgen/web-sys. Callers
implement the `AsyncTransport` trait for their own transport (browser
WebTransport, a native tokio socket, ...) and get back `async fn`s like
`connect()`/`subscribe()`/`next_object()`. This is the shared foundation
`moq_web` and `moq_tokio` both build their platform-specific APIs on.
