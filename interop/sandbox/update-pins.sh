#!/usr/bin/env bash
# update-pins.sh - refreshes pins.json: every image moq-interop-runner's registry names, by its current amd64 digest,
# and the Nix hash of its fetch. Adapter images pin their upstream image; images only built locally have no pin.
# Needs network:
#   bash interop/sandbox/update-pins.sh [implementations.json]
# Without an argument, the registry of the runner revision default.nix pins.
# - `--hashes-from LOG`: only fill in the hashes a `nix build --keep-going` of the fetches reported in LOG

set -euo pipefail

command -v skopeo >/dev/null && command -v jq >/dev/null || exec nix shell nixpkgs#skopeo nixpkgs#jq -c bash "$0" "$@"

dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
pins="$dir/pins.json"
flake="git+file://$(cd "$dir/../.." && pwd)"

# jq_in_place FILE JQ_ARGS...
jq_in_place() {
  local file="$1"; shift
  jq "$@" "$file" > "$file.tmp" && mv "$file.tmp" "$file"
}

# fills in the hashes of the fetches LOG reports a mismatch of, pullImage names them docker-image-<name>-<tag>.tar
fill_hashes() {
  local log="$1" ref name hash
  for ref in $(jq -r 'keys[]' "$pins"); do
    name="docker-image-$(echo "$ref" | sed 's#[/:]#-#g').tar.drv"
    hash=$(grep -A3 -F -- "-$name'" "$log" | sed -n 's/^ *got: *\(sha256-[^ ]*\).*/\1/p' | head -1)
    [ -n "$hash" ] || continue
    jq_in_place "$pins" --arg ref "$ref" --arg hash "$hash" '.[$ref].hash = $hash'
  done
}

if [ "${1:-}" = --hashes-from ]; then
  fill_hashes "$2"
  exit 0
fi

registry="${1:-$(nix eval --raw "$flake#interop.runnerSrc")/implementations.json}"

echo "{}" > "$pins.new"
jq -r '.implementations[].roles[]? | .docker? // empty
  | if (.build.dockerfile // "" | startswith("adapters/")) then .upstream_image else .image end' "$registry" | sort -u |
while read -r ref; do
  case "$ref" in */*) ;; *) echo "skip $ref: only built locally" >&2; continue ;; esac
  [[ "$ref" == *:* ]] || ref="$ref:latest"
  if ! digest=$(skopeo inspect --override-os linux --override-arch amd64 --format '{{.Digest}}' "docker://$ref" 2>/dev/null); then
    echo "skip $ref: not pullable" >&2
    continue
  fi
  jq_in_place "$pins.new" --arg ref "$ref" --arg digest "$digest" '.[$ref] = { digest: $digest, hash: "" }'
done
mv "$pins.new" "$pins"

log=$(mktemp)
nix build --impure --keep-going --no-link --expr "builtins.attrValues (builtins.getFlake \"$flake\").legacyPackages.x86_64-linux.interop.archives" > "$log" 2>&1 || true
fill_hashes "$log"
rm -f "$log"
echo "pinned $(jq 'length' "$pins") images, without hash: $(jq '[.[] | select(.hash == "")] | length' "$pins")"
