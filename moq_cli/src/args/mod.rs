pub mod cc_algorithm;
pub mod qlog_level;

use crate::file_output::OutputKind;
use crate::fragment::{FragmentParams, reject_value, require_value};
use cc_algorithm::CcAlgorithm;
use qlog_level::QlogLevelArg;
use clap::Parser;
use moq_wire::control_message::subscribe::LocationFilter;
use moq_wire::{Namespace, NamespaceTrackname};
use quiche_moq::SubscribeOptions;
use std::path::PathBuf;
use std::str::FromStr;

/// Unified MoQ node: publish, subscribe, relay, and route tracks over MoQ transport.
///
/// Signals:
///   SIGUSR1/SIGUSR2  Runs the `SigAction`s assigned via `--sigusr1`/`--sigusr2`
///            (ping, toggle-padding-stream, unpub, migrate, race-probe).
///            No-op if none assigned. Two independent trigger groups —
///            Linux's limit of arbitrary-use signals without going real-time.
#[derive(Parser, Debug, Clone)]
#[command(name = "moq")]
pub struct Args {
    /// Listen for incoming QUIC connections on this port.
    /// - `#upstream`: fallback `--sub` source when no namespace announced yet;
    ///   announcements from here always accepted, no gating.
    /// - `#native`: native QUIC sessions too (moqt://, needs `--setup-version` 15+);
    ///   WebTransport sessions then start once the handshake completes, not in 0.5-RTT.
    #[arg(long, value_name = "PORT[#upstream&native]")]
    pub listen: Option<ListenArg>,

    /// Connect outbound to one or more MoQ nodes. Repeat for multiple targets.
    /// - `#upstream`: same as `--listen`'s; any number of connections may be marked.
    /// - an https URL runs the session on WebTransport, a moqt URL on native QUIC
    ///   (needs `--setup-version` 15+)
    ///
    /// Examples:
    ///   --connect https://relay.example.com:4433
    ///   --connect https://relay.example.com:4433#upstream
    ///   --connect moqt://relay.example.com:4433
    ///   --connect https://a.example.com --connect https://b.example.com#upstream
    #[arg(long, value_name = "URL[#upstream]")]
    pub connect: Vec<ConnectArg>,

    /// Re-announce (PUBLISH_NAMESPACE) every known namespace to all peers —
    /// own `--pub` namespaces plus any learned elsewhere — restricted to:
    ///   `<ns>--*`   — that namespace
    ///   `*`         — all namespaces
    ///
    /// Off by default. Only controls PUBLISH_NAMESPACE re-advertisement;
    /// relaying itself already works via plain `--sub`/`--pub`.
    ///
    /// Examples: --relay-namespace "*"
    ///           --relay-namespace "live--*"
    #[arg(long = "relay-namespace", value_name = "NS--*|*")]
    pub relay_namespace: Vec<TrackPattern>,

    /// Publish a track, announce a namespace, or register a passive standby
    /// track buffer served from an incoming PUBLISH. Operates purely on the
    /// local object cache — feed it from a file first with `--in`.
    ///
    /// Format:
    ///   <namespace--track>[#max_cache_duration=N&passive&datagram]
    ///     — concrete track: active PUBLISH of whatever's in the cache, or
    ///     (`&passive`) wait for SUBSCRIBE/PUBLISH instead. Doesn't announce
    ///     the namespace — pair with `--pub <NS>` for that.
    ///     max_cache_duration=N: MAX_CACHE_DURATION (draft-18 §12.3, ms) in
    ///     SUBSCRIBE_OK — relays must stop forwarding past that age.
    ///     datagram: each Object in a datagram (draft-18 §11.3, drafts 14+),
    ///     dropped if it doesn't fit one.
    ///   <namespace--*> / *
    ///     — auto-accept matching PUBLISH, dynamic per-track buffer, always
    ///     implicitly passive.
    ///   <namespace>
    ///     — PUBLISH_NAMESPACE announcement only.
    ///
    /// Examples: --pub tos--video
    ///           --pub tos--video#max_cache_duration=200
    ///           --pub tos--video#passive
    ///           --pub "tos--*"
    ///           --pub "*"
    ///           --pub tos
    #[arg(long = "pub", value_name = "NS--TRACK[#PARAMS]|NS--*|*|NS")]
    pub publish: Vec<PubArg>,

    /// Feed a concrete track's local object cache from a file, independent of
    /// whether it's also `--pub`lished. Default (no `#file=`): read stdin.
    ///
    /// Format: <namespace--track>[#file=FILE&delimiter=BYTE]
    /// File format (fMP4 or line-delimited) is auto-detected.
    ///
    /// Examples: --in tos--video#file=/tmp/video.mp4
    ///           --in tos--video
    #[arg(long = "in", value_name = "NS--TRACK[#file=FILE&delimiter=BYTE]")]
    pub r#in: Vec<InArg>,

    /// Subscribe to a track, or register a passive standby that accepts an
    /// incoming PUBLISH instead. Operates purely on the local object cache —
    /// drain it to a file with `--out`.
    ///
    /// Candidate connection for a concrete track: any connection announcing
    /// its namespace, or marked `#upstream` — a plain connection with
    /// neither is not a candidate. Default: one candidate (an announcer before
    /// an `#upstream`, earliest first; sticky — losing it doesn't trigger a
    /// search for a replacement).
    /// `&multi`: subscribe via every known candidate at once, merged into
    /// one track buffer (multipath).
    ///
    /// Format: <namespace--track>[#priority=N&delivery_timeout=N&rendezvous_timeout=N&filter=F&passive&multi&race[=M]]
    /// Parameters:
    ///   priority=N          SUBSCRIBE subscriber_priority (0=highest,
    ///                       255=lowest, default 128)
    ///   delivery_timeout=N  SUBSCRIBE delivery_timeout, ms
    ///   rendezvous_timeout=N  how long a relay may hold the SUBSCRIBE for a
    ///                       publisher, ms (draft 17 on)
    ///   filter=F            next-group-start (default) or largest-object
    ///   passive             wait for PUBLISH instead of sending SUBSCRIBE
    ///   multi               subscribe via every known candidate
    ///   race[=M]            one leader forwards, the others stand by; a
    ///                       `race-probe` signal races them: all forward
    ///                       (default), or M = keep/switch probe instead
    ///
    /// <namespace--*> / `*` also accepted — dynamic per-track buffer,
    /// implicitly passive, no parameters (including `&multi`; own separate
    /// single-candidate selection).
    ///
    /// Examples: --sub tos--video
    ///           --sub tos--video#priority=0
    ///           --sub tos--video#priority=0&delivery_timeout=5000
    ///           --sub tos--video#filter=largest-object
    ///           --sub tos--video#passive
    ///           --sub tos--video#multi
    ///           --sub tos--video#race=switch
    ///           --sub "tos--*"
    ///           --sub "*"
    #[arg(long = "sub", value_name = "NS--TRACK[#PARAMS]|NS--*|*")]
    pub subscribe: Vec<SubArg>,

