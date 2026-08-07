# quiche_moq_webtransport_helper

Glues `quiche_moq` (MoQ over `quiche`) to `quiche_webtransport` (WebTransport
over `quiche::h3`): `MoqWebTransportHelper` drives a connection through QUIC
handshake → H3/WebTransport session establishment → MoQ SETUP, as either
client or server, so `mio`-driven applications (`moq_cli`) don't have to
hand-roll that state progression themselves.
