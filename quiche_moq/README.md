# quiche_moq

Ties `moq_core`'s sans-IO `MoqTransportSession` to a real `quiche` connection: `QuicheTransport` implements
`moq_core::Transport` on a WebTransport session (`quiche_webtransport`) or directly on the QUIC connection
(native QUIC), so an application can drive the session with whatever event loop it likes (`moq_cli` uses `mio`
via `quiche_mio_runner`; see `moq_core_async`/`moq_tokio` for the async path built on `tokio-quiche` instead).

`MoqHandle` (in `moq_handle`) is the top-level type applications drive; the
`qlog` feature wires up MoQT qlog event emission. Example binaries (time
client/server, video client/server) live under `examples/`; the relay is `moq_cli`.