    /// Drain a concrete track's local object cache to a file, independent of
    /// whether it's also `--sub`scribed. Default (no `#file=`): write stdout.
    ///
    /// Format: <namespace--track>[#file=FILE&format=F&delimiter=BYTE]
    /// format: auto (default, detected from file extension), raw, fmp4, lines.
    ///
    /// Examples: --out tos--video#file=/tmp/out.mp4
    ///           --out tos--video
    #[arg(long = "out", value_name = "NS--TRACK[#file=FILE&format=F&delimiter=BYTE]")]
    pub out: Vec<OutArg>,

    /// Maximum number of cached objects per track.
    #[arg(long, default_value_t = 16)]
    pub cache_depth: usize,

    /// Maximum object payload size in bytes.
    #[arg(long, default_value_t = 1_048_576)]
    pub cache_max_obj: usize,

    /// Expected average object payload size in bytes.
    #[arg(long, default_value_t = 65_536)]
    pub cache_avg_obj: usize,

    /// QUIC idle timeout in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub timeout: u64,

    /// Drop packets of accepted connections from a new 4-tuple (no migration,
    /// no NAT rebinding).
    #[arg(long, default_value_t = false)]
    pub no_migration: bool,

    /// Reconnect closed `#upstream` --connect connections, resuming concrete
    /// `--sub` tracks at the first object not yet fully received.
    #[arg(long, default_value_t = false)]
    pub reconnect: bool,

    /// Send keepalive ping on outbound connections (--connect) every
    /// N milliseconds (0 = disabled).
    #[arg(long, default_value_t = 0)]
    pub keepalive: u64,

    /// Disable the PTO floor for keepalive pings: send pings at exactly the
    /// configured interval regardless of the current PTO estimate.
    #[arg(long = "keepalive-no-pto-limit", default_value_t = false)]
    pub keepalive_no_pto_limit: bool,

    /// TLS certificate file (PEM) for --listen. If omitted a self-signed
    /// certificate is generated and its SPKI fingerprint is logged.
    #[arg(long, value_name = "FILE", requires = "key")]
    pub cert: Option<PathBuf>,

    /// TLS private key file (PEM) for --listen. Must be paired with --cert.
    #[arg(long, value_name = "FILE", requires = "cert")]
    pub key: Option<PathBuf>,

    /// Write TLS secrets for outbound connections (--connect) to
    /// this file in NSS key log format, for decryption in Wireshark etc.
    /// Does not apply to connections accepted via --listen.
    #[arg(env = "SSLKEYLOGFILE", long)]
    pub ssl_key_log_file: Option<PathBuf>,

    /// Write a qlog file per QUIC connection to this directory. Usually set
    /// via the QLOGDIR environment variable, the flag is hidden.
    #[arg(long = "qlog-dir", env = "QLOGDIR", hide = true)]
    pub qlog_dir: Option<PathBuf>,

    /// qlog verbosity: which native `quic:*` events quiche writes.
    /// - `core`: packet_sent/received, recovery_metrics_updated, packet_lost only
    /// - `base` (default): core + `stream_data_moved` (high volume, dominates CPU at high rates)
    /// - `extra`: everything
    /// - `moqt`: no quiche events, only moq_core's own `moqt:*`
    ///
    /// Doesn't affect moq_core's `moqt:*` events, which are unconditional.
    #[arg(long = "qlog-level", default_value = "base")]
    pub qlog_level: QlogLevelArg,

    /// Pin the MoQ-transport draft version used during SETUP to exactly this
    /// one, e.g. "07" or "draft16" — no negotiation, the handshake fails if
    /// the peer doesn't also support it. Applies to both outbound
    /// (--connect) and accepted (--listen) connections. For negotiation
    /// between several versions, use --supported-versions instead.
    #[arg(long = "setup-version", conflicts_with = "supported_versions")]
    pub setup_version: Option<MoqVersionArg>,

    /// Comma-separated list of MoQ-transport draft versions to offer/accept
    /// during SETUP negotiation, e.g. "12,13,14" or "draft12,draft13,draft14".
    /// The peer's SETUP negotiation picks the highest-priority version present
    /// in both sides' lists. Drafts 15 and later are negotiated outside SETUP,
    /// so they can only be given alone. If omitted: the default set of drafts
    /// before 15.
    #[arg(long = "supported-versions", value_delimiter = ',')]
    pub supported_versions: Option<Vec<MoqVersionArg>>,

    /// Embed the Unix-microsecond publish time on each object published from
    /// file input: as LOC Timestamp for plain text, as object property 0xA000
    /// for fMP4 (whose LOC Timestamp is the decode time).
    #[arg(long = "timestamp", default_value_t = false)]
    pub timestamp: bool,

    /// Override the initial QUIC congestion window, in packets, for BOTH
    /// accepted (--listen) and outbound (--connect) connections.
    /// Mutually exclusive with --listen-init-cwnd / --connect-init-cwnd.
    /// Mainly useful for testing SUBSCRIBE subscriber_priority's effect on
    /// stream scheduling under bandwidth contention; leave unset to use
    /// quiche's default.
    #[arg(long = "init-cwnd", conflicts_with_all = ["listen_init_cwnd", "connect_init_cwnd"])]
    pub init_cwnd: Option<usize>,

    /// Like --init-cwnd, but only for accepted (--listen) connections.
    #[arg(long = "listen-init-cwnd")]
    pub listen_init_cwnd: Option<usize>,

    /// Like --init-cwnd, but only for outbound (--connect) connections.
    #[arg(long = "connect-init-cwnd")]
    pub connect_init_cwnd: Option<usize>,

