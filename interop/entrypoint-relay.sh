#!/bin/bash
# entrypoint-relay.sh - moq-interop-runner relay wrapper for `moq` (moq_cli)
#
# Translates the runner's standard MoQT interop environment variables to
# `moq` CLI flags. Pinned to draft-18 (see moq_core::VersionNegotiation):
# drafts >= 15 can't be multi-candidate negotiated in one connection attempt
# by this implementation, so this relay only accepts draft-18 clients.
#
# Expected environment:
#   MOQT_ROLE  - Role to run: relay (required, only relay supported)
#   MOQT_PORT  - Port to listen on (default: 4443)
#   MOQT_CERT  - Path to TLS certificate (default: /certs/cert.pem)
#   MOQT_KEY   - Path to TLS private key (default: /certs/priv.key)
#
# Expected mounts:
#   /certs/cert.pem - TLS certificate chain (PEM)
#   /certs/priv.key - TLS private key (PEM)
#
# Exit codes:
#   0   - Clean shutdown
#   1   - Configuration error
#   127 - Unsupported role

set -euo pipefail

ROLE="${MOQT_ROLE:-relay}"
PORT="${MOQT_PORT:-4443}"
CERT="${MOQT_CERT:-/certs/cert.pem}"
KEY="${MOQT_KEY:-/certs/priv.key}"

case "$ROLE" in
  relay)
    echo "Starting birneee-moq relay on port $PORT (draft-18)"
    echo "  Cert: $CERT"
    echo "  Key:  $KEY"

    if [ ! -f "$CERT" ]; then
      echo "ERROR: Certificate not found at $CERT" >&2
      echo "Make sure /certs is mounted with cert.pem and priv.key" >&2
      exit 1
    fi
    if [ ! -f "$KEY" ]; then
      echo "ERROR: Private key not found at $KEY" >&2
      exit 1
    fi

    # --relay-namespace '*' + --pub-passive '*' + --sub-passive '*': accept
    # any PUBLISH_NAMESPACE/PUBLISH/SUBSCRIBE from any peer and re-announce
    # every known namespace to all peers, i.e. behave as an open relay.
    exec moq \
      --listen "$PORT" \
      --cert "$CERT" \
      --key "$KEY" \
      --setup-version 18 \
      --relay-namespace '*' \
      --pub-passive '*' \
      --sub-passive '*'
    ;;

  *)
    echo "Role '$ROLE' not supported by the birneee-moq relay adapter" >&2
    echo "Supported roles: relay" >&2
    exit 127
    ;;
esac
