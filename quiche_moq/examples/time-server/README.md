# time-server

Minimal MoQ publisher on `quiche_moq`: listens on UDP 8080 and sends the
current time as one object per group, once a second, to every subscriber.
Pair with `time-client`.
