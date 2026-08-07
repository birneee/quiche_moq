use crate::file_output::OutputKind;
use crate::fragment::{FragmentParams, reject_value, require_value};
use clap::Parser;
use moq_wire::control_message::subscribe::FilterType;
use moq_wire::{Namespace, NamespaceTrackname};
use std::path::PathBuf;
use std::str::FromStr;

/// Unified MoQ node: publish, subscribe, relay, and route tracks over MoQ transport.
///
/// Signals:
///   SIGUSR1  Acts per whichever of --usr1-ping / --usr1-toggle-padding-stream /
///            --usr1-unpub is set (any combination, or none — no-op if none
///            is set): --usr1-ping sends an immediate PING on all paths of
///            every connection, useful for triggering NAT rebind detection
///            without waiting for the next --keepalive interval.
///            --usr1-toggle-padding-stream starts the padding stream on
///            every connection that doesn't have one running, and stops it
///            on every connection that does. --usr1-unpub withdraws each of its namespaces on every
///            connection that actually announced it via --pub.
#[derive(Parser, Debug, Clone)]
#[command(name = "moq")]
pub struct Args {
    /// Listen for incoming QUIC connections on this port.
    #[arg(long)]
    pub listen: Option<u16>,

    /// Connect outbound to one or more MoQ nodes. Repeat for multiple targets.
    /// Exactly one connection may be marked `#upstream`: subscriptions for
    /// namespaces unknown locally are forwarded there and namespaces announced
    /// by it are made available to local subscribers.
    ///
    /// Examples:
    ///   --connect https://relay.example.com:4433
    ///   --connect https://relay.example.com:4433#upstream
    ///   --connect https://a.example.com --connect https://b.example.com#upstream
    #[arg(long, value_name = "URL[#upstream]")]
    pub connect: Vec<ConnectArg>,

    /// Act as a namespace discovery hub: re-announce (PUBLISH_NAMESPACE) every
    /// namespace this node knows — its own `--pub`/`--pub-passive` namespaces
    /// plus any learned from an `--upstream` peer — to all other connected peers,
    /// but only for namespaces whose namespace component matches one of these
    /// patterns. Accepts the same syntax as `--sub-passive`:
    ///   `<ns>`      — exact match
    ///   `<ns>--*`   — any track under that namespace prefix
    ///   `*`         — all namespaces
    ///
    /// Off by default; omit the flag to disable namespace relaying entirely.
    /// (Relaying traffic itself already works via `--sub-passive`/`--pub-passive`
    /// without this flag; this only controls PUBLISH_NAMESPACE re-advertisement.)
    ///
    /// Examples: --relay-namespace "*"
    ///           --relay-namespace live
    ///           --relay-namespace "live--*"
    #[arg(long = "relay-namespace", value_name = "NS|NS--*|*")]
    pub relay_namespace: Vec<TrackPattern>,

    /// Actively publish a track or announce a namespace to all connections.
    ///
    /// Format: <namespace--track>[#in=file]   — sends PUBLISH (0x1D) to each peer
    ///         <namespace>                     — sends PUBLISH_NAMESPACE announcement
    ///
    /// Examples: --pub tos--video#in=/tmp/video.mp4
    ///           --pub tos--video
    ///           --pub tos
    #[arg(long = "pub", value_name = "NS--TRACK[#in=FILE]|NS")]
    pub publish: Vec<PubArg>,

    /// Register a local standby track buffer served in response to SUBSCRIBE,
    /// or accepted from an incoming PUBLISH_NAMESPACE announcement.
    ///
    /// Format: <namespace--track>[#in=file]  — a concrete track, fed from a
    ///                                          file (fMP4 or line-delimited,
    ///                                          auto-detected; `-`/omitted
    ///                                          means stdin)
    ///         <namespace--*>                 — auto-accept any incoming
    ///                                          PUBLISH_NAMESPACE for this
    ///                                          namespace, creating a track
    ///                                          buffer dynamically per track
    ///         *                               — same, for any namespace
    ///
    /// Concrete entries do *not* announce the track's namespace — pair with
    /// `--pub <NS>` if a peer/relay needs to discover it via
    /// PUBLISH_NAMESPACE rather than subscribing to a namespace--track it
    /// already knows about.
    ///
    /// Examples: --pub-passive tos--video#in=/tmp/video.mp4
    ///           --pub-passive tos--video
    ///           --pub-passive "tos--*"
    ///           --pub-passive "*"
    #[arg(long = "pub-passive", value_name = "NS--TRACK[#in=FILE]|NS--*|*")]
    pub pub_passive: Vec<TrackArg>,