    /// Cap the QUIC pacing rate, in bytes/sec, for BOTH accepted (--listen)
    /// and outbound (--connect) connections.
    /// - mutually exclusive with --listen-max-pacing-rate / --connect-max-pacing-rate
    /// - quiche paces to whatever cwnd/RTT implies; cwnd's slow-start growth (incl.
    ///   after a migration, see --init-cwnd) can exceed real link capacity before
    ///   loss-based CC would notice — this caps the paced rate directly
    /// - unset: quiche's default (unlimited)
    #[arg(long = "max-pacing-rate", conflicts_with_all = ["listen_max_pacing_rate", "connect_max_pacing_rate"])]
    pub max_pacing_rate: Option<u64>,

    /// Like --max-pacing-rate, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-pacing-rate")]
    pub listen_max_pacing_rate: Option<u64>,

    /// Like --max-pacing-rate, but only for outbound (--connect) connections.
    #[arg(long = "connect-max-pacing-rate")]
    pub connect_max_pacing_rate: Option<u64>,

    /// Override the QUIC congestion control algorithm for BOTH accepted
    /// (--listen) and outbound (--connect) connections.
    /// Mutually exclusive with --listen-cc-algorithm / --connect-cc-algorithm.
    /// Leave unset to use quiche's default (CUBIC).
    #[arg(long = "cc-algorithm", value_enum, conflicts_with_all = ["listen_cc_algorithm", "connect_cc_algorithm"])]
    pub cc_algorithm: Option<CcAlgorithm>,

    /// Like --cc-algorithm, but only for accepted (--listen) connections.
    #[arg(long = "listen-cc-algorithm", value_enum)]
    pub listen_cc_algorithm: Option<CcAlgorithm>,

    /// Like --cc-algorithm, but only for outbound (--connect) connections.
    #[arg(long = "connect-cc-algorithm", value_enum)]
    pub connect_cc_algorithm: Option<CcAlgorithm>,

    /// Override the connection-level initial max data (QUIC flow control), in
    /// bytes, for BOTH accepted and outbound connections. Mutually exclusive
    /// with --listen-max-data / --connect-max-data. Leave unset to use
    /// quiche_moq_webtransport_helper's default (10,000,000).
    #[arg(long = "max-data", conflicts_with_all = ["listen_max_data", "connect_max_data"])]
    pub max_data: Option<u64>,

    /// Like --max-data, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-data")]
    pub listen_max_data: Option<u64>,

    /// Like --max-data, but only for outbound (--connect) connections.
    #[arg(long = "connect-max-data")]
    pub connect_max_data: Option<u64>,

    /// Override the per-stream initial max data (QUIC flow control), in
    /// bytes, for BOTH accepted and outbound connections (applies uniformly
    /// to bidirectional and unidirectional streams). Mutually exclusive with
    /// --listen-max-stream-data / --connect-max-stream-data. Leave unset to
    /// use quiche_moq_webtransport_helper's default (1,000,000).
    #[arg(long = "max-stream-data", conflicts_with_all = ["listen_max_stream_data", "connect_max_stream_data"])]
    pub max_stream_data: Option<u64>,

    /// Like --max-stream-data, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-stream-data")]
    pub listen_max_stream_data: Option<u64>,

    /// Like --max-stream-data, but only for outbound (--connect) connections.
    #[arg(long = "connect-max-stream-data")]
    pub connect_max_stream_data: Option<u64>,

    /// Override the initial unidirectional stream count limit (QUIC
    /// MAX_STREAMS), for BOTH accepted and outbound connections. Mutually
    /// exclusive with --listen-max-streams-uni / --connect-max-streams-uni.
    /// Leave unset to use quiche_moq_webtransport_helper's default (100).
    ///
    /// This node opens one new unidirectional stream per published object
    /// (one per MoQT group/subgroup), so at any sustained publish rate the
    /// default of 100 is exhausted in seconds. MAX_STREAMS credit is only
    /// replenished as the peer fully retires (reads to FIN and garbage
    /// collects) streams it has already received; once usage catches up to
    /// the granted limit there's nothing left to retire to trigger the next
    /// grant, and the sender has nothing new to send without credit it
    /// doesn't have -- a permanent standstill with no crash and no log
    /// output to explain it. Set this comfortably above the expected number
    /// of concurrently in-flight objects for sustained throughput.
    #[arg(long = "max-streams-uni", conflicts_with_all = ["listen_max_streams_uni", "connect_max_streams_uni"])]
    pub max_streams_uni: Option<u64>,

    /// Like --max-streams-uni, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-streams-uni")]
    pub listen_max_streams_uni: Option<u64>,

    /// Like --max-streams-uni, but only for outbound (--connect) connections.
    #[arg(long = "connect-max-streams-uni")]
    pub connect_max_streams_uni: Option<u64>,

    /// Override active_connection_id_limit, for BOTH accepted and outbound
    /// connections. Also quiche's path limit (default 2); each migration uses
    /// one up for good, so raise it for repeated migrations.
    #[arg(long = "active-conn-id-limit")]
    pub active_conn_id_limit: Option<u64>,

    /// Upper limit for the QUIC congestion window, in packets, for BOTH
    /// accepted and outbound connections. Also caps --init-cwnd. Keeps the
    /// window from overshooting a path of known capacity.
    #[arg(long = "max-cwnd")]
    pub max_cwnd: Option<usize>,

    /// Override the initial bidirectional stream count limit (QUIC
    /// MAX_STREAMS), for BOTH accepted and outbound connections. Mutually
    /// exclusive with --listen-max-streams-bidi / --connect-max-streams-bidi.
    /// Leave unset to use quiche_moq_webtransport_helper's default (100).
    #[arg(long = "max-streams-bidi", conflicts_with_all = ["listen_max_streams_bidi", "connect_max_streams_bidi"])]
    pub max_streams_bidi: Option<u64>,

    /// Like --max-streams-bidi, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-streams-bidi")]
    pub listen_max_streams_bidi: Option<u64>,

    /// Like --max-streams-bidi, but only for outbound (--connect) connections.
    #[arg(long = "connect-max-streams-bidi")]
    pub connect_max_streams_bidi: Option<u64>,

    /// MAX_REQUEST_ID granted to peers in SETUP (default 100). Never grown
    /// mid-session, so size it for all expected requests.
    #[arg(long)]
    pub max_request_id: Option<u64>,

    /// Create a low priority MoQ padding stream to reserve bandwidth
    #[arg(long)]
    pub padding_stream: bool,

    /// Cap the padding stream at this many bytes/sec; set below the path's rate.
    #[arg(long, value_name = "BYTES_PER_SEC")]
    pub padding_stream_rate: Option<u64>,

