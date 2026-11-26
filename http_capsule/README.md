# http_capsule

The HTTP Capsule Protocol, [RFC 9297](https://datatracker.ietf.org/doc/rfc9297/)
§3.2: `Capsule` encodes and decodes one capsule, `Decoder` reads a capsule
sequence arriving in pieces, skipping the values of types it doesn't keep.
`quiche_webtransport` uses it for `WT_CLOSE_SESSION` on the CONNECT stream.
