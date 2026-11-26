#!/usr/bin/env bash
# Subscribe to a Cloudflare MoQT relay broadcast and print it to stdout.
# See https://blog.cloudflare.com/moq-relays/
#
# Usage: TOKEN=<token> ./subscribe.sh

set -euo pipefail

: "${TOKEN:?set TOKEN to the broadcast subscribe token}"

draft=16

moq --connect "https://draft-${draft}.cloudflare.mediaoverquic.com/${TOKEN}#upstream" \
    --setup-version "${draft}" \
    --sub chat--messages \
    --out chat--messages