    /// Actions to run on SIGUSR1 — repeat or comma-separate for more than
    /// one (e.g. `--sigusr1 ping,race-probe`). See `SigAction` for the list.
    /// Split actions between --sigusr1/--sigusr2 to trigger them independently
    /// (Linux has no SIGUSR3+).
    #[arg(long, value_delimiter = ',')]
    pub sigusr1: Vec<SigAction>,

    /// Same as --sigusr1, for SIGUSR2.
    #[arg(long, value_delimiter = ',')]
    pub sigusr2: Vec<SigAction>,

    /// With the `toggle-padding-stream` action, only toggle the padding
    /// stream on --connect connections, not --listen ones.
    #[arg(long)]
    pub padding_stream_connect_only: bool,

    /// Relaying, keep upstream subscriptions delivering while every subscriber
    /// of the track is at Forward State 0: a standby switching to Forward 1
    /// finds the cache warm. Without it upstream is paused too, saving the
    /// traffic (draft-ietf-moq-transport-19 §9.2).
    #[arg(long)]
    pub prewarm: bool,

    /// Namespace to withdraw (PUBLISH_NAMESPACE_DONE) when the `unpub`
    /// action fires, on every connection that actually announced it via
    /// `--pub <NS>`. Repeatable.
    #[arg(long = "unpub", value_name = "NS")]
    pub unpub: Vec<Namespace>,
}

/// One thing a SIGUSR1/SIGUSR2 signal (see `--sigusr1`/`--sigusr2`) can
/// trigger. Multiple actions can share a signal; the same action can also
/// be assigned to both signals.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SigAction {
    /// Send an immediate QUIC PING on all paths of every connection —
    /// useful for triggering NAT rebind detection without waiting for the
    /// next --keepalive interval.
    Ping,
    /// Start the padding stream on every connection that doesn't have one
    /// running, and stop it on every connection that does (see
    /// --padding-stream-connect-only).
    TogglePaddingStream,
    /// Withdraw every --unpub NS on every connection that actually
    /// announced it via --pub.
    Unpub,
    /// Actively migrate every outbound (--connect) connection
    /// to a freshly bound local UDP port, keeping the same peer address
    /// (quiche's `migrate_source`). Simulates a NAT rebind / local
    /// interface change for testing real QUIC connection migration without
    /// an actual network-level change. No effect on accepted (--listen)
    /// connections, since only a client may initiate migration.
    Migrate,
    /// Trigger every `#race` track's (see `--sub`) leader election — see
    /// `crate::race::trigger_race`.
    RaceProbe,
}

impl Args {
    /// Checks constraints between values that clap cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(versions) = &self.supported_versions
            && versions.len() > 1
            && versions.iter().any(|v| v.0 >= moq_wire::MOQ_VERSION_DRAFT_15)
        {
            return Err("--supported-versions: drafts 15 and later can only be given alone".into());
        }
        Ok(())
    }

    /// `--listen-*` transport settings, falling back to the global ones
    /// (clap allows at most one of each pair).
    pub fn listen_quic(&self) -> QuicSettings {
        QuicSettings {
            init_cwnd: self.listen_init_cwnd.or(self.init_cwnd),
            max_pacing_rate: self.listen_max_pacing_rate.or(self.max_pacing_rate),
            cc_algorithm: self.listen_cc_algorithm.or(self.cc_algorithm),
            max_data: self.listen_max_data.or(self.max_data),
            max_stream_data: self.listen_max_stream_data.or(self.max_stream_data),
            max_streams_uni: self.listen_max_streams_uni.or(self.max_streams_uni),
            max_streams_bidi: self.listen_max_streams_bidi.or(self.max_streams_bidi),
        }
    }

    /// `--connect-*` transport settings, falling back to the global ones.
    pub fn connect_quic(&self) -> QuicSettings {
        QuicSettings {
            init_cwnd: self.connect_init_cwnd.or(self.init_cwnd),
            max_pacing_rate: self.connect_max_pacing_rate.or(self.max_pacing_rate),
            cc_algorithm: self.connect_cc_algorithm.or(self.cc_algorithm),
            max_data: self.connect_max_data.or(self.max_data),
            max_stream_data: self.connect_max_stream_data.or(self.max_stream_data),
            max_streams_uni: self.connect_max_streams_uni.or(self.max_streams_uni),
            max_streams_bidi: self.connect_max_streams_bidi.or(self.max_streams_bidi),
        }
    }
}

/// Transport settings of one side, `--listen-*` or `--connect-*`.
#[derive(Debug, PartialEq)]
pub struct QuicSettings {
    pub init_cwnd: Option<usize>,
    pub max_pacing_rate: Option<u64>,
    pub cc_algorithm: Option<CcAlgorithm>,
    pub max_data: Option<u64>,
    pub max_stream_data: Option<u64>,
    pub max_streams_uni: Option<u64>,
    pub max_streams_bidi: Option<u64>,
}

impl Default for Args {
    /// Matches clap's own parsing of zero arguments — a single source of
    /// truth instead of duplicating every `default_value_t` by hand. Forces
    /// `ssl_key_log_file`/`qlog_dir` to `None` even though clap would
    /// otherwise read them from `SSLKEYLOGFILE`/`QLOGDIR`, so a developer's
    /// local shell environment can't silently change `Args::default()` (used
    /// by tests as a base to override only the fields they care about).
    fn default() -> Self {
        let mut args = Args::try_parse_from(["moq"]).unwrap();
        args.ssl_key_log_file = None;
        args.qlog_dir = None;
        args
    }
}

pub fn parse_delimiter(s: &str) -> Result<u8, String> {
    match s {
        "\\n" => Ok(b'\n'),
        "\\t" => Ok(b'\t'),
        "\\r" => Ok(b'\r'),
        "\\0" => Ok(b'\0'),
        _ if s.len() == 1 => Ok(s.as_bytes()[0]),
        _ => Err(format!("delimiter must be a single byte or one of \\n, \\t, \\r, \\0, got {s:?}")),
    }
}

/// `--setup-version` argument: a MoQ-transport draft version number, parsed
/// from "NN" or "draftNN" (e.g. "16" or "draft16") into the wire `Version`.
#[derive(Debug, Clone, Copy)]
pub struct MoqVersionArg(pub moq_wire::Version);

impl FromStr for MoqVersionArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(MoqVersionArg)
    }
}

