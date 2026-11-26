use quiche_mio_runner::quiche_endpoint::quiche;

/// `--qlog-level`; mirrors `quiche::QlogLevel`, which isn't a `ValueEnum`.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QlogLevelArg {
    /// Core-importance quic events only — drops the high-volume
    /// `stream_data_moved`, keeps packet_sent/received and recovery metrics.
    Core,
    /// quiche's default: Core plus Base.
    Base,
    /// Everything quiche can emit.
    Extra,
    /// None of quiche's own `quic:*`/`http3:*` events — only the `moqt:*`
    /// events moq_core writes itself. Enough for per-object analysis (object
    /// created/parsed/received carry the embedded `--timestamp` used to join
    /// server and client), at a fraction of the size and CPU: quiche's
    /// per-packet events outnumber the moqt ones by roughly 20x here.
    Moqt,
}

impl QlogLevelArg {
    /// Importance level passed to `set_qlog_with_level_and_name_filter`.
    /// Irrelevant for `Moqt`, since `name_filter()` already rejects every
    /// quiche-emitted event by name; moq_core's own `moqt:*` JsonEvents are
    /// tagged `EventImportance::Core`, which passes at any level.
    pub fn level(self) -> quiche::QlogLevel {
        match self {
            QlogLevelArg::Core | QlogLevelArg::Moqt => quiche::QlogLevel::Core,
            QlogLevelArg::Base => quiche::QlogLevel::Base,
            QlogLevelArg::Extra => quiche::QlogLevel::Extra,
        }
    }

    /// Additional by-name filter, on top of `level()`.
    pub fn name_filter(self) -> Option<qlog::streamer::EventNameFilter> {
        match self {
            // Rejects quiche's own quic:*/http3:* events by wire name;
            // moq_core's moqt:* events aren't quiche-emitted so pass through.
            QlogLevelArg::Moqt => Some(Box::new(|name: &str| {
                !name.starts_with("quic:") && !name.starts_with("http3:")
            })),
            _ => None,
        }
    }
}
