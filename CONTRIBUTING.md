# Contributing

Contributions are welcome — feel free to open issues or pull requests.

## Guidelines

- keep code elegant and readable
- give variables and files descriptive names
- find good abstractions, avoid duplication
- use files as logical units, single purpose and short
- keep comments and error messages short, terse fragments instead of full sentences
- add tests and assertions for features and behavior
- keep git diffs minimal
- verify against draft-ietf-moq-transport
- remove unnecessary code and tests

## Testing

- run what CI checks with `nix develop -c ./check.sh`: clippy, the tests, the wasm and `no_std` builds
