#!/bin/bash
# entrypoint-relay.sh - the moq-interop-runner relay: `moq` as an open relay on MOQT_PORT (default 4443),
# pinned to draft 18, with the certificate the runner mounts at MOQT_CERT/MOQT_KEY (default /certs/), else a
# generated one.

set -euo pipefail

PORT="${MOQT_PORT:-4443}"
CERT="${MOQT_CERT:-/certs/cert.pem}"
KEY="${MOQT_KEY:-/certs/priv.key}"

tls=()
[ -f "$CERT" ] && tls=(--cert "$CERT" --key "$KEY")

echo "Starting birneee-moq relay on port $PORT (draft-18)"
# accepts every PUBLISH_NAMESPACE, PUBLISH and SUBSCRIBE and re-announces every namespace to all peers
# TODO --listen "$PORT#native" once the runner can give clients a moqt:// URL of this relay,
#  it delays WebTransport sessions by a round trip
exec moq --listen "$PORT" "${tls[@]}" --setup-version 18 --relay-namespace '*' --pub '*' --sub '*'
