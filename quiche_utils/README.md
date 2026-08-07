# quiche_utils

Small standalone helpers for running `quiche` QUIC endpoints: TLS
certificate loading/generation (`cert`), a QUIC stream-id helper
(`stream_id`), and `KeepaliveHandler`, which sends keepalive pings when no
packet has been received on a path for a configurable interval (working
around `quiche` not exposing per-path last-receive time directly).
