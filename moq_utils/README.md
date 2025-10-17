# MoQ Utils

```shell
while true; do date --rfc-3339=s; sleep 1; done | \
moq-utils pub https://relay.cloudflare.mediaoverquic.com/
```

```shell
moq-utils sub --namespace "yawning-impala" --trackname "video" https://relay.cloudflare.mediaoverquic.com/
```
