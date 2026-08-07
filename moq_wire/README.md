# moq_wire

Wire encoding for [MoQ Transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/):
control messages (`CLIENT_SETUP`, `SUBSCRIBE`, `PUBLISH`, ...), object
headers, and the shared primitives they're built from (varints, key-value
pairs, tuples, namespaces). Pure encode/decode — no session state machine
and no I/O; that's `moq_core`, built on top of this crate.

Supports multiple MoQT draft versions (see `version.rs`) since the wire
format has changed across drafts — most notably switching key-value pair
type IDs from absolute to delta-encoded starting at draft-15.