    /// Subscribe to a track and write to a file or stdout.
    /// Format: <namespace--track>[#out=file&option=value&...]
    /// Output format is auto-detected from received LoC extension headers.
    /// Omit file (or use `-`) to write to stdout.
    /// Supported parameters:
    ///   out=FILE            Destination file (stdout if omitted).
    ///   priority=N          SUBSCRIBE subscriber_priority (0 = highest,
    ///                       255 = lowest; MoQ default 128 if omitted).
    ///   delivery_timeout=N  SUBSCRIBE delivery_timeout in milliseconds.
    ///   filter=F            SUBSCRIBE filter: next-group-start (default) or
    ///                       largest-object.
    /// Examples: --sub tos--video#out=/tmp/video.mp4
    ///           --sub tos--video
    ///           --sub tos--video#priority=0
    ///           --sub tos--video#priority=0&delivery_timeout=5000
    ///           --sub tos--video#filter=largest-object
    #[arg(long = "sub", value_name = "NS--TRACK[#PARAMS]")]
    pub subscribe: Vec<SubArg>,

    /// Passively accept an incoming PUBLISH for a track buffer (does not send
    /// SUBSCRIBE). When the peer actively publishes a matching track it is
    /// accepted and written to a file or stdout, and/or relay-forwarded to
    /// any other peer subscribed to it.
    ///
    /// Format: <namespace--track>[#out=file&option=value&...]  — see --sub
    ///                                          for supported parameters
    ///         <namespace--*>                 — relay-forward any incoming
    ///                                          SUBSCRIBE for this namespace
    ///                                          to an `#upstream` peer,
    ///                                          creating a track buffer
    ///                                          dynamically per track
    ///         *                               — same, for any namespace
    ///
    /// Examples: --sub-passive tos--video
    ///           --sub-passive tos--video#out=/tmp/out.mp4
    ///           --sub-passive "tos--*"
    ///           --sub-passive "*"
    #[arg(long = "sub-passive", value_name = "NS--TRACK[#PARAMS]|NS--*|*")]
    pub sub_passive: Vec<SubArg>,

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

    /// Send keepalive ping on outbound connections (--connect / --upstream) every
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

    /// Write TLS secrets for outbound connections (--connect / --upstream) to
    /// this file in NSS key log format, for decryption in Wireshark etc.
    /// Does not apply to connections accepted via --listen.
    #[arg(env = "SSLKEYLOGFILE", long)]
    pub ssl_key_log_file: Option<PathBuf>,

    /// Write a qlog file per QUIC connection to this directory. Set only via
    /// the QLOGDIR environment variable (no corresponding flag) — node.rs
    /// reads this field, not the environment, when wiring up qlog.
    #[arg(long = "qlog-dir", env = "QLOGDIR", hide = true)]
    pub qlog_dir: Option<PathBuf>,

    /// Pin the MoQ-transport draft version used during SETUP to exactly this
    /// one, e.g. "07" or "draft16" — no negotiation, the handshake fails if
    /// the peer doesn't also support it. Applies to both outbound
    /// connections (--connect / --upstream, which version is offered) and
    /// accepted connections (--listen, which version is selected). For real
    /// multi-version negotiation instead of a hard pin, use
    /// --supported-versions; --setup-version still controls which version is
    /// *preferred* (and which dialect is used to encode/decode the SETUP
    /// message itself) but no longer forces it to be the only option.
    #[arg(long = "setup-version")]
    pub setup_version: Option<MoqVersionArg>,

