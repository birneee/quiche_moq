# moq_cli

A unified MoQ node binary (`moq`): publish, subscribe, relay, and route tracks
over MoQ transport, built on `quiche_moq`/`moq_core`.

## Usage

```shell
$ moq --listen 8080 --pub "*" --sub "*"
```

A node can simultaneously listen for incoming connections (`--listen`) and
connect out to others (`--connect`), and combine any number of active/passive
publish/subscribe roles:

| Flag | Role |
|------|------|
| `--pub <NS[--TRACK][#PARAMS]\|NS--*\|*>` | Publish a track (or wait to be asked with `#passive`), or announce a namespace. Wildcards always imply `#passive` |
| `--in <NS--TRACK[#file=FILE]>` | Feed a track from a file, default stdin |
| `--out <NS--TRACK[#file=FILE]>` | Write a track to a file, default stdout |
| `--sub <NS--TRACK[#PARAMS]\|NS--*\|*>` | Subscribe to a track (or wait to be asked with `#passive`). Wildcards always imply `#passive` |
| `--relay-namespace <NS--*\|*>` | Re-announce namespaces to all peers (discovery hub) |

An `https://` target runs the session on WebTransport, a `moqt://` target on
native QUIC (draft 15+); a listener marked `#native` accepts both.
`--pub NS--TRACK#datagram` sends the track's objects in datagrams (draft 14+).

A `--connect` target or `--listen` listener can be marked `#upstream`
(any number of connections may be); subscriptions for namespaces this node
doesn't know locally are forwarded there, and namespaces it announces become
available to local subscribers. A concrete `--sub` only actually subscribes
via a connection that has announced its track's namespace, or is marked
`#upstream` — by default just one such connection (deterministically the
earliest known); add `#multi` to subscribe via every known one at once,
merging their data into one track buffer (multipath). This, together with
wildcard `*`/`NS--*` patterns, is what makes a node act as a relay.

`#race` keeps one connection forwarding and every other known one as an
idle (Forward=0) standby; a `race-probe` signal (`--sigusr1 race-probe`)
lets the standbys race the leader and switches to whichever proves it has
the content (`#race=keep`/`#race=switch`: decide by probing instead of
duplicating data). A standby also takes over when the leader disconnects.
`--reconnect` re-dials closed `#upstream` connections and resumes concrete
`--sub` tracks at the first object not yet fully received.

Run `moq --help` for the full flag reference, including per-connection QUIC
tuning (congestion window, flow control, stream limits), keepalive/PTO
control, delivery-timeout/priority SUBSCRIBE options, and TLS/qlog setup.

## Examples

```shell
# publisher: stream lines typed on stdin as chat--messages
$ moq --listen 8080 --pub chat--messages --in chat--messages

# subscriber: connect and print the track to stdout
# (#upstream: the publisher never announces the "chat" namespace, so this
# connection must be trusted directly rather than discovered)
$ moq --connect https://127.0.0.1:8080#upstream --sub chat--messages --out chat--messages
```

