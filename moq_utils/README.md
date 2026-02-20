# MoQ Utils

publish seconds

```shell
$ while true; do date --rfc-3339=ns; sleep 1; done | \
moq-utils pub -t time--seconds https://127.0.0.1:8080
[2026-02-20T11:06:27Z INFO  moq_utils::publish] connect to 127.0.0.1:8080
[2026-02-20T11:06:27Z INFO  moq_utils::publish] publishing namespace time
[2026-02-20T11:06:27Z INFO  moq_utils::publish] announced namespace time successfully
[2026-02-20T11:06:29Z INFO  moq_utils::publish] accepting subscription to time--seconds
```

subscribe seconds

```shell
$ moq-utils sub -t time--seconds -s '\n' https://127.0.0.1:8080
[2026-02-20T11:06:29Z INFO  moq_utils::subscribe] connect to 127.0.0.1:8080
[2026-02-20T11:06:29Z INFO  moq_utils::subscribe] namespace announced: time
[2026-02-20T11:06:29Z INFO  moq_utils::subscribe] request subscribe: time--seconds
[2026-02-20T11:06:29Z INFO  moq_utils::subscribe] subscribe accepted: time--seconds
2026-02-20 12:06:27.145408448+01:00
2026-02-20 12:06:28.149532851+01:00
2026-02-20 12:06:29.153622718+01:00
```