    /// Comma-separated list of MoQ-transport draft versions to offer/accept
    /// during SETUP negotiation, e.g. "13,15,16" or "draft13,draft15,draft16".
    /// The peer's SETUP negotiation picks the highest-priority version present
    /// in both sides' lists. If omitted: defaults to just [--setup-version]
    /// when that's set (preserving a hard pin to one version), or to this
    /// binary's full default set otherwise.
    #[arg(long = "supported-versions", value_delimiter = ',')]
    pub supported_versions: Option<Vec<MoqVersionArg>>,

    /// Embed a Unix-microsecond timestamp KVP on each published line-delimited
    /// object (from --pub-passive on plain-text input). Has no effect on fMP4
    /// input, which already embeds a decode-time-derived timestamp.
    #[arg(long = "timestamp", default_value_t = false)]
    pub timestamp: bool,

    /// Override the initial QUIC congestion window, in packets, for BOTH
    /// accepted (--listen) and outbound (--connect / --upstream) connections.
    /// Mutually exclusive with --listen-init-cwnd / --connect-init-cwnd.
    /// Mainly useful for testing SUBSCRIBE subscriber_priority's effect on
    /// stream scheduling under bandwidth contention; leave unset to use
    /// quiche's default.
    #[arg(long = "init-cwnd", conflicts_with_all = ["listen_init_cwnd", "connect_init_cwnd"])]
    pub init_cwnd: Option<usize>,

    /// Like --init-cwnd, but only for accepted (--listen) connections.
    #[arg(long = "listen-init-cwnd")]
    pub listen_init_cwnd: Option<usize>,

    /// Like --init-cwnd, but only for outbound (--connect / --upstream) connections.
    #[arg(long = "connect-init-cwnd")]
    pub connect_init_cwnd: Option<usize>,

    /// Override the connection-level initial max data (QUIC flow control), in
    /// bytes, for BOTH accepted and outbound connections. Mutually exclusive
    /// with --listen-max-data / --connect-max-data. Leave unset to use
    /// quiche_moq_webtransport_helper's default (10,000,000).
    #[arg(long = "max-data", conflicts_with_all = ["listen_max_data", "connect_max_data"])]
    pub max_data: Option<u64>,

    /// Like --max-data, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-data")]
    pub listen_max_data: Option<u64>,

    /// Like --max-data, but only for outbound (--connect / --upstream) connections.
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

    /// Like --max-stream-data, but only for outbound (--connect / --upstream) connections.
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

    /// Like --max-streams-uni, but only for outbound (--connect / --upstream) connections.
    #[arg(long = "connect-max-streams-uni")]
    pub connect_max_streams_uni: Option<u64>,

    /// Override the initial bidirectional stream count limit (QUIC
    /// MAX_STREAMS), for BOTH accepted and outbound connections. Mutually
    /// exclusive with --listen-max-streams-bidi / --connect-max-streams-bidi.
    /// Leave unset to use quiche_moq_webtransport_helper's default (100).
    #[arg(long = "max-streams-bidi", conflicts_with_all = ["listen_max_streams_bidi", "connect_max_streams_bidi"])]
    pub max_streams_bidi: Option<u64>,

    /// Like --max-streams-bidi, but only for accepted (--listen) connections.
    #[arg(long = "listen-max-streams-bidi")]
    pub listen_max_streams_bidi: Option<u64>,

    /// Like --max-streams-bidi, but only for outbound (--connect / --upstream) connections.
    #[arg(long = "connect-max-streams-bidi")]
    pub connect_max_streams_bidi: Option<u64>,

    /// Create a low priority MoQ padding stream to reserve bandwidth
    #[arg(long)]
    pub padding_stream: bool,

    /// QUIC ping on SIGUSR1
    #[arg(long)]
    pub usr1_ping: bool,
    
    /// Toggle MoQ padding stream for on SIGUSR1
    #[arg(long)]
    pub usr1_toggle_padding_stream: bool,

    /// Withdraw (PUBLISH_NAMESPACE_DONE) this namespace on SIGUSR1, on every
    /// connection that actually announced it via `--pub <NS>`. Repeatable.
    #[arg(long = "usr1-unpub", value_name = "NS")]
    pub usr1_unpub: Vec<Namespace>,
}