/// A track pattern: a concrete `namespace--track`, `namespace--*` matching
/// any track within that namespace, or bare `*` matching any namespace--track
/// at all. The wildcard forms don't carry a file — there's no fixed
/// source/sink, only a policy of dynamically creating a track buffer per
/// matched track as it's seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackPattern {
    Exact(NamespaceTrackname),
    NamespaceWildcard(Namespace),
    Wildcard,
}

impl TrackPattern {
    pub fn as_exact(&self) -> Option<&NamespaceTrackname> {
        match self {
            TrackPattern::Exact(nt) => Some(nt),
            _ => None,
        }
    }

    pub fn is_wildcard(&self) -> bool {
        !matches!(self, TrackPattern::Exact(_))
    }

    /// Whether `nt` falls under this pattern.
    pub fn matches(&self, nt: &NamespaceTrackname) -> bool {
        match self {
            TrackPattern::Exact(pat) => pat == nt,
            TrackPattern::NamespaceWildcard(ns) => nt.namespace() == ns,
            TrackPattern::Wildcard => true,
        }
    }

    /// Whether `ns` falls under this pattern (for namespace-only matching,
    /// e.g. filtering incoming PUBLISH_NAMESPACE announcements).
    pub fn matches_namespace(&self, ns: &Namespace) -> bool {
        match self {
            TrackPattern::Exact(nt) => nt.namespace() == ns,
            TrackPattern::NamespaceWildcard(p) => p == ns,
            TrackPattern::Wildcard => true,
        }
    }
}

impl FromStr for TrackPattern {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "*" {
            return Ok(TrackPattern::Wildcard);
        }
        if let Some((ns, "*")) = s.rsplit_once("--") {
            return Ok(TrackPattern::NamespaceWildcard(ns.parse()?));
        }
        Ok(TrackPattern::Exact(s.parse()?))
    }
}

/// `--pub` argument: publish a track (PUBLISH 0x1D), announce a namespace, or
/// (with `#passive`, or implicitly for a wildcard pattern) register a
/// standby track buffer served on incoming SUBSCRIBE / accepted from an
/// incoming PUBLISH instead of sent proactively. Reads from the local object
/// cache only — pair with `--in` to feed it from a file.
#[derive(Debug, Clone)]
pub struct PubArg {
    /// MAX_CACHE_DURATION (MoQT draft-18 §12.3) to advertise for this track,
    /// in milliseconds. Only meaningful for a concrete Track.
    pub max_cache_duration: Option<u64>,
    /// Wait to be asked (SUBSCRIBE) rather than actively PUBLISH. Always
    /// `true` for a wildcard Track pattern — there's no such thing as
    /// actively pushing to a pattern.
    pub passive: bool,
    /// Sends its Objects in datagrams (Object Forwarding Preference Datagram, draft-ietf-moq-transport-18 §11.3).
    pub datagram: bool,
    pub target: PubTarget,
}

#[derive(Debug, Clone)]
pub enum PubTarget {
    /// Concrete or wildcard track pattern. A wildcard dynamically accepts
    /// any incoming PUBLISH matching it, creating a track buffer per match.
    Track(TrackPattern),
    Namespace(Namespace),
}

impl PubArg {
    pub fn as_track(&self) -> Option<&TrackPattern> {
        match &self.target {
            PubTarget::Track(pattern) => Some(pattern),
            PubTarget::Namespace(_) => None,
        }
    }

    pub fn as_namespace(&self) -> Option<&Namespace> {
        match &self.target {
            PubTarget::Namespace(ns) => Some(ns),
            PubTarget::Track(_) => None,
        }
    }
}

/// An outbound connection target parsed from a `--connect` value.
/// The URL fragment (`#upstream`) designates relay-forwarding semantics;
/// the fragment is stripped before the URL is used to open the connection.
#[derive(Debug, Clone)]
pub struct ConnectArg {
    /// URL without the fragment, used to open the QUIC/WebTransport connection.
    pub url: String,
    /// Whether this connection is the upstream relay for subscription forwarding.
    pub upstream: bool,
}

impl FromStr for ConnectArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (url, params) = FragmentParams::split(s);
        let mut upstream = false;
        for (key, value) in params.iter() {
            match key {
                "upstream" => { reject_value(key, value)?; upstream = true; }
                _ => return Err(format!("unknown parameter `{key}` (expected: upstream)")),
            }
        }
        Ok(ConnectArg { url: url.to_owned(), upstream })
    }
}

/// A `--listen` argument: the port to accept QUIC connections on, and
/// whether every connection accepted there is trusted as upstream (see
/// `ConnectArg::upstream`).
#[derive(Debug, Clone)]
pub struct ListenArg {
    pub port: u16,
    pub upstream: bool,
    /// Accepts native QUIC sessions too (draft-ietf-moq-transport-18 §3.1.4). WebTransport sessions wait
    /// for the handshake to complete then, the server can only tell them apart by the ALPN.
    pub native: bool,
}

impl From<u16> for ListenArg {
    fn from(port: u16) -> Self {
        ListenArg { port, upstream: false, native: false }
    }
}

impl FromStr for ListenArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (port, params) = FragmentParams::split(s);
        let port = port.parse::<u16>().map_err(|_| format!("invalid port {port:?}"))?;
        let mut upstream = false;
        let mut native = false;
        for (key, value) in params.iter() {
            match key {
                "upstream" => { reject_value(key, value)?; upstream = true; }
                "native" => { reject_value(key, value)?; native = true; }
                _ => return Err(format!("unknown parameter `{key}` (expected: upstream, native)")),
            }
        }
        Ok(ListenArg { port, upstream, native })
    }
}

impl FromStr for PubArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (rest, params) = FragmentParams::split(s);
        let mut max_cache_duration = None;
        let mut passive = false;
        let mut datagram = false;
        for (key, value) in params.iter() {
            match key {
                "max_cache_duration" => {
                    max_cache_duration = Some(require_value(key, value)?.parse::<u64>()
                        .map_err(|_| format!("invalid max_cache_duration {value:?}: expected a non-negative integer"))?);
                }
                "passive" => { reject_value(key, value)?; passive = true; }
                "datagram" => { reject_value(key, value)?; datagram = true; }
                _ => return Err(format!("unknown parameter `{key}` (expected: max_cache_duration, passive, datagram)")),
            }
        }
        // If `rest` contains `--`, it's a namespace--track (or pattern); otherwise just a namespace.
        if rest.contains("--") || rest == "*" {
            let pattern: TrackPattern = rest.parse()?;
            if pattern.is_wildcard() {
                if max_cache_duration.is_some() || datagram {
                    return Err("#max_cache_duration= and &datagram require a concrete namespace--track, not a wildcard pattern".into());
                }
                passive = true;
            }
            Ok(Self { max_cache_duration, passive, datagram, target: PubTarget::Track(pattern) })
        } else {
            if passive || datagram {
                return Err("&passive and &datagram have no effect on a bare namespace".into());
            }
            if max_cache_duration.is_some() {
                return Err("#max_cache_duration= has no effect on a bare namespace".into());
            }
            Ok(Self { max_cache_duration, passive, datagram, target: PubTarget::Namespace(rest.parse()?) })
        }
    }
}

