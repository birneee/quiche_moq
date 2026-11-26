#!/usr/bin/env bash
# Publish lines typed on stdin to a Cloudflare MoQT relay broadcast.
# See https://blog.cloudflare.com/moq-relays/
#
# Usage: TOKEN=<token> ./publish.sh

set -euo pipefail

: "${TOKEN:?set TOKEN to the broadcast publish token}"

draft=16

moq --connect "https://draft-${draft}.cloudflare.mediaoverquic.com/${TOKEN}" \
    --setup-version "${draft}" \
    --pub chat \
    --pub "chat--messages#passive" \
    --in chat--messages
