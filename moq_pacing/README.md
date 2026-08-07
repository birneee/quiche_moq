# moq_pacing

Buffer-level → playback-speed-multiplier logic, shared by every MoQ
subscriber that presents timed media on its own clock (fMP4 file/pipe output
in `moq_cli`, `AudioContext` scheduling in `moq_web`).

Pure logic, no I/O and no OS/browser APIs, so it works identically on native
and `wasm32-unknown-unknown`. Nudges the presented media clock by a few
percent based on how much is buffered ahead of the consumer — a jitter
smoother, not a fast-forward/rewind control.