/// `--in` argument: feeds a concrete track's local object cache from a file.
/// Parsed from `<namespace--track>[#file=FILE&delimiter=BYTE]`. No wildcard
/// form — a dynamic set of tracks can't share one fixed file source.
#[derive(Debug, Clone)]
pub struct InArg {
    pub track: NamespaceTrackname,
    /// `None` means read stdin — `--in`'s only purpose is a file/stdin feed.
    pub file: Option<PathBuf>,
    /// Byte delimiter for line-delimited input. Default `\n`.
    pub delimiter: u8,
}

impl FromStr for InArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (head, params) = FragmentParams::split(s);
        let mut file = None;
        let mut delimiter = b'\n';
        for (key, value) in params.iter() {
            match key {
                "file" => file = Some(PathBuf::from(require_value(key, value)?)),
                "delimiter" => delimiter = parse_delimiter(require_value(key, value)?)?,
                _ => return Err(format!("unknown parameter `{key}` (expected: file, delimiter)")),
            }
        }
        Ok(Self { track: head.parse()?, file, delimiter })
    }
}

/// A subscribe argument: SUBSCRIBE options (`priority`, `delivery_timeout`,
/// `filter`) for a track, populating its local object cache. Parsed from
/// `<pattern>[#option=value&...]`, where `pattern` is a concrete
/// `namespace--track`, a `namespace--*` namespace wildcard, or bare `*`
/// matching anything (wildcards are always implicitly `passive`).
#[derive(Debug, Clone)]
pub struct SubArg {
    pub track: TrackPattern,
    /// SUBSCRIBE options, see [`SubArg::default_options`].
    pub options: SubscribeOptions,
    /// Wait to be asked (accept an incoming PUBLISH) rather than sending
    /// SUBSCRIBE immediately. Always `true` for a wildcard pattern.
    pub passive: bool,
    /// Subscribe via every currently-known candidate connection (namespace
    /// announcers and `#upstream` connections) instead of just one. Only
    /// meaningful for a concrete pattern.
    pub multi: bool,
    /// Subscribe via one candidate at a time (the leader), with the others
    /// kept at Forward=0. A `race-probe` signal starts an election, decided
    /// by `race_mode`. Only meaningful for a concrete pattern.
    pub race: bool,
    /// `#race` (`Data`), `#race=keep` or `#race=switch`. Only used with `race`.
    pub race_mode: RaceMode,
}

/// How a `#race` election (`SigAction::RaceProbe`) is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RaceMode {
    /// All candidates forward; first to deliver/confirm the next object wins.
    /// - duplicates load while racing
    #[default]
    Data,
    /// Leader keeps forwarding, standbys probed with Forward=0.
    /// - first caught-up REQUEST_UPDATE_OK wins
    Keep,
    /// A standby forwards at once, leader paused; both probed.
    /// - first caught-up REQUEST_UPDATE_OK wins
    Switch,
}

/// Parses the `filter=` SUBSCRIBE option. Only the two location-less filter
/// types are accepted, the CLI has no syntax for a start/end Location.
fn parse_filter(value: &str) -> Result<LocationFilter, String> {
    match value {
        "next-group-start" | "next-group" => Ok(LocationFilter::NextGroupStart),
        "largest-object" | "largest" => Ok(LocationFilter::LargestObject),
        _ => Err(format!("invalid filter {value:?}: expected next-group-start or largest-object")),
    }
}

impl FromStr for SubArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (rest, params) = FragmentParams::split(s);
        let mut options = Self::default_options();
        let mut options_given = false;
        let mut passive = false;
        let mut multi = false;
        let mut race = false;
        let mut race_mode = RaceMode::default();
        for (key, value) in params.iter() {
            match key {
                "priority" => {
                    let v = require_value(key, value)?;
                    options_given = true;
                    options.priority = v.parse::<u8>()
                        .map_err(|_| format!("invalid priority {v:?}: expected an integer 0-255"))?;
                }
                "delivery_timeout" => {
                    let v = require_value(key, value)?;
                    options_given = true;
                    options.delivery_timeout = Some(v.parse::<u64>()
                        .map_err(|_| format!("invalid delivery_timeout {v:?}: expected a non-negative integer"))?);
                }
                "rendezvous_timeout" => {
                    let v = require_value(key, value)?;
                    options_given = true;
                    options.rendezvous_timeout = Some(v.parse::<u64>()
                        .map_err(|_| format!("invalid rendezvous_timeout {v:?}: expected a non-negative integer"))?);
                }
                "filter" => {
                    options_given = true;
                    options.filter = parse_filter(require_value(key, value)?)?;
                }
                "passive" => { reject_value(key, value)?; passive = true; }
                "multi" => { reject_value(key, value)?; multi = true; }
                "race" => {
                    race = true;
                    race_mode = match value {
                        None => RaceMode::Data,
                        Some("keep") => RaceMode::Keep,
                        Some("switch") => RaceMode::Switch,
                        Some(v) => return Err(format!("invalid race mode {v:?}: expected keep or switch (or no value)")),
                    };
                }
                _ => return Err(format!("unknown parameter `{key}` (expected: priority, delivery_timeout, rendezvous_timeout, filter, passive, multi, race[=keep|switch])")),
            }
        }
        if multi && race {
            return Err("#multi and #race are mutually exclusive".into());
        }
        let track: TrackPattern = rest.parse()?;
        if track.is_wildcard() {
            if options_given || multi || race {
                return Err("wildcard patterns don't support #priority=/#delivery_timeout=/#rendezvous_timeout=/#filter=/#multi/#race".into());
            }
            passive = true;
        }
        Ok(Self { track, options, passive, multi, race, race_mode })
    }
}

