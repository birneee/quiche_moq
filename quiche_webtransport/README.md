# quiche_webtransport

WebTransport-over-HTTP/3 session and stream handling on top of `quiche`'s H3
connection: the extended-CONNECT handshake (client and server), WT stream
framing (bidi/uni, including the WebTransport stream-type prefix), and
capsule-based session termination (`http_capsule`).

Used by both `quiche_moq` (via `quiche_moq_webtransport_helper`) and
`moq_tokio` as the WebTransport layer underneath MoQ; genericized over
`quiche::BufFactory` so it works with tokio-quiche's non-default buffer type
as well as quiche's default one.
