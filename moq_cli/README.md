# moq_cli

A unified MoQ node binary (`moq`): publish, subscribe, relay, and route tracks
over MoQ transport, built on `quiche_moq`/`moq_core`.

## Usage

```shell
$ moq --listen 8080 --pub-passive "*" --sub-passive "*"
```

A node can simultaneously listen for incoming connections (`--listen`) and
connect out to others (`--connect`), and combine any number of active/passive
publish/subscribe roles:

| Flag | Role |
|------|------|
| `--pub <NS[--TRACK][#in=FILE]>` | Actively publish a track, or announce a namespace |
| `--pub-passive <NS--TRACK\|NS--*\|*>` | Serve a track on demand / auto-accept published namespaces |
| `--sub <NS--TRACK[#PARAMS]>` | Actively subscribe to a track |
| `--sub-passive <NS--TRACK\|NS--*\|*>` | Accept incoming publishes / relay-forward subscriptions upstream |
| `--relay-namespace <NS\|NS--*\|*>` | Re-announce namespaces to all peers (discovery hub) |

Exactly one `--connect` target can be marked `#upstream`; subscriptions for
namespaces this node doesn't know locally are forwarded there, and namespaces
it announces become available to local subscribers. This, together with
wildcard `*`/`NS--*` patterns on the passive flags, is what makes a node act
as a relay.

Run `moq --help` for the full flag reference, including per-connection QUIC
tuning (congestion window, flow control, stream limits), keepalive/PTO
control, delivery-timeout/priority SUBSCRIBE options, and TLS/qlog setup.

## Examples

```shell
# publisher: stream lines typed on stdin as chat--messages
$ moq --listen 8080 --pub chat--messages

# subscriber: connect and print the track to stdout
$ moq --connect https://127.0.0.1:8080 --sub chat--messages
```

