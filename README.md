<div align="center">
    <img src="logo.svg" width="400">
</div>

# QUICHE MoQ

> ⚠️ Not production ready

> This project is not affiliated with [Cloudflare quiche](https://github.com/cloudflare/quiche)

A [Media over QUIC (MoQ)](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) implementation for [quiche](https://github.com/cloudflare/quiche).

## Time example

```shell
$  RUST_LOG=info cargo run -p time-server
[INFO  quiche_utils::cert] generate self signed TLS certificate
[INFO  quiche_utils::cert] certificate spki: rugY1MtvoZbK+pVqkoojKCLBJMr2S6gKjacqkO2YqDY=
[INFO  time_server] start server on 0.0.0.0:8080
```

```shell
$ RUST_LOG=info cargo run -p time-client
[INFO  time_client] connect to 127.0.0.1:8080
[INFO  time_client] subscribe clock second
[INFO  time_client] received "2025-10-17T11:37:17.265898446+02:00"
[INFO  time_client] received "2025-10-17T11:37:18.268052136+02:00"
[INFO  time_client] received "2025-10-17T11:37:19.270110847+02:00"
```

## Architecture

`moq_wire` (wire format) → `moq_core` (sans-IO session state machine, talking
to a small `Transport` trait) → a `Transport` impl per platform: `quiche_moq`
(native QUIC/WebTransport, used by `moq_cli`) and `moq_web` (browser
WebTransport). See each crate's own README for details.

## Features

- multi version support: draft 07 through draft 18 (see caveat below for 17/18)
- subscribe, publish, and namespace announce/unannounce over WebTransport
- relaying with selective namespace re-announcement, priority propagation, and
  delivery-timeout aggregation across downstream subscribers
- in-browser client (`moq_web`) with live WebCodecs video/audio playback
- adaptive playback pacing and pluggable object caching, shared between the
  native and browser clients
- typed [LOC](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/)
  object-extension headers, driving automatic codec setup and keyframe
  detection
- fMP4 demuxing and remuxing with automatic codec detection
- [qlog](https://datatracker.ietf.org/doc/draft-pardue-moq-qlog-moq-events/05/)
  tracing for control messages and object delivery
- runtime-tunable QUIC transport (congestion window, flow control, keepalive)
  and a runtime-toggleable padding stream
- interop tested against [Cloudflare](https://blog.cloudflare.com/moq/) and
  [moqtransport](https://github.com/mengelbart/moqtransport)

## Planned Features

- MoQ via native QUIC (WebTransport only today)
- fetch, unsubscribe, subscribe-to-namespace / track status
- datagrams (streams only today)

> Draft 17+ negotiates its version via ALPN (QUIC) or the
> `WT-Available-Protocols` header (WebTransport, §3.1.3) instead of the SETUP
> message — but that header isn't settable from the browser's `WebTransport`
> JS API, so this implementation keeps the old in-band SETUP negotiation at
> every draft as a deliberate, non-compliant testing simplification.
