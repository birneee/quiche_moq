# interop

Integration with [moq-interop-runner](https://github.com/englishm/moq-interop-runner), registered there as `birneee-moq`.
This directory contains everything that integration needs: the image (`interop.nix`, built by
`nix build .#interop-image`), the relay (`entrypoint-relay.sh`) and the TAP test client (`interop-client.sh`,
`lib.sh`, one script per test in `tests/`), both wrapping `moq`.

## Running the tests locally

Our client against our relay, no containers:

    cargo build --release -p moq-cli && bash interop/run-local.sh [-t TEST] [-v]

Every registered implementation against every other, ours included, in the Nix build sandbox:

    nix build .#interop-matrix -j 4   # result/results/report.html, summary.json, one log per pair

`sandbox/` runs the unchanged runner there. Its images, pinned by digest in `sandbox/pins.json`, are unpacked
and started with bubblewrap by `sandbox/docker`, which stands in for docker: no daemon, no VM. Failing pairs
are reported, the build succeeds. Refresh the pins with `bash interop/sandbox/update-pins.sh`.

Each relay and client pair is its own derivation, depending only on their two images: a change to our
implementation reruns only the pairs with `birneee-moq`, the others come from the store. Pairs run in parallel,
each sandbox has its own network; few jobs keep the tests' timeouts reliable. A single pair:

    nix build .#interop.pairs.moq-rs-draft-18--birneee-moq    # <relay>--<client>, result/results/*.log
