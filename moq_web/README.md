# moq_web

Browser-side MoQ subscriber using the browser's native WebTransport API.
Implemented in Rust/WASM on top of `moq_core`, with WebCodecs for video and
audio playback.

No bundler or npm required.

## Build

Requires `wasm-pack` (available in the repo devShell via `nix develop`) and `curl`, which fetches
[stats.js](https://github.com/mrdoob/stats.js) for the stats panels:

```sh
./make-dist
```

Or the same files without `wasm-opt`, in `result/`, from the repository root:

```sh
nix build .#web-player
```

## Run locally

```sh
python3 -m http.server 8080 --directory dist/
```

Then open `http://localhost:8080/?relay=https://<relay>` in a browser.

URL parameters:

| param   | default                 | description                                  |
|---------|-------------------------|----------------------------------------------|
| `relay` | `https://127.0.0.1:443` | MoQ relay/server WebTransport URL            |
| `video` | `tos--video`            | video track namespace--name                  |
| `audio` | `tos--audio`            | audio track namespace--name                  |
| `draft` | (negotiated)            | pin the MoQ draft version, e.g. `18`         |
| `log`   | `0` (off)               | log every stat as JSON every N ms            |

Below the video, panels show the MoQ session, each track's playout buffer and the WebTransport connection;
`moqStats()` in the browser console logs one snapshot of them.

Because the relay uses a self-signed TLS certificate, launch Chromium with:

```sh
chromium --no-sandbox --ignore-certificate-errors \
  "http://localhost:8080/?relay=https://<relay>"
```

## nixnet experiment

A ready-made two-node testbed (server publishes Tears of Steel, client opens the browser), with `moq-cli` and
the web player built from this repository, in `experiments/`. From the repository root:

```sh
nix run .#experiments.web
```
