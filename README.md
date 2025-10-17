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

## Features

- multi version support
  - [x] draft 07
  - [x] draft 08
  - [x] draft 09
  - [x] draft 10
  - [x] draft 11
  - [x] draft 12
  - [x] draft 13
  - [ ] draft 14
- [x] MoQ via WebTransport
- [ ] MoQ via QUIC
- [x] subscribe
- [ ] fetch
- [ ] announce
- [x] publish
- [ ] unannounce
- [ ] unsubscribe
- [ ] track status
- [x] streams
- [ ] datagrams
- interop
  - [ ] [Cloudflare](https://blog.cloudflare.com/moq/)
    - [x] handshake
    - [x] subscribe
    - [ ] publish
  - [x] [moqtransport](https://github.com/mengelbart/moqtransport)
    - [x] handshake
    - [x] subscribe
    - [x] publish

## Flow Chart
```
   +-----------------+
   |                 |
   |                 v
   |     🗲 io event or timer fires
   |                 ∨
   |     QUIC process all UDP packets & timeouts
   |                 v
   |     setup H3 connections
   |                 v
   |     update H3 from QUIC state
   |                 v
   |     update WebTransport state from H3
   |                 v
   |     setup WebTransport sessions
   |                 v
   |     setup MoQ sessions
   |                 v
   |     update MoQ from WebTransport state
   |                 v
   |     read MoQ objects
   |                 v
   |     manage MoQ subscriptions
   |                 v
   |     send MoQ objects
   |                 v
   |     QUIC collect garbage
   |                 |
   |                 |
   +-----------------+
```
