# moq_wire

Wire encoding for [MoQ Transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/):
control messages (`SETUP`, `SUBSCRIBE`, `PUBLISH`, `FETCH`, ...), subgroup
and fetch streams (`fetch`), and the shared parts they're built from (key-value
pairs, namespaces, locations, setup options, range filters). Pure encode/decode, no session state machine and no
I/O; that's `moq_core`, built on top of this crate.

Built on `moq_wire_primitives`: `version.reader(buf)` / `version.writer(&mut vec)`
pick the draft's varint form, so every field uses the negotiated encoding.
A control message body is decoded as a complete frame: fields running past
its length and leftover bytes are a `ProtocolViolation`, input that ends early
is `Incomplete { needed }`.

Supports MoQT drafts 7 to 19 (see `version.rs`). The types follow draft 19's
names and concepts and map to each older draft's wire format, e.g. delta-encoded
key-value pair types from draft 15 and leading-ones varints from draft 17. What a
draft can't express is `Unsupported` when encoding, and a hint it lacks is left out.
