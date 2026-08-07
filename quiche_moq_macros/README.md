# quiche_moq_macros

Proc-macro used by `quiche_moq`: `#[generate_moq_handle]` is applied to an
`impl` block whose public methods take a trailing `&mut Connection`
parameter (named `wt`, `h3`, or `quic`), and generates matching forwarding
methods on `MoqHandle` with that parameter stripped — since `MoqHandle`
already owns the connection, callers don't have to pass it in themselves.