impl Args {
    /// --listen-init-cwnd if set, else the global --init-cwnd (clap's
    /// `conflicts_with_all` guarantees at most one of the two is ever set).
    pub fn listen_init_cwnd(&self) -> Option<usize> {
        self.listen_init_cwnd.or(self.init_cwnd)
    }

    /// --connect-init-cwnd if set, else the global --init-cwnd.
    pub fn connect_init_cwnd(&self) -> Option<usize> {
        self.connect_init_cwnd.or(self.init_cwnd)
    }

    /// --listen-max-data if set, else the global --max-data.
    pub fn listen_max_data(&self) -> Option<u64> {
        self.listen_max_data.or(self.max_data)
    }

    /// --connect-max-data if set, else the global --max-data.
    pub fn connect_max_data(&self) -> Option<u64> {
        self.connect_max_data.or(self.max_data)
    }

    /// --listen-max-stream-data if set, else the global --max-stream-data.
    pub fn listen_max_stream_data(&self) -> Option<u64> {
        self.listen_max_stream_data.or(self.max_stream_data)
    }

    /// --connect-max-stream-data if set, else the global --max-stream-data.
    pub fn connect_max_stream_data(&self) -> Option<u64> {
        self.connect_max_stream_data.or(self.max_stream_data)
    }

    /// --listen-max-streams-uni if set, else the global --max-streams-uni.
    pub fn listen_max_streams_uni(&self) -> Option<u64> {
        self.listen_max_streams_uni.or(self.max_streams_uni)
    }

    /// --connect-max-streams-uni if set, else the global --max-streams-uni.
    pub fn connect_max_streams_uni(&self) -> Option<u64> {
        self.connect_max_streams_uni.or(self.max_streams_uni)
    }

    /// --listen-max-streams-bidi if set, else the global --max-streams-bidi.
    pub fn listen_max_streams_bidi(&self) -> Option<u64> {
        self.listen_max_streams_bidi.or(self.max_streams_bidi)
    }

    /// --connect-max-streams-bidi if set, else the global --max-streams-bidi.
    pub fn connect_max_streams_bidi(&self) -> Option<u64> {
        self.connect_max_streams_bidi.or(self.max_streams_bidi)
    }
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
        _ if s.as_bytes().len() == 1 => Ok(s.as_bytes()[0]),
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
        let digits = s.strip_prefix("draft").unwrap_or(s).trim_start_matches('-');
        let n: u64 = digits.parse().map_err(|_| format!("invalid MoQ draft version: {s:?}"))?;
        let version = moq_wire::version_from_draft_number(n)
            .ok_or_else(|| format!("unsupported MoQ draft version: {n} (supported: 07-18)"))?;
        Ok(MoqVersionArg(version))
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

/// `--pub` argument: either actively publish a track (PUBLISH 0x1D) or announce a namespace.
#[derive(Debug, Clone)]
pub struct PubArg {
    /// Only set when target is a Track.
    pub file: Option<PathBuf>,
    /// Byte delimiter for line-delimited input. Default `\n`.
    pub delimiter: u8,
    pub target: PubTarget,
}

#[derive(Debug, Clone)]
pub enum PubTarget {
    Track(NamespaceTrackname),
    Namespace(Namespace),
}

impl PubArg {
    pub fn as_track(&self) -> Option<(&NamespaceTrackname, Option<&PathBuf>)> {
        match &self.target {
            PubTarget::Track(nt) => Some((nt, self.file.as_ref())),
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

/// Parses fragment parameters shared by `--pub` and `--pub-passive`.
/// Returns `(head, file, delimiter)` where `head` is the track/namespace string
/// and `delimiter` defaults to `\n` if `#delimiter=` is not specified.
fn parse_in_param(s: &str) -> Result<(&str, Option<PathBuf>, u8), String> {
    let (head, params) = FragmentParams::split(s);
    let mut file = None;
    let mut delimiter = b'\n';
    for (key, value) in params.iter() {
        match key {
            "in" => file = Some(PathBuf::from(require_value(key, value)?)),
            "delimiter" => delimiter = parse_delimiter(require_value(key, value)?)?,
            _ => return Err(format!("unknown parameter `{key}` (expected: in, delimiter)")),
        }
    }
    Ok((head, file, delimiter))
}

impl FromStr for PubArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (rest, file, delimiter) = parse_in_param(s)?;
        // If `rest` contains `--`, it's a namespace--track (or pattern); otherwise just a namespace.
        if rest.contains("--") {
            let pattern: TrackPattern = rest.parse()?;
            let Some(nt) = pattern.as_exact() else {
                return Err("--pub doesn't support wildcard patterns (use --pub-passive to dynamically accept a namespace)".into());
            };
            Ok(Self { file, delimiter, target: PubTarget::Track(nt.clone()) })
        } else {
            if file.is_some() {
                return Err("#in= requires a namespace--track, not a bare namespace".into());
            }
            if rest == "*" {
                return Err("--pub doesn't support wildcard patterns (use --pub-passive to dynamically accept a namespace)".into());
            }
            Ok(Self { file: None, delimiter, target: PubTarget::Namespace(rest.parse()?) })
        }
    }
}

/// `--pub-passive`/`--sub-passive` argument: a standby track buffer, fed from
/// or drained to an optional file. Parsed from `<pattern>[#in=file]`, where
/// `pattern` is a concrete `namespace--track`, a `namespace--*` namespace
/// wildcard, or bare `*` matching anything.
#[derive(Debug, Clone)]
pub struct TrackArg {
    pub file: Option<PathBuf>,
    pub track: TrackPattern,
    /// Byte delimiter for line-delimited input. Default `\n`.
    pub delimiter: u8,
}

impl FromStr for TrackArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (track, file, delimiter) = parse_in_param(s)?;
        let track: TrackPattern = track.parse()?;
        if track.is_wildcard() && file.is_some() {
            return Err("#in= requires a concrete namespace--track, not a wildcard pattern".into());
        }
        Ok(Self { file, track, delimiter })
    }
}

