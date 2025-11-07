# MoQ Utils

```shell
while true; do date --rfc-3339=s; sleep 1; done | \
moq-utils pub https://relay.cloudflare.mediaoverquic.com/
```

```shell
moq-utils sub --namespace "yawning-impala" --trackname "video" https://relay.cloudflare.mediaoverquic.com/
```

subscribe to time-server

```shell
$ RUST_LOG=info moq-utils sub --namespace clock --trackname second --setup-version draft13 --output - --separator $'\n'  https://127.0.0.1:8080/moq
[2025-11-07T21:08:39Z INFO  moq_utils::subscribe] connect to 127.0.0.1:8080
[2025-11-07T21:08:39Z INFO  moq_utils::subscribe] subscribed to: clock second
2025-11-07T22:08:40.205169467+01:00
2025-11-07T22:08:41.206708773+01:00
2025-11-07T22:08:42.208809883+01:00
2025-11-07T22:08:43.210662802+01:00
2025-11-07T22:08:44.212387335+01:00
2025-11-07T22:08:45.214460190+01:00
2025-11-07T22:08:46.216391891+01:00
```
