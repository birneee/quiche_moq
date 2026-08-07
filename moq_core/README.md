# moq_core

Transport-agnostic implementation of the [Media over QUIC Transport (MoQT)](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) session state machine.

## Design

`moq_core` follows the [sans-IO](https://sans-io.readthedocs.io/) pattern. All protocol logic lives here; no I/O is performed directly. Instead, callers provide a [`Transport`] implementation that handles the actual network operations.

```
┌─────────────────────────────────────────────┐
│                  Application                │
└────────────────────┬────────────────────────┘
                     │
┌────────────────────▼────────────────────────┐
│              transport                      │
│           e.g. QuicheTransport              │
└────────────────────┬────────────────────────┘
                     │ impl Transport
┌────────────────────▼────────────────────────┐
│                 moq_core                    │  ← this crate
│         MoqTransportSession                 │
└─────────────────────────────────────────────┘
```

## Features

| Feature | Description |
|---------|-------------|
| `qlog`  | Emit MoQT qlog events via `Transport::qlog_streamer()`. Requires `serde_json`. |

## Flow Chart

A typical `Transport` driving `MoqTransportSession` on top of QUIC/WebTransport:

```
   +--------------------------------------------+
   |                                             |
   |                                             v
   |     🗲 io event or timer fires
   |                          v
   |     QUIC: process UDP packets, or on_timeout()
   |                          v
   |     per connection: advance H3 / WebTransport / MoQ
   |     handshake state (one-time, until session ready)
   |                          v
   |     MoqTransportSession::handle()
   |     (parse control messages + object streams)
   |                          v
   |     app logic: manage subscriptions/publishes,
   |     read + forward + send MoQ objects
   |                          v
   |     MoqTransportSession::post_handle()
   |                          v
   |     QUIC: send_all(), then collect garbage
   |                                             |
   +---------------------------------------------+
```