/// A subscribe argument: a track flowing into an optional destination file,
/// with optional SUBSCRIBE options (`priority`, `delivery_timeout`, `filter`).
/// Parsed from `<pattern>[#out=file&option=value&...]`, where `pattern` is a
/// concrete `namespace--track`, a `namespace--*` namespace wildcard, or bare
/// `*` matching anything (wildcards only valid for `--sub-passive`).
#[derive(Debug, Clone)]
pub struct SubArg {
    pub track: TrackPattern,
    pub file: Option<PathBuf>,
    /// Output format and delimiter. Default: `OutputKind::Auto { delimiter: b'\n' }`.
    pub output_kind: OutputKind,
    /// `subscriber_priority` (0 = highest, 255 = lowest; MoQ default 128 if omitted).
    pub priority: Option<u8>,
    /// `delivery_timeout` in milliseconds.
    pub delivery_timeout: Option<u64>,
    /// SUBSCRIBE filter type (defaults to NextGroupStart when omitted).
    pub filter: Option<FilterType>,
}

/// Parses the `filter=` SUBSCRIBE option. Only the two location-less filter
/// types are accepted, since an outbound SUBSCRIBE here carries no
/// start/end Location (AbsoluteStart/AbsoluteRange would need one).
fn parse_filter(value: &str) -> Result<FilterType, String> {
    match value {
        "next-group-start" | "next-group" => Ok(FilterType::NextGroupStart),
        "largest-object" | "largest" => Ok(FilterType::LargestObject),
        _ => Err(format!("invalid filter {value:?}: expected next-group-start or largest-object")),
    }
}

