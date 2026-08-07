# moq_loc

Typed wrappers for [Low Overhead Media Container (LOC)](https://datatracker.ietf.org/doc/draft-ietf-moq-loc/)
properties — draft-ietf-moq-loc-04 §2.3 — the per-object metadata (codec
config, timestamp, timescale, audio level, video frame marking) carried in
MoQ object extension headers as key-value pairs.

Converts to/from `moq_wire::KeyValuePair`; doesn't know about sessions,
transports, or I/O.
