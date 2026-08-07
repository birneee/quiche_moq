# quiche_moq_web

Browser-side MoQ subscriber using the browser's native WebTransport API.
Implemented in Rust/WASM via `quiche_moq_wire` for protocol parsing and the
MSE (Media Source Extensions) API for video/audio playback.

No bundler or npm required.

## Build

Requires `wasm-pack` (available in the repo devShell via `nix develop`):

```sh
./make-dist
```

## Run locally

```sh
python3 -m http.server 8080 --directory dist/
```

Then open `http://localhost:8080/?relay=https://<relay>` in a browser.

URL parameters:

| param    | default          | description                        |
|----------|------------------|------------------------------------|
| `relay`  | `https://127.0.0.1:443` | MoQ relay/server WebTransport URL |
| `video`  | `tos--video`     | video track namespace--name        |
| `audio`  | `tos--audio`     | audio track namespace--name        |
| `vcodec` | `avc1.640028`    | MSE video codec string             |
| `acodec` | `mp4a.40.2`      | MSE audio codec string             |

Because the relay uses a self-signed TLS certificate, launch Chromium with:

```sh
chromium --no-sandbox --ignore-certificate-errors \
  "http://localhost:8080/?relay=https://1.0.0.1"
```

## nixnet experiment

A ready-made two-node testbed (server publishes Tears of Steel, client opens
the browser) lives in `nixnet/`. Requires `moq` and the WASM dist to be built
first (see above), then:

```sh
nix run ./nixnet
```