impl FromStr for SubArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (rest, params) = FragmentParams::split(s);
        let mut file = None;
        let mut priority = None;
        let mut delivery_timeout = None;
        let mut filter = None;
        let mut format = "auto";
        let mut delimiter = b'\n';
        for (key, value) in params.iter() {
            match key {
                "out" => file = Some(PathBuf::from(require_value(key, value)?)),
                "format" => format = require_value(key, value)?,
                "delimiter" => delimiter = parse_delimiter(require_value(key, value)?)?,
                "priority" => {
                    let v = require_value(key, value)?;
                    priority = Some(v.parse::<u8>()
                        .map_err(|_| format!("invalid priority {v:?}: expected an integer 0-255"))?);
                }
                "delivery_timeout" => {
                    let v = require_value(key, value)?;
                    delivery_timeout = Some(v.parse::<u64>()
                        .map_err(|_| format!("invalid delivery_timeout {v:?}: expected a non-negative integer"))?);
                }
                "filter" => filter = Some(parse_filter(require_value(key, value)?)?),
                _ => return Err(format!("unknown parameter `{key}` (expected: out, format, delimiter, priority, delivery_timeout, filter)")),
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
        let track: TrackPattern = rest.parse()?;
        if track.is_wildcard() && (file.is_some() || priority.is_some() || delivery_timeout.is_some() || filter.is_some()) {
            return Err("wildcard patterns don't support #out=/#priority=/#delivery_timeout=/#filter=".into());
        }
        Ok(Self { track, file, output_kind, priority, delivery_timeout, filter })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_arg_with_file() {
        let arg: TrackArg = "tos--video#in=/tmp/video.mp4".parse().unwrap();
        assert_eq!(arg.file.unwrap().to_str().unwrap(), "/tmp/video.mp4");
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
    }

    #[test]
    fn track_arg_stdin() {
        let arg: TrackArg = "tos--video".parse().unwrap();
        assert!(arg.file.is_none());
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
    }

    #[test]
    fn track_arg_namespace_wildcard() {
        let arg: TrackArg = "tos--*".parse().unwrap();
        assert!(arg.file.is_none());
        assert!(arg.track.is_wildcard());
        assert!(arg.track.as_exact().is_none());
    }

    #[test]
    fn track_arg_wildcard() {
        let arg: TrackArg = "*".parse().unwrap();
        assert!(arg.file.is_none());
        assert!(arg.track.is_wildcard());
    }

    #[test]
    fn track_arg_wildcard_rejects_file() {
        let result: Result<TrackArg, _> = "tos--*#in=/tmp/video.mp4".parse();
        assert!(result.is_err());
        let result: Result<TrackArg, _> = "*#in=/tmp/video.mp4".parse();
        assert!(result.is_err());
    }

    #[test]
    fn sub_arg_with_file() {
        let arg: SubArg = "tos--video#out=/tmp/out.mp4".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.file.unwrap().to_str().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn sub_arg_stdout() {
        let arg: SubArg = "tos--video".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert!(arg.file.is_none());
        assert!(arg.priority.is_none());
    }

    #[test]
    fn sub_arg_with_priority() {
        let arg: SubArg = "tos--video#priority=5".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert!(arg.file.is_none());
        assert_eq!(arg.priority, Some(5));
    }

    #[test]
    fn sub_arg_with_file_and_priority() {
        let arg: SubArg = "tos--video#out=/tmp/out.mp4&priority=200".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.file.unwrap().to_str().unwrap(), "/tmp/out.mp4");
        assert_eq!(arg.priority, Some(200));
    }

    #[test]
    fn sub_arg_with_delivery_timeout() {
        let arg: SubArg = "tos--video#delivery_timeout=5000".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.delivery_timeout, Some(5000));
        assert!(arg.priority.is_none());
    }

    #[test]
    fn sub_arg_with_priority_and_delivery_timeout() {
        let arg: SubArg = "tos--video#out=/tmp/out.mp4&priority=10&delivery_timeout=5000".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "tos--video");
        assert_eq!(arg.file.unwrap().to_str().unwrap(), "/tmp/out.mp4");
        assert_eq!(arg.priority, Some(10));
        assert_eq!(arg.delivery_timeout, Some(5000));
    }

    #[test]
    fn sub_arg_with_filter() {
        let largest: SubArg = "tos--video#filter=largest-object".parse().unwrap();
        assert_eq!(largest.filter, Some(FilterType::LargestObject));
        let next: SubArg = "tos--video#filter=next-group".parse().unwrap();
        assert_eq!(next.filter, Some(FilterType::NextGroupStart));
        let none: SubArg = "tos--video".parse().unwrap();
        assert_eq!(none.filter, None);
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
        assert!(arg.file.is_none());
    }

    #[test]
    fn sub_arg_wildcard() {
        let arg: SubArg = "*".parse().unwrap();
        assert!(arg.track.is_wildcard());
    }

    #[test]
    fn sub_arg_wildcard_rejects_options() {
        let result: Result<SubArg, _> = "*#out=/tmp/out.mp4".parse();
        assert!(result.is_err());
        let result: Result<SubArg, _> = "tos--*#priority=5".parse();
        assert!(result.is_err());
    }

    #[test]
    fn track_arg_multi_component_namespace() {
        let arg: TrackArg = "org-example--audio".parse().unwrap();
        assert_eq!(arg.track.as_exact().unwrap().to_string(), "org-example--audio");
    }

    #[test]
    fn pub_arg_track() {
        let arg: PubArg = "tos--video#in=/tmp/video.mp4".parse().unwrap();
        assert_eq!(arg.file.as_ref().unwrap().to_str().unwrap(), "/tmp/video.mp4");
        let (nt, _) = arg.as_track().unwrap();
        assert_eq!(nt.to_string(), "tos--video");
    }

    #[test]
    fn pub_arg_rejects_wildcard() {
        let result: Result<PubArg, _> = "tos--*".parse();
        assert!(result.is_err());
        let result: Result<PubArg, _> = "*".parse();
        assert!(result.is_err());
    }

    #[test]
    fn pub_arg_namespace_only() {
        let arg: PubArg = "tos".parse().unwrap();
        assert!(arg.file.is_none());
        let ns = arg.as_namespace().unwrap();
        assert_eq!(ns.to_string(), "tos");
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
        let args = Args::try_parse_from(["moq", "--init-cwnd", "4", "--max-data", "1000", "--max-stream-data", "2000"]).unwrap();
        assert_eq!(args.listen_init_cwnd(), Some(4));
        assert_eq!(args.connect_init_cwnd(), Some(4));
        assert_eq!(args.listen_max_data(), Some(1000));
        assert_eq!(args.connect_max_data(), Some(1000));
        assert_eq!(args.listen_max_stream_data(), Some(2000));
        assert_eq!(args.connect_max_stream_data(), Some(2000));
    }

    /// A --listen-*/--connect-* override only affects its own side.
    #[test]
    fn quic_tuning_getters_prefer_per_side_override() {
        let args = Args::try_parse_from(["moq", "--listen-init-cwnd", "4", "--connect-max-data", "1000"]).unwrap();
        assert_eq!(args.listen_init_cwnd(), Some(4));
        assert_eq!(args.connect_init_cwnd(), None);
        assert_eq!(args.listen_max_data(), None);
        assert_eq!(args.connect_max_data(), Some(1000));
    }

    /// The global flag and its per-side overrides are mutually exclusive at parse
    /// time (clap's `conflicts_with_all`) — specifying both is a usage error, not
    /// silently resolved by precedence.
    #[test]
    fn quic_tuning_global_and_per_side_flags_conflict() {
        assert!(Args::try_parse_from(["moq", "--init-cwnd", "4", "--listen-init-cwnd", "3"]).is_err());
        assert!(Args::try_parse_from(["moq", "--max-data", "4", "--connect-max-data", "3"]).is_err());
        assert!(Args::try_parse_from(["moq", "--max-stream-data", "4", "--listen-max-stream-data", "3"]).is_err());
        // Per-side flags on both sides at once are fine (not mutually exclusive with each other).
        assert!(Args::try_parse_from(["moq", "--listen-init-cwnd", "4", "--connect-init-cwnd", "3"]).is_ok());
    }

    #[test]
    fn supported_versions_parses_comma_separated_list() {
        let args = Args::try_parse_from(["moq", "--supported-versions", "13,15,16"]).unwrap();
        let versions = args.supported_versions.unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions[0].0, 0xff00000d);
        assert_eq!(versions[1].0, 0xff00000f);
        assert_eq!(versions[2].0, 0xff000010);
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
