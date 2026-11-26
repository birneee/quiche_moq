# moq_wire_primitives

The basic data types of the [MoQ Transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/)
wire format, below the protocol messages in `moq_wire`. `no_std`, no
dependencies, no `unsafe`.

- `Reader`: decodes borrowed input, a failed read consumes nothing
  - `Reader<Stream>`: input may continue, running short is `Incomplete { needed }`
  - `Reader<Frame>`: a complete frame, running short or leftover bytes are `Malformed`
  - lengths are `u64`, so a peer's length never wraps on 32-bit targets (wasm32)
- `Writer`: appends to a reused `Vec`, fails only on values the wire can't carry;
  `with_u16_length` / `with_varint_length` fill in length prefixes
- `VarIntForm`: the QUIC varint (until draft 16) or the leading-ones varint
  (draft 17+), fixed per reader and writer so every varint matches the version
- `BoundedBytes<MAX>`, `BoundedString<MAX>`: length limits as types

