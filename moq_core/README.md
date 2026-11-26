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

## Principles

The goal: using `moq_core` can hardly be done wrong, while implementations stay free in how they run it.

### Sans-IO

- The session owns no socket, thread, timer or clock source of its own.
- The transport is passed into each call, never stored.
- Time is passed in: `handle()` takes the current `Timestamp`, calls until the next `handle()` use it, and `timeout_at()` says when to call `handle()` again. Tests move a clock forward instead of sleeping.
- The same state machine runs on quiche, tokio and in the browser.

### `core` and `alloc` only

- `#![no_std]`, so the compiler enforces sans-IO: no clock, socket or thread can sneak in.
- The default `std` feature adds what needs it: `Timestamp::now()`, `SharedDropQueue`, and qlog.
- `cargo build -p moq_core --no-default-features` checks this. The `octets` dependency still links std.

### Policy stays with the caller

- The session implements the protocol, the caller decides: accept or reject, what to cache, what to forward, which priority.
- A request does not have to be answered when it arrives. A relay can ask upstream first and answer later.

### Obligations are linear types

Whatever must be resolved exactly once is a value the caller owns, not an id it has to remember.

| Value | Created by | Resolved by | If dropped |
|-------|-----------|-------------|------------|
| `Request<K>` | incoming SUBSCRIBE, PUBLISH, PUBLISH_NAMESPACE, REQUEST_UPDATE | `accept_request` or `reject_request` | rejected |
| `Subscription` | `subscribe`, accepting a `Request<Publish>` | `unsubscribe` | unsubscribed |
| `Publication` | accepting a `Request<Subscribe>`, `publish_track` | `publish_done` | PUBLISH_DONE with an error status, open subgroups reset |
| `Announcement` | `publish_namespace` | `publish_namespace_done` | PUBLISH_NAMESPACE_DONE |

- Resolving takes the value by move, so nothing is answered or ended twice.
- These values are neither `Clone` nor `Copy`, and can only be created by the session.
- Rust has no linear types yet, only affine ones: a value can still be dropped. A drop therefore resolves the obligation with the protocol's own error or cancel message on the next `handle()`, so the peer always gets an answer and no state leaks.
- Racing with the peer is not a bug: answering a request the peer withdrew returns `Error::RequestCancelled`, ending what the peer already ended does nothing.
- Passing a value to a session that did not create it is a bug and panics.
- Objects and subgroups are not linear. They are the hot path and are keyed by location; ending their `Publication` or `Subscription` cleans them up.

### Atomics only when necessary

- The session holds no shared state and needs no lock.
- A dropped value reaches its session through a `DropQueue`, chosen by the implementation as a type parameter:
  - `LocalDropQueue` for thread-per-core: no atomics, not `Send`.
  - `SharedDropQueue` for multi-threaded runtimes: `Send`.
  - or its own, e.g. an existing command queue.
- `set_drop_waker` lets an async driver run `handle()` as soon as a value is dropped.
- The queue is touched when a value is created or dropped, never per object or packet.

### Nothing is missed, nothing grows unbounded

- Everything the peer does is an `Event` from `next_event()`, in arrival order, so ignoring a return value cannot lose it.
- Every piece of per-peer state has an end: a message, a stream closing, or a timeout.

### Types over conventions

- Request ids, track aliases, stream ids and status codes are distinct types.
- Only the session, its configuration, the transport trait, events and errors are public.
- Calls do not depend on being made in a certain order.

### Fail loudly

- A protocol violation closes the session with the matching error code.
- No peer input panics: malformed or oversized messages close the session, buffers are bounded.
- Maps keyed by values the peer chooses use a keyed hasher.
- Each `Error` says whether it is permanent or worth retrying.
- Calls on a closed session fail with `Error::SessionClosed` instead of sending into the void.

> TODO a distinct type for stream ids.

## Features

| Feature | Description |
|---------|-------------|
| `std` (default) | `Timestamp::now()` and `From<Instant>`, `SharedDropQueue`. |
| `qlog`  | Emit MoQT qlog events via `Transport::qlog_streamer()`. Requires `std` and `serde_json`. |

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
   |     MoqTransportSession::send_padding()
   |                          v
   |     QUIC: send_all(), then collect garbage
   |                                             |
   +---------------------------------------------+
```