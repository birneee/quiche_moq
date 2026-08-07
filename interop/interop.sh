#!/bin/bash
# interop.sh - single-image entrypoint dispatching relay vs. client role.
#
# Uses MOQT_ROLE (moq-interop-runner's convention - see AGENTS.md's "Docker
# Image Conventions"): docker-compose.test.yml sets MOQT_ROLE=relay for the
# relay container and leaves it unset for the client container, so anything
# other than "relay" defaults to client (interop-client) behavior.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [ "${MOQT_ROLE:-}" = "relay" ]; then
  exec bash "$SCRIPT_DIR/entrypoint-relay.sh" "$@"
else
  exec bash "$SCRIPT_DIR/interop-client.sh" "$@"
fi
