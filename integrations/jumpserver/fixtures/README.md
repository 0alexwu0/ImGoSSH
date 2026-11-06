# Compatibility fixture

`client-2.1.4-config.json` is the unmodified configuration from
[jumpserver/client v2.1.4](https://github.com/jumpserver/client/blob/b134a68ab6fa0ad9fb880fa7512318a669e36bc8/go-client/config.json)
(commit `b134a68ab6fa0ad9fb880fa7512318a669e36bc8`). It contains tool defaults,
not credentials or live connection information. The offline installer tests use
it to verify selection, ordering, repeat installation and restoration against
the minimum supported client's actual configuration.
