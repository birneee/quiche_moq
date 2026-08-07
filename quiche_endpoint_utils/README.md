# quiche_endpoint_utils

Small standalone helpers for `quiche_mio_runner`-based endpoints: qlog setup
(enabled via the `QLOGDIR` environment variable) and `SignalSource`, a `mio`
event source (via Linux `signalfd`) that becomes readable when a specified
Unix signal arrives — used for both shutdown and recurring signals like
SIGUSR1.