impl SubArg {
    /// MoQ's defaults, but starting at the next group.
    pub fn default_options() -> SubscribeOptions {
        SubscribeOptions { filter: LocationFilter::NextGroupStart, ..SubscribeOptions::default() }
    }
}

/// `--out` argument: drains a concrete track's local object cache to a file.
/// Parsed from `<namespace--track>[#file=FILE&format=F&delimiter=BYTE]`. No
/// wildcard form — a dynamic set of tracks can't share one fixed file sink.
#[derive(Debug, Clone)]
pub struct OutArg {
    pub track: NamespaceTrackname,
    /// `None` means write stdout — `--out`'s only purpose is a file/stdout sink.
    pub file: Option<PathBuf>,
    /// Output format and delimiter. Default: `OutputKind::Auto { delimiter: b'\n' }`.
    pub output_kind: OutputKind,
}

impl FromStr for OutArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (head, params) = FragmentParams::split(s);
        let mut file = None;
        let mut format = "auto";
        let mut delimiter = b'\n';
        for (key, value) in params.iter() {
            match key {
                "file" => file = Some(PathBuf::from(require_value(key, value)?)),
                "format" => format = require_value(key, value)?,
                "delimiter" => delimiter = parse_delimiter(require_value(key, value)?)?,
                _ => return Err(format!("unknown parameter `{key}` (expected: file, format, delimiter)")),
            }
        }
        let output_kind = match format {
            "auto"  => OutputKind::auto(delimiter),
            "raw"   => OutputKind::raw(),
            "fmp4"  => OutputKind::fmp4(),
            "lines" => OutputKind::lines(delimiter),
            _ => return Err(format!("unknown format {format:?}: expected auto, raw, fmp4, lines")),
        };
        if matches!(output_kind, OutputKind::Raw | OutputKind::FMp4 { .. }) && delimiter != b'\n' {
            return Err(format!("delimiter has no effect with format={format:?}"));
        }
        Ok(Self { track: head.parse()?, file, output_kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pub_arg_passive() {
        let arg: PubArg = "tos--video#passive".parse().unwrap();
        assert_eq!(arg.as_track().unwrap().as_exact().unwrap().to_string(), "tos--video");
        assert!(arg.passive);
    }

    #[test]
    fn pub_arg_namespace_wildcard_implies_passive() {
        let arg: PubArg = "tos--*".parse().unwrap();
        let pattern = arg.as_track().unwrap();
        assert!(pattern.is_wildcard());
        assert!(pattern.as_exact().is_none());
        assert!(arg.passive);
    }

    #[test]
    fn pub_arg_wildcard_implies_passive() {
        let arg: PubArg = "*".parse().unwrap();
        let pattern = arg.as_track().unwrap();
        assert!(pattern.is_wildcard());
        assert!(arg.passive);
    }

    #[test]
    fn pub_arg_wildcard_rejects_max_cache_duration() {
        let result: Result<PubArg, _> = "tos--*#max_cache_duration=200".parse();
        assert!(result.is_err());
    }

    #[test]
    fn pub_arg_with_max_cache_duration() {
        let arg: PubArg = "tos--video#max_cache_duration=200".parse().unwrap();
        assert_eq!(arg.max_cache_duration, Some(200));
    }

    #[test]
    fn sub_arg_defaults() {
        let arg: SubArg = "tos--video".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.options, SubArg::default_options());
    }

    #[test]
    fn sub_arg_with_priority() {
        let arg: SubArg = "tos--video#priority=5".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.options.priority, 5);
    }

    #[test]
    fn sub_arg_with_delivery_timeout() {
        let arg: SubArg = "tos--video#delivery_timeout=5000".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.options.delivery_timeout, Some(5000));
        assert_eq!(arg.options.priority, moq_wire::DEFAULT_SUBSCRIBER_PRIORITY);
    }

    #[test]
    fn sub_arg_with_priority_and_delivery_timeout() {
        let arg: SubArg = "tos--video#priority=10&delivery_timeout=5000".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.options.priority, 10);
        assert_eq!(arg.options.delivery_timeout, Some(5000));
    }

    #[test]
    fn sub_arg_with_filter() {
        let largest: SubArg = "tos--video#filter=largest-object".parse().unwrap();
        assert_eq!(largest.options.filter, LocationFilter::LargestObject);
        let next: SubArg = "tos--video#filter=next-group".parse().unwrap();
        assert_eq!(next.options.filter, LocationFilter::NextGroupStart);
        let none: SubArg = "tos--video".parse().unwrap();
        assert_eq!(none.options.filter, LocationFilter::NextGroupStart);
    }

    #[test]
    fn sub_arg_with_multi() {
        let arg: SubArg = "tos--video#multi".parse().unwrap();
        assert!(arg.multi);
        assert!(!arg.passive);
        let none: SubArg = "tos--video".parse().unwrap();
        assert!(!none.multi);
    }

    #[test]
    fn sub_arg_wildcard_rejects_multi() {
        let result: Result<SubArg, _> = "tos--*#multi".parse();
        assert!(result.is_err());
        let result: Result<SubArg, _> = "*#multi".parse();
        assert!(result.is_err());
    }

    #[test]
    fn sub_arg_rejects_invalid_filter() {
        let result: Result<SubArg, _> = "tos--video#filter=bogus".parse();
        assert!(result.is_err());
    }

    #[test]
    fn sub_arg_rejects_unknown_option() {
        let result: Result<SubArg, _> = "tos--video#bogus=1".parse();
        assert!(result.is_err());
    }

    #[test]
    fn sub_arg_rejects_malformed_option() {
        let result: Result<SubArg, _> = "tos--video#priority".parse();
        assert!(result.is_err());
    }

    #[test]
    fn sub_arg_namespace_wildcard() {
        let arg: SubArg = "tos--*".parse().unwrap();
        assert!(arg.track.is_wildcard());
    }

    #[test]
    fn sub_arg_wildcard() {
        let arg: SubArg = "*".parse().unwrap();
        assert!(arg.track.is_wildcard());
        assert!(arg.passive);
    }

    #[test]
    fn sub_arg_wildcard_rejects_options() {
        let result: Result<SubArg, _> = "*#priority=5".parse();
        assert!(result.is_err());
        let result: Result<SubArg, _> = "tos--*#priority=5".parse();
        assert!(result.is_err());
    }

    #[test]
    fn pub_arg_multi_component_namespace() {
        let arg: PubArg = "org-example--audio".parse().unwrap();
        let pattern = arg.as_track().unwrap();
        assert_eq!(pattern.as_exact().unwrap().to_string(), "org-example--audio");
    }

    #[test]
    fn pub_arg_namespace_only() {
        let arg: PubArg = "tos".parse().unwrap();
        let ns = arg.as_namespace().unwrap();
        assert_eq!(ns.to_string(), "tos");
    }

    #[test]
    fn in_arg_with_file() {
        let arg: InArg = "tos--video#file=/tmp/video.mp4".parse().unwrap();
        assert_eq!(arg.track.to_string(), "tos--video");
        assert_eq!(arg.file.as_ref().unwrap().to_str().unwrap(), "/tmp/video.mp4");
    }

    #[test]
    fn in_arg_defaults_to_stdin() {
        let arg: InArg = "tos--video".parse().unwrap();
        assert!(arg.file.is_none());
        assert_eq!(arg.delimiter, b'\n');
    }

    #[test]
    fn in_arg_with_delimiter() {
        let arg: InArg = "tos--video#delimiter=,".parse().unwrap();
        assert_eq!(arg.delimiter, b',');
    }

    #[test]
    fn out_arg_with_file() {
        let arg: OutArg = "tos--video#file=/tmp/out.mp4".parse().unwrap();
        assert_eq!(arg.track.to_string(), "tos--video");
        assert_eq!(arg.file.as_ref().unwrap().to_str().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn out_arg_defaults_to_stdout() {
        let arg: OutArg = "tos--video".parse().unwrap();
        assert!(arg.file.is_none());
    }

    #[test]
    fn delimiter_parses_escapes() {
        assert_eq!(parse_delimiter("\\n").unwrap(), b'\n');
        assert_eq!(parse_delimiter("\\t").unwrap(), b'\t');
        assert_eq!(parse_delimiter("\\r").unwrap(), b'\r');
        assert_eq!(parse_delimiter("\\0").unwrap(), b'\0');
    }

    #[test]
    fn delimiter_parses_literal_byte() {
        assert_eq!(parse_delimiter(",").unwrap(), b',');
    }

    #[test]
    fn delimiter_rejects_multi_byte() {
        assert!(parse_delimiter("ab").is_err());
    }

    /// Each per-side getter falls back to the global flag when its own --listen-*/
    /// --connect-* override isn't set.
    #[test]
    fn quic_tuning_getters_fall_back_to_global() {
        let args = Args::try_parse_from(["moq", "--init-cwnd", "4", "--max-data", "1000", "--max-stream-data", "2000", "--cc-algorithm", "bbr"]).unwrap();
        assert_eq!(args.listen_quic().init_cwnd, Some(4));
        assert_eq!(args.connect_quic().init_cwnd, Some(4));
        assert_eq!(args.listen_quic().max_data, Some(1000));
        assert_eq!(args.connect_quic().max_data, Some(1000));
        assert_eq!(args.listen_quic().max_stream_data, Some(2000));
        assert_eq!(args.connect_quic().max_stream_data, Some(2000));
        assert_eq!(args.listen_quic().cc_algorithm, Some(CcAlgorithm::Bbr));
        assert_eq!(args.connect_quic().cc_algorithm, Some(CcAlgorithm::Bbr));
    }

    /// A --listen-*/--connect-* override only affects its own side.
    #[test]
    fn quic_tuning_getters_prefer_per_side_override() {
        let args = Args::try_parse_from(["moq", "--listen-init-cwnd", "4", "--connect-max-data", "1000", "--listen-cc-algorithm", "bbr"]).unwrap();
        assert_eq!(args.listen_quic().init_cwnd, Some(4));
        assert_eq!(args.connect_quic().init_cwnd, None);
        assert_eq!(args.listen_quic().max_data, None);
        assert_eq!(args.connect_quic().max_data, Some(1000));
        assert_eq!(args.listen_quic().cc_algorithm, Some(CcAlgorithm::Bbr));
        assert_eq!(args.connect_quic().cc_algorithm, None);
    }

    /// The global flag and its per-side overrides are mutually exclusive at parse
    /// time (clap's `conflicts_with_all`) — specifying both is a usage error, not
    /// silently resolved by precedence.
    #[test]
    fn quic_tuning_global_and_per_side_flags_conflict() {
        assert!(Args::try_parse_from(["moq", "--init-cwnd", "4", "--listen-init-cwnd", "3"]).is_err());
        assert!(Args::try_parse_from(["moq", "--max-data", "4", "--connect-max-data", "3"]).is_err());
        assert!(Args::try_parse_from(["moq", "--max-stream-data", "4", "--listen-max-stream-data", "3"]).is_err());
        assert!(Args::try_parse_from(["moq", "--cc-algorithm", "cubic", "--connect-cc-algorithm", "bbr"]).is_err());
        // Per-side flags on both sides at once are fine (not mutually exclusive with each other).
        assert!(Args::try_parse_from(["moq", "--listen-init-cwnd", "4", "--connect-init-cwnd", "3"]).is_ok());
    }

    #[test]
    fn supported_versions_parses_comma_separated_list() {
        let args = Args::try_parse_from(["moq", "--supported-versions", "12,13,14"]).unwrap();
        args.validate().unwrap();
        let versions = args.supported_versions.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].0, moq_wire::MOQ_VERSION_DRAFT_12);
        assert_eq!(versions[1].0, moq_wire::MOQ_VERSION_DRAFT_13);
        assert_eq!(versions[2].0, moq_wire::MOQ_VERSION_DRAFT_14);
    }

    #[test]
    fn supported_versions_rejects_new_draft_in_list() {
        let args = Args::try_parse_from(["moq", "--supported-versions", "13,15,16"]).unwrap();
        assert!(args.validate().is_err());
        let args = Args::try_parse_from(["moq", "--supported-versions", "16"]).unwrap();
        args.validate().unwrap();
    }

    #[test]
    fn setup_version_conflicts_with_supported_versions() {
        assert!(Args::try_parse_from(["moq", "--setup-version", "13", "--supported-versions", "12,13"]).is_err());
    }

    #[test]
    fn supported_versions_accepts_draft_prefixed_entries() {
        let args = Args::try_parse_from(["moq", "--supported-versions", "draft07,draft16"]).unwrap();
        let versions = args.supported_versions.unwrap();
        assert_eq!(versions.len(), 2);
    }

    #[test]
    fn supported_versions_defaults_to_none() {
        let args = Args::try_parse_from(["moq"]).unwrap();
        assert!(args.supported_versions.is_none());
    }
}
