use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use std::time::{Duration, Instant};

use boring::ssl::{SslContextBuilder, SslMethod};
use log::{debug, error, info};
use mio::Interest;
use url::Url;

use moq_loc::{LocProperties, Timestamp};
use moq_object_cache::{CachedObjectRef, ObjectCache, PackedObjectCache};
use quiche_endpoint_utils::SignalSource;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::{
    ClientId, ConnMap, Endpoint, EndpointConfig, ServerConfig, make_qlog_writer, quiche,
};
use quiche_mio_runner::Socket;
use quiche_moq as moq;
use quiche_moq::wire::control_message::subscribe::FilterType;
use quiche_moq::wire::{
    Location, Namespace, NamespaceTrackname, Parameters, REQUEST_ERROR_DOES_NOT_EXIST,
    REQUEST_ERROR_UNINTERESTED, RequestId, version_to_name,
};
use quiche_moq::{NamespacePublishRequestAction, PublishRequestAction, SubscriptionRequestAction};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use quiche_utils::cert::load_or_generate_keys;
use quiche_utils::keepalive_handler::KeepaliveHandler;

use crate::conn::{ClientActivePubTrack, ClientSubTrack, ConnAppData, SubTrackTemplate};
use crate::file_output::FileOutputState;
use crate::local_track::{is_video_fourcc, FileInputState, LocalAliasState, LocalTrack, PendingInboundSubscription};
use crate::relay::{RelayPublisherInfo, RelaySubscriberInfo, RelaySubscription, aggregate_delivery_timeout};
use crate::{Args, PubTarget, TrackPattern};

// ── type alias ────────────────────────────────────────────────────────────────

type Runner = runner::Runner<ConnAppData, AppData, i32>;

/// Sentinel external-event value for the SIGINT/SIGTERM signal source.
/// Real file descriptors registered via `register_external` are always >= 0.
const SHUTDOWN_EVENT: i32 = -1;

/// Sentinel external-event value for the SIGUSR1 signal source.
const USR1_EVENT: i32 = -2;

/// Accepts a SUBSCRIBE for one of our own locally-served tracks (`--pub`/
/// `--pub-passive`, or a relay's directly-served track): creates the
/// `LocalTrack` entry if needed and registers the new alias so Phase 5
/// forwards cached objects to it.
fn accept_local_subscription(
    moq: &mut MoqHandle, ad: &mut AppData, cid: ClientId,
    request_id: RequestId, nt: &NamespaceTrackname, filter: FilterType, priority: u8,
    delivery_timeout: Option<u64>,
) {
    if !ad.local_tracks.contains_key(nt) {
        let t = ad.new_local_track();
        ad.local_tracks.insert(nt.clone(), t);
    }
    let local = ad.local_tracks.get_mut(nt).unwrap();
    let largest = local.cache.latest().map(|o: CachedObjectRef| o.location());
    // Skip stale cache entries if the subscriber's requested delivery timeout has already
    // elapsed for the latest cached object — avoids immediately serving an object that
    // would be rejected as ObjectExpired by the session at send time.
    let last_sent = local.start_location_for_filter_with_dt(filter, delivery_timeout);
    let alias = moq.accept_subscription(request_id, largest, None);
    moq.set_out_track_priority(alias, priority, false);
    local.aliases.push(LocalAliasState { conn_id: cid, alias, last_sent, remaining: 0 });
    info!("conn {cid} accept subscription {nt}");
}

// ── global app data ───────────────────────────────────────────────────────────

struct AppData {
    /// `--pub-passive` namespace/global wildcard patterns: any incoming
    /// SUBSCRIBE for a track under a matched namespace is dynamically
    /// accepted and served locally (a `LocalTrack` entry is created lazily),
    /// even without a concrete `--pub-passive`/`--pub` registration for it.
    pub_passive_wildcards: Vec<TrackPattern>,
    /// `--sub-passive` namespace/global wildcard patterns: any incoming
    /// SUBSCRIBE for a track under a matched namespace, and whose namespace
    /// is known via another peer's announcement or `--upstream`, is
    /// relay-forwarded. A non-empty list also means this node trusts peers'
    /// PUBLISH_NAMESPACE announcements (populates `namespaces` below) —
    /// without it there'd be nothing to relay-forward to anyway.
    sub_passive_wildcards: Vec<TrackPattern>,
    relay_namespace: Vec<TrackPattern>,
    cache_depth: usize,
    cache_max_obj: usize,
    cache_avg_obj: usize,
    local_tracks: HashMap<NamespaceTrackname, LocalTrack>,
    local_namespaces: HashSet<Namespace>,
    file_inputs: Vec<FileInputState>,
    // Server-mode relay routing between remote clients
    namespaces: HashMap<Namespace, ClientId>,
    relay_subscriptions: HashMap<NamespaceTrackname, RelaySubscription>,
    // The --upstream connection, if configured: a fallback publisher for any
    // SUBSCRIBE whose namespace isn't (yet) known via `namespaces` above.
    upstream_cid: Option<ClientId>,
    // Tracks we actively PUBLISH (0x1D) to all connections (from --pub ns--track).
    active_pub_tracks: Vec<NamespaceTrackname>,
    // Template used to initialize per-connection sub track state for new connections.
    sub_track_templates: Vec<SubTrackTemplate>,
    // Per-connection track state (keyed by ClientId — separate from ConnAppData
    // to avoid split-borrow conflicts when moq_handle borrows conn.conn).
    client_active_pub_tracks: HashMap<ClientId, Vec<ClientActivePubTrack>>,
    client_sub_tracks: HashMap<ClientId, Vec<ClientSubTrack>>,
    // Set when SIGINT/SIGTERM is received; post_handle_recvs closes sessions then stops the runner.
    shutdown_requested: bool,
    // SIGUSR1 received; post_handle_recvs acts on it per --usr1-ping /
    // --usr1-toggle-padding-stream, then clears it.
    usr1_requested: bool,
    // From --usr1-ping: force an immediate ping on every connection with keepalive
    // enabled on SIGUSR1, without waiting for the next keepalive interval (e.g. to
    // detect a NAT rebind).
    usr1_ping: bool,
    // From --usr1-toggle-padding-stream: start/stop the padding stream on every
    // connection on SIGUSR1.
    usr1_toggle_padding_stream: bool,
    // From --usr1-unpub: withdraw (PUBLISH_NAMESPACE_DONE) these namespaces on
    // every connection that actually announced them (see `ConnAppData::announced_namespaces`)
    // on SIGUSR1.
    usr1_unpub: Vec<Namespace>,
    // Owns the SIGUSR1 signalfd so on_file_event can drain it when USR1_EVENT fires.
    // Registered via the non-owning `register_external` (see run()); kept alive here instead.
    sigusr1_source: Option<SignalSource>,
    // From --timestamp. on_file_event (where line-delimited samples are
    // pushed) is a non-capturing `fn` too, so it's read from here.
    embed_timestamps: bool,
    // From the QLOGDIR environment variable, via clap (Args.qlog_dir). Read
    // by `setup_qlog`, which `EndpointConfig::setup_qlog` is now generic
    // over `AppData` for — see quiche_endpoint's `Endpoint::connect`/accept
    // path, which passes `&self.app_data` through to it.
    qlog_dir: Option<std::path::PathBuf>,
}

impl AppData {
    fn new_local_track(&self) -> LocalTrack {
        LocalTrack::new(self.cache_depth, self.cache_max_obj, self.cache_avg_obj)
    }
}

// ── entry point ───────────────────────────────────────────────────────────────

pub fn run(args: Args) {
    let r = build_runner(&args);
    finish_run(r);
}

/// Builds the fully-configured `Runner` for `args`: opens local track files,
/// constructs the `Endpoint`/`AppData`, establishes outbound `--connect` /
/// `--upstream` connections, and registers all sockets/fds/signal sources.
/// Shared by `run()` and (in tests) `run_with_extra_close()`, which differ
/// only in how they register their close source and invoke `r.run()`.
fn build_runner(args: &Args) -> Runner {
    let server_socket = args.listen.map(|p| Socket::bind(format!("0.0.0.0:{p}")).unwrap());
    let client_socket = server_socket.is_none().then(|| Socket::bind("0.0.0.0:0").unwrap());
    let local_addr = server_socket.as_ref().or(client_socket.as_ref()).unwrap().local_addr;

    if let Some(port) = args.listen { info!("listening on 0.0.0.0:{port}"); }

    let mut local_tracks: HashMap<NamespaceTrackname, LocalTrack> = HashMap::new();
    let mut local_namespaces: HashSet<Namespace> = HashSet::new();
    let mut active_pub_tracks: Vec<NamespaceTrackname> = Vec::new();
    let mut file_inputs: Vec<FileInputState> = Vec::new();
    let mut file_input_fds: Vec<RawFd> = Vec::new();
    let mut pub_passive_wildcards: Vec<TrackPattern> = Vec::new();

    // --pub-passive: standby local tracks served on SUBSCRIBE. Concrete
    // entries get a file/stdin feed; namespace/global wildcard entries carry
    // no file and instead become a policy (any incoming PUBLISH push for a
    // matching track is dynamically accepted and re-served — see
    // `accept_or_reject_track_publish`; a SUBSCRIBE for it only succeeds
    // once that's happened, since that's what actually populates
    // `local_tracks`). Namespace announcement is a separate, explicit
    // concern (--pub <NS>) — serving a track here doesn't imply announcing
    // its namespace.
    for ta in &args.pub_passive {
        let Some(nt) = ta.track.as_exact() else {
            pub_passive_wildcards.push(ta.track.clone());
            continue;
        };
        local_tracks.entry(nt.clone())
            .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj));
        let path = ta.file.as_deref().unwrap_or(std::path::Path::new("-"));
        let file = open_input(path);
        let fd = file.as_raw_fd();
        if is_epoll_compatible(&file) { file_input_fds.push(fd); }
        file_inputs.push(FileInputState::new(file, nt.clone(), ta.delimiter));
    }
    // --pub: active PUBLISH (ns--track) or PUBLISH_NAMESPACE announcement (namespace only)
    for pa in &args.publish {
        match &pa.target {
            PubTarget::Namespace(ns) => { local_namespaces.insert(ns.clone()); }
            PubTarget::Track(nt) => {
                local_tracks.entry(nt.clone())
                    .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj));
                active_pub_tracks.push(nt.clone());
                let path = pa.file.as_deref().unwrap_or(std::path::Path::new("-"));
                let file = open_input(path);
                let fd = file.as_raw_fd();
                if is_epoll_compatible(&file) { file_input_fds.push(fd); }
                file_inputs.push(FileInputState::new(file, nt.clone(), pa.delimiter));
            }
        }
    }

    let mut file_output_fds: Vec<RawFd> = Vec::new();
    let mut sub_passive_wildcards: Vec<TrackPattern> = Vec::new();
    for sub_arg in &args.subscribe {
        let nt = sub_arg.track.as_exact()
            .expect("--sub requires a concrete namespace--track, not a wildcard pattern");
        let track = local_tracks.entry(nt.clone())
            .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj));
        let path = sub_arg.file.as_deref().unwrap_or(std::path::Path::new("-"));
        let file = open_output(path);
        let fd = file.as_raw_fd();
        if is_epoll_compatible(&file) { file_output_fds.push(fd); }
        track.file_outputs.push(FileOutputState::new(file, sub_arg.output_kind.clone()));
    }
    for sub_arg in &args.sub_passive {
        let Some(nt) = sub_arg.track.as_exact() else {
            sub_passive_wildcards.push(sub_arg.track.clone());
            continue;
        };
        let track = local_tracks.entry(nt.clone())
            .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj));
        let path = sub_arg.file.as_deref().unwrap_or(std::path::Path::new("-"));
        let file = open_output(path);
        let fd = file.as_raw_fd();
        if is_epoll_compatible(&file) { file_output_fds.push(fd); }
        track.file_outputs.push(FileOutputState::new(file, sub_arg.output_kind.clone()));
    }

    let endpoint = Endpoint::new(
        args.listen.map(|_| {
            let (cert, key) = load_or_generate_keys(&args.cert, &args.key);
            let moq_cfg = make_moq_config(args);
            let mut c = ServerConfig::new(move |_| ConnAppData {
                moq_helper: MoqWebTransportHelper::new_server(moq_cfg.clone()),
                announced_namespaces: HashMap::new(),
                logged_connect: false,
                keepalive: None,
            });
            c.client_config = {
                let mut cfg = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, {
                    let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                    b.set_private_key(&key).unwrap();
                    b.set_certificate(&cert).unwrap();
                    b
                }).unwrap();
                MoqWebTransportHelper::configure_quic(&mut cfg);
                cfg.set_max_idle_timeout(args.timeout);
                if let Some(packets) = args.listen_init_cwnd() { cfg.set_initial_congestion_window_packets(packets); }
                if let Some(v) = args.listen_max_data() { cfg.set_initial_max_data(v); }
                if let Some(v) = args.listen_max_stream_data() {
                    cfg.set_initial_max_stream_data_bidi_remote(v);
                    cfg.set_initial_max_stream_data_bidi_local(v);
                    cfg.set_initial_max_stream_data_uni(v);
                }
                if let Some(v) = args.listen_max_streams_uni() { cfg.set_initial_max_streams_uni(v); }
                if let Some(v) = args.listen_max_streams_bidi() { cfg.set_initial_max_streams_bidi(v); }
                cfg
            };
            c
        }),
        { let mut c = EndpointConfig::default(); c.setup_qlog = setup_qlog; c },
        AppData {
            pub_passive_wildcards,
            sub_passive_wildcards,
            relay_namespace: args.relay_namespace.clone(),
            cache_depth: args.cache_depth,
            cache_max_obj: args.cache_max_obj,
            cache_avg_obj: args.cache_avg_obj,
            local_tracks,
            local_namespaces,
            file_inputs,
            namespaces: HashMap::new(),
            relay_subscriptions: HashMap::new(),
            upstream_cid: None,
            active_pub_tracks,
            sub_track_templates: args.subscribe.iter()
                .map(|a| SubTrackTemplate { track: a.track.as_exact().unwrap().clone(), passive: false, priority: a.priority, delivery_timeout: a.delivery_timeout, filter: a.filter })
                .chain(args.sub_passive.iter().filter_map(|a| a.track.as_exact().map(|nt| SubTrackTemplate { track: nt.clone(), passive: true, priority: a.priority, delivery_timeout: a.delivery_timeout, filter: a.filter })))
                .collect(),
            client_active_pub_tracks: HashMap::new(),
            client_sub_tracks: HashMap::new(),
            shutdown_requested: false,
            usr1_requested: false,
            usr1_ping: args.usr1_ping,
            usr1_toggle_padding_stream: args.usr1_toggle_padding_stream,
            usr1_unpub: args.usr1_unpub.clone(),
            sigusr1_source: None,
            embed_timestamps: args.timestamp,
            qlog_dir: args.qlog_dir.clone(),
        },
    );

    let mut r = Runner::new(
        { let mut c = runner::Config::<ConnAppData, AppData, i32>::default();
          c.post_handle_recvs = post_handle_recvs;
          c.on_external_event = Some(on_file_event);
          c },
        endpoint,
    );

    assert!(args.connect.iter().filter(|c| c.upstream).count() <= 1,
        "at most one --connect target may be marked #upstream");

    for conn in &args.connect {
        let cid = connect_outbound(&mut r, args, local_addr, &conn.url);
        if conn.upstream {
            info!("connecting to upstream {}", conn.url);
            r.endpoint.app_data_mut().upstream_cid = Some(cid);
        } else {
            info!("connecting to {}", conn.url);
            let ad = r.endpoint.app_data_mut();
            ad.client_sub_tracks.insert(cid, ad.sub_track_templates.iter().map(ClientSubTrack::from_template).collect());
        }
    }

    for fd in &file_input_fds {
        r.registry().register_external(&mut mio::unix::SourceFd(fd), Interest::READABLE, *fd);
    }
    for fd in &file_output_fds {
        r.registry().register_external(&mut mio::unix::SourceFd(fd), Interest::WRITABLE, *fd);
    }
    if let Some(sock) = server_socket { r.register_socket(sock); }
    if let Some(sock) = client_socket { r.register_socket(sock); }
    r.registry().register_external_owned(SignalSource::new(&[libc::SIGINT, libc::SIGTERM]), Interest::READABLE, SHUTDOWN_EVENT);
    let mut sigusr1 = SignalSource::new(&[libc::SIGUSR1]);
    r.registry().register_external(&mut sigusr1, Interest::READABLE, USR1_EVENT);
    r.endpoint.app_data_mut().sigusr1_source = Some(sigusr1);
    r
}

fn finish_run(mut r: Runner) {
    r.run();
}

fn open_input(path: &std::path::Path) -> File {
    use std::os::unix::fs::OpenOptionsExt;
    let path = if path.as_os_str() == "-" { std::path::Path::new("/dev/stdin") } else { path };
    std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK)
        .open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"))
}

fn open_output(path: &std::path::Path) -> File {
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let path = if path.as_os_str() == "-" { std::path::Path::new("/dev/stdout") } else { path };
    // Opening a FIFO O_WRONLY|O_NONBLOCK fails with ENXIO if no reader has
    // opened the read end yet (open(2)). Retrying isn't enough when there
    // are multiple FIFOs to open: e.g. ffmpeg opens its inputs one at a
    // time and won't get to the second until it's done probing the first,
    // which itself needs us to be running our event loop to feed it data —
    // a real deadlock if we block here waiting for ffmpeg's second open.
    // Opening O_RDWR instead is the standard sidestep: POSIX guarantees it
    // never blocks and never fails with ENXIO for a FIFO, since the single
    // fd satisfies both ends' rendezvous itself. We only ever write to it.
    let is_fifo = std::fs::metadata(path).map(|m| m.file_type().is_fifo()).unwrap_or(false);
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).custom_flags(libc::O_NONBLOCK);
    if is_fifo { opts.read(true).write(true); } else { opts.write(true); }
    opts.open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"))
}

fn is_epoll_compatible(file: &File) -> bool {
    if file.metadata().map(|m| m.file_type().is_file()).unwrap_or(true) {
        return false;
    }
    // Some character devices (notably /dev/null) aren't regular files but
    // still don't implement poll, so epoll_ctl(ADD) on them fails with
    // EPERM. Probe on a scratch epoll instance instead of asserting that
    // here and panicking later when the real registration happens.
    unsafe {
        let epfd = libc::epoll_create1(0);
        if epfd < 0 { return true; }
        let mut ev = libc::epoll_event { events: libc::EPOLLIN as u32, u64: 0 };
        let ok = libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, file.as_raw_fd(), &mut ev) == 0;
        libc::close(epfd);
        ok
    }
}

/// Builds the keepalive handler for an outbound connection (--connect)
/// from `--keepalive` / `--keepalive-no-pto-limit`. None if keepalive is disabled.
fn make_keepalive(args: &Args) -> Option<KeepaliveHandler> {
    (args.keepalive != 0).then(|| {
        let interval = Duration::from_millis(args.keepalive);
        if args.keepalive_no_pto_limit {
            KeepaliveHandler::new_no_pto_limit(interval)
        } else {
            KeepaliveHandler::new(interval)
        }
    })
}

/// `EndpointConfig::setup_qlog` callback: writes a qlog file for `conn` under
/// `app_data.qlog_dir`, if set. `quiche_endpoint::Endpoint` passes its own
/// `&app_data` through to this fn pointer (it's generic over `TAppData`),
/// so `Args.qlog_dir` (populated purely from the QLOGDIR environment
/// variable via clap) reaches here without this function reading the
/// environment itself.
fn setup_qlog(conn: &mut quiche::Connection, role: &str, scid: &quiche::ConnectionId, app_data: &AppData) {
    let Some(dir) = app_data.qlog_dir.as_ref() else { return };
    let id = format!("{:?}", scid);
    let writer = make_qlog_writer(dir.as_os_str(), role, &id);
    conn.set_qlog(Box::new(writer), format!("quiche-{role} qlog"), format!("quiche-{role} qlog id={id}"));
}

/// Opens the keylog file configured via `--ssl-key-log-file` / `SSLKEYLOGFILE`,
/// for an outbound (--connect / --upstream) connection. Same as the `moq_utils`
/// pattern: `cfg.log_keys()` must be called on the `quiche::Config` *before*
/// `connect()`, and `conn.set_keylog(...)` *after* it returns (connect() never
/// processes received bytes itself, so no keys are derived in between).
fn open_keylog(args: &Args) -> Option<File> {
    let path = args.ssl_key_log_file.as_ref()?;
    Some(std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap())
}

/// Builds the `moq::Config` for an outbound (--connect / --upstream) connection,
/// restricting the offered version to `--setup-version` if set.
fn make_moq_config(args: &Args) -> moq::Config {
    let mut cfg = moq::Config::default();
    if let Some(versions) = args.supported_versions.as_ref().map(|vs| vs.iter().map(|v| v.0).collect())
        .or(args.setup_version.map(|v| vec![v.0]))
    {
        // draft >= 15 negotiates via ALPN/WT-Available-Protocols, not
        // CLIENT_SETUP's version list — a lone new-draft entry pins that
        // version directly; anything else (multiple entries, or a new-draft
        // entry mixed with others) can't be conveyed on the wire and is
        // rejected by `VersionNegotiation::validate` when the session is
        // constructed.
        cfg.version_negotiation = match versions.as_slice() {
            [v] if *v >= moq::wire::MOQ_VERSION_DRAFT_15 => moq::VersionNegotiation::Pinned(*v),
            _ => moq::VersionNegotiation::InBand { candidates: versions },
        };
    }
    if args.padding_stream {
        cfg.enable_padding_stream = true;
    }
    cfg
}

/// Opens an outbound QUIC connection to `url_str`, shared by `--connect` and
/// `--upstream` (which differ only in what the caller does with the
/// resulting `ClientId` afterward: populate `client_sub_tracks`, or set
/// `AppData::upstream_cid`).
fn connect_outbound(r: &mut Runner, args: &Args, local_addr: std::net::SocketAddr, url_str: &str) -> ClientId {
    let url = Url::parse(url_str).unwrap();
    let addrs = url.socket_addrs(|| Some(443)).unwrap();
    let peer_addr = *addrs.iter().find(|a| a.is_ipv4() == local_addr.is_ipv4())
        .unwrap_or_else(|| addrs.first().expect("DNS resolution returned no addresses"));
    let keylog = open_keylog(args);
    let mut cfg = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
    MoqWebTransportHelper::configure_quic(&mut cfg);
    cfg.verify_peer(false);
    cfg.set_max_idle_timeout(args.timeout);
    if let Some(packets) = args.connect_init_cwnd() { cfg.set_initial_congestion_window_packets(packets); }
    if let Some(v) = args.connect_max_data() { cfg.set_initial_max_data(v); }
    if let Some(v) = args.connect_max_stream_data() {
        cfg.set_initial_max_stream_data_bidi_remote(v);
        cfg.set_initial_max_stream_data_bidi_local(v);
        cfg.set_initial_max_stream_data_uni(v);
    }
    if let Some(v) = args.connect_max_streams_uni() { cfg.set_initial_max_streams_uni(v); }
    if let Some(v) = args.connect_max_streams_bidi() { cfg.set_initial_max_streams_bidi(v); }
    if keylog.is_some() { cfg.log_keys(); }
    let cid = r.endpoint.connect(None, local_addr, peer_addr, &mut cfg,
        ConnAppData { 
            moq_helper: MoqWebTransportHelper::new_client(url.clone(), make_moq_config(args)),
            announced_namespaces: HashMap::new(), 
            logged_connect: false,
            keepalive: make_keepalive(args),
        },
        None, None);
    if let Some(keylog) = keylog {
        r.endpoint.conn_mut(cid).unwrap().conn.set_keylog(Box::new(keylog));
    }
    cid
}

/// Spawn the node in a background thread. Drop the returned `StopHandle` to shut it down.
/// The `JoinHandle` can be used to wait for the thread to exit after stopping.
#[cfg(test)]
pub fn run_stoppable(args: Args) -> (std::thread::JoinHandle<()>, StopHandle) {
    let (stop_r, stop_w) = mio::net::UnixStream::pair().unwrap();
    let handle = std::thread::spawn(move || {
        // build_runner re-uses the public run() body up to the r.run() call
        // We replay it with one extra close source: the stop socket.
        // Easiest: call run_with_close(args, stop_r).
        run_with_extra_close(args, stop_r);
    });
    (handle, StopHandle(stop_w))
}

#[cfg(test)]
pub struct StopHandle(#[allow(dead_code)] mio::net::UnixStream);

#[cfg(test)]
fn run_with_extra_close(args: Args, extra: mio::net::UnixStream) {
    let mut r = build_runner(&args);
    // Same graceful path as a real SIGINT, so tests exercise the actual shutdown logic.
    r.registry().register_external_owned(extra, Interest::READABLE, SHUTDOWN_EVENT);
    r.run();
}

// ── file event ────────────────────────────────────────────────────────────────

fn on_file_event(endpoint: &mut Endpoint<ConnAppData, AppData>, fd: &i32) {
    if *fd == SHUTDOWN_EVENT {
        endpoint.app_data_mut().shutdown_requested = true;
        return;
    }
    if *fd == USR1_EVENT {
        let ad = endpoint.app_data_mut();
        if let Some(ref source) = ad.sigusr1_source { source.drain(); }
        ad.usr1_requested = true;
        return;
    }
    let ad = endpoint.app_data_mut();
    let Some(idx) = ad.file_inputs.iter().position(|i| i.file.as_raw_fd() == *fd) else {
        // Writable event on an output fd — drain buffers
        for track in ad.local_tracks.values_mut() {
            let cache = &track.cache;
            for out in &mut track.file_outputs { out.drain(cache); }
        }
        return;
    };
    let track_nt = ad.file_inputs[idx].track.clone();
    let embed_timestamps = ad.embed_timestamps;
    let (file_inputs, local_tracks) = (&mut ad.file_inputs, &mut ad.local_tracks);
    if let Some(local) = local_tracks.get_mut(&track_nt) {
        file_inputs[idx].drain_into(local, embed_timestamps);
    }
}

// ── main event loop ───────────────────────────────────────────────────────────
//
// TODO: namespace announcements are currently gated only by whether any
// --sub-passive wildcard is configured (all-or-nothing). Add a mechanism to
// restrict *who* may announce / *which* namespaces are accepted (e.g. an
// allow-list or per-connection policy), instead of trusting any connected
// peer once relay-forwarding is on.

fn post_handle_recvs(r: &mut Runner) {
    // Shutdown: SIGINT/SIGTERM received. Close each MoQ session (sends a
    // CLOSE_WEBTRANSPORT_SESSION capsule, draft-ietf-moq-transport-16 Section 3.4)
    // and the underlying QUIC connection, then ask the runner to stop. The normal
    // send_all() later this iteration flushes those frames before the loop exits.
    if r.endpoint.app_data().shutdown_requested {
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        for (_, conn) in conns.iter_mut() {
            if let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) {
                moq.close(quiche_moq::wire::NO_ERROR, "");
            }
            let _ = conn.conn.close(false, 0x0, b"");
        }
        r.close();
        return;
    }

    // Keepalive: ping outbound connections (--connect) per --keepalive.
    {
        let now = Instant::now();
        let mut app_timeout: Option<Duration> = None;
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        for (_, conn) in conns.iter_mut() {
            if let Some(keepalive) = &mut conn.app_data.keepalive
                && let Some(remaining) = keepalive.handle(&mut conn.conn, now) {
                app_timeout = Some(app_timeout.map_or(remaining, |t| t.min(remaining)));
            }
        }
        if let Some(t) = app_timeout { r.set_app_timeout(t); }
    }

    // SIGUSR1 — clear the flag and snapshot which actions to take this tick.
    let (usr1_ping, usr1_toggle_padding_stream, usr1_unpub) = if r.endpoint.app_data().usr1_requested {
        r.endpoint.app_data_mut().usr1_requested = false;
        let ad = r.endpoint.app_data();
        (ad.usr1_ping, ad.usr1_toggle_padding_stream, ad.usr1_unpub.clone())
    } else {
        (false, false, Vec::new())
    };

    // --usr1-ping: force an immediate ping on every connection (no MoQ state change,
    // so this can run before handle()).
    if usr1_ping {
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        for (_, conn) in conns.iter_mut() {
            let paths: Vec<_> = conn.conn.path_stats().map(|p| (p.local_addr, p.peer_addr)).collect();
            for (local, peer) in paths {
                let _ = conn.conn.send_ack_eliciting_on_path(local, peer);
            }
        }
        info!("sent ping on all connections (SIGUSR1)");
    }

    // cleanup_closed_conns
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        if !conn.conn.is_closed() { continue; }
        info!("conn {cid} disconnected");
        ad.namespaces.retain(|_, owner| *owner != cid);
        for sub in ad.relay_subscriptions.values_mut() {
            if sub.publisher.as_ref().is_some_and(|p| p.client_id == cid) {
                sub.publisher = None;
                for s in &mut sub.subscribers { s.publisher_gone = true; }
            }
            sub.subscribers.retain(|s| s.client_id != cid);
        }
        for track in ad.local_tracks.values_mut() { track.aliases.retain(|a| a.conn_id != cid); }
        ad.client_sub_tracks.remove(&cid);
        if ad.upstream_cid == Some(cid) { ad.upstream_cid = None; }
    }

    // tick_connections
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        let is_server = conn.conn.is_server();
        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        if let Some(v) = moq.version() && !conn.app_data.logged_connect {
            let peer = moq.quic.path_stats().next().map(|s| s.peer_addr).unwrap();
            let verb = if is_server { "accept" } else { "connected to" };
            info!("conn {cid} {verb} {peer} v{}", version_to_name(v));
            conn.app_data.logged_connect = true;
        }
        if Some(cid) == ad.upstream_cid {
            process_upstream_conn(cid, moq, ad);
        } else {
            // Serving local tracks (--pub/--pub-passive) and relay-forwarding
            // (--sub-passive wildcards) don't depend on which side dialed the
            // QUIC connection — a plain outbound (--connect) publisher must
            // accept/reject incoming SUBSCRIBEs exactly like an accepted one.
            process_server_conn(cid, moq, ad);
        }
    }

    // announce_namespaces: send PUBLISH_NAMESPACE to every connection (accepted,
    // --connect, and --upstream alike). Own `--pub <NS>` namespaces
    // (`local_namespaces`) are always announced — note that `--pub-passive`/
    // `--pub <NS--TRACK>` serve a track without announcing its namespace; pair
    // them with an
    // explicit `--pub <NS>` if announcement is wanted too. With
    // `--relay-namespaces`, namespaces learned from elsewhere (`namespaces`,
    // via `--upstream` or another peer's announcement) are also announced —
    // skipped for the connection that is their source — and un-announced
    // (PUBLISH_NAMESPACE_DONE) once they disappear.
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for (cid, conn) in conns.iter_mut() {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        if !ad.relay_namespace.is_empty() {
            conn.app_data.announced_namespaces.retain(|ns, rid| {
                let still_known = ad.local_namespaces.contains(ns)
                    || ad.namespaces.get(ns).is_some_and(|&owner| owner != cid);
                if still_known { true } else { moq.publish_namespace_done(*rid); false }
            });
        }
        let relay_ns = &ad.relay_namespace;
        let relayed = ad.namespaces.keys().filter(|ns| relay_ns.iter().any(|p| p.matches_namespace(ns)));
        for ns in ad.local_namespaces.iter().chain(relayed) {
            if ad.namespaces.get(ns).is_some_and(|&owner| owner == cid) { continue; }
            if conn.app_data.announced_namespaces.contains_key(ns) { continue; }
            if let Ok(rid) = moq.publish_namespace(ns.0.0.clone()) {
                conn.app_data.announced_namespaces.insert(ns.clone(), rid);
                info!("conn {cid} announce namespace {ns}");
            }
        }
    }

    // send_active_publishes: PUBLISH (0x1D) to all initialized connections
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    if !ad.active_pub_tracks.is_empty() {
        let (cache_depth, cache_max_obj, cache_avg_obj) = (ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj);
        let all_cids: Vec<ClientId> = conns.iter()
            .map(|(cid, _)| cid)
            .collect();
        for cid in all_cids {
            let entry = ad.client_active_pub_tracks.entry(cid).or_default();
            for nt in &ad.active_pub_tracks {
                if entry.iter().any(|e| &e.track == nt) { continue; }
                entry.push(ClientActivePubTrack { track: nt.clone(), request_id: None });
            }
            let Some(mut moq) = moq_handle_for(conns, cid) else { continue };
            let entry = ad.client_active_pub_tracks.get_mut(&cid).unwrap();
            for apt in entry.iter_mut() {
                if apt.request_id.is_none() {
                    match moq.publish_track(apt.track.clone(), Parameters(vec![])) {
                        Ok((rid, alias)) => {
                            apt.request_id = Some(rid);
                            if !ad.local_tracks.contains_key(&apt.track) {
                            ad.local_tracks.insert(apt.track.clone(), LocalTrack::new(cache_depth, cache_max_obj, cache_avg_obj));
                        }
                        let local = ad.local_tracks.get_mut(&apt.track).unwrap();
                            local.aliases.push(LocalAliasState { conn_id: cid, alias, last_sent: None, remaining: 0 });
                            info!("conn {cid} publish track {}", apt.track);
                        }
                        Err(e) => { error!("publish_track {}: {e:?}", apt.track); }
                    }
                } else {
                    let rid = apt.request_id.unwrap();
                    if let Some(resp) = moq.poll_publish_response(rid) {
                        match resp {
                            Ok(_) => info!("conn {cid} accepted publish {}", apt.track),
                            Err(e) => error!("conn {cid} rejected publish {}: {e}", apt.track),
                        }
                    }
                }
            }
        }
    }

    // handle_client_connections: accept incoming subscriptions/PUBLISH + sub track draining.
    // client_sub_tracks lives in AppData to avoid split-borrow conflicts.
    // Namespace announcement for these connections is handled by announce_namespaces.
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    let client_cids: Vec<ClientId> = conns.iter()
        .filter(|(cid, c)| !c.conn.is_server() && Some(*cid) != ad.upstream_cid)
        .map(|(cid, _)| cid)
        .collect();

    for cid in client_cids {
        let Some(mut moq) = moq_handle_for(conns, cid) else { continue };

        // A publisher always serves whatever it's been asked to publish, so
        // every pending subscription resolves to Accept — but the closure
        // holds `&mut moq` and can't call `accept_subscription` itself, so
        // collect decisions and return `Keep`, then accept in a second pass
        // below (same split used by `process_server_conn`).
        let mut pending: Vec<PendingInboundSubscription> = Vec::new();
        moq.process_subscription_requests(|&request_id, sub_msg, _track_alias| {
            pending.push(PendingInboundSubscription {
                request_id,
                track: sub_msg.namespace_trackname.clone(),
                filter: sub_msg.filter_type,
                priority: sub_msg.subscriber_priority,
                delivery_timeout: sub_msg.delivery_timeout(),
            });
            SubscriptionRequestAction::Keep
        });
        for p in pending {
            accept_local_subscription(&mut moq, ad, cid, p.request_id, &p.track, p.filter, p.priority, p.delivery_timeout);
        }

        // Accept incoming PUBLISH if a matching --sub/--sub-passive entry
        // exists, or it matches a --pub-passive namespace/global wildcard.
        accept_or_reject_track_publish(&mut moq, ad, cid);
        // Sub tracks: send SUBSCRIBE (active) or wait for PUBLISH (passive), then drain objects
        drain_client_sub_tracks(&mut moq, ad, cid);
    }

    // forward_relay_subscriptions
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    let mut unservable: Vec<NamespaceTrackname> = Vec::new();
    for (nt, sub) in ad.relay_subscriptions.iter_mut() {
        let wanted_delivery_timeout = aggregate_delivery_timeout(&sub.subscribers);
        // Send SUBSCRIBE upstream if not done
        if !sub.is_sent() {
            if sub.publisher.is_none() {
                let Some(pub_id) = ad.namespaces.get(nt.namespace()).copied().or(ad.upstream_cid) else { continue };
                sub.publisher = Some(RelayPublisherInfo { client_id: pub_id, request_id: None, track_alias: None, largest_location: None, delivery_timeout: None, max_cache_duration: None });
            }
            let Some(pub_info) = sub.publisher.as_mut() else { continue };
            let Some(mut moq) = moq_handle_for(conns, pub_info.client_id) else { continue };
            if let Ok(rid) = moq.subscribe(nt, None, wanted_delivery_timeout, FilterType::NextGroupStart) {
                pub_info.request_id = Some(rid);
                pub_info.delivery_timeout = wanted_delivery_timeout;
            }
        } else if sub.is_publisher_accepted() && sub.publisher.as_ref().is_some_and(|p| p.delivery_timeout != wanted_delivery_timeout) {
            // A subscriber joined, left, or updated its own delivery_timeout
            // since the initial SUBSCRIBE: tell upstream the new aggregate.
            let Some(pub_info) = sub.publisher.as_mut() else { continue };
            let Some(rid) = pub_info.request_id else { continue };
            let Some(mut moq) = moq_handle_for(conns, pub_info.client_id) else { continue };
            if moq.request_update_delivery_timeout(rid, wanted_delivery_timeout).is_ok() {
                pub_info.delivery_timeout = wanted_delivery_timeout;
            }
        }
        // Poll publisher accept
        if !sub.is_publisher_accepted() {
            let Some(pub_info) = sub.publisher.as_mut() else { continue };
            let Some(rid) = pub_info.request_id else { continue };
            let Some(mut moq) = moq_handle_for(conns, pub_info.client_id) else { continue };
            match moq.poll_subscribe_response(rid) {
                Some(Ok((alias, ok_msg))) => {
                    pub_info.track_alias = Some(alias);
                    pub_info.largest_location = ok_msg.largest_location();
                    pub_info.max_cache_duration = ok_msg.max_cache_duration();
                }
                Some(Err(_)) => {
                    // Publisher (or upstream fallback) doesn't have this
                    // namespace either: reject everyone waiting on it rather
                    // than leaving them hanging forever.
                    for s in &sub.subscribers {
                        if let Some(mut moq) = moq_handle_for(conns, s.client_id) {
                            moq.reject_subscription(s.request_id, REQUEST_ERROR_DOES_NOT_EXIST);
                        }
                    }
                    unservable.push(nt.clone());
                    continue;
                }
                None => {}
            }
        }
        // Accept pending subscribers
        if sub.is_publisher_accepted() {
            let largest = sub.publisher.as_ref().and_then(|p| p.largest_location);
            let max_cache_duration = sub.publisher.as_ref().and_then(|p| p.max_cache_duration);
            for s in &mut sub.subscribers {
                if s.is_accepted() { continue; }
                if let Some(mut moq) = moq_handle_for(conns, s.client_id) {
                    let alias = moq.accept_subscription(s.request_id, largest, max_cache_duration);
                    moq.set_out_track_priority(alias, s.subscriber_priority, false);
                    s.track_alias = Some(alias);
                }
            }
        }
        forward_relay_subscription(nt, sub, conns);
    }
    for nt in unservable {
        ad.relay_subscriptions.remove(&nt);
    }

    // finalize_disconnected_relay_subs: send PUBLISH_DONE to subscribers whose publisher disconnected.
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for sub in ad.relay_subscriptions.values_mut() {
        sub.subscribers.retain(|s| {
            if !s.publisher_gone { return true; }
            if let Some(mut moq) = moq_handle_for(conns, s.client_id) {
                moq.publish_done(s.request_id);
                info!("conn {} publish done for {}", s.client_id, s.request_id);
            }
            false
        });
    }

    // forward_local_tracks: forward local track caches → QUIC aliases
    let (conns, ad) = r.endpoint.mut_conns_and_app_data();
    for track in ad.local_tracks.values_mut() {
        if track.aliases.is_empty() { continue; }
        forward_local_track(track, conns);
    }

    // --usr1-toggle-padding-stream: runs after handle() so dirty is already set;
    // post_handle below will flush the padding stream state to the lower layers.
    if usr1_toggle_padding_stream {
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        for (_, conn) in conns.iter_mut() {
            let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
            if moq.padding_stream_running() {
                moq.stop_padding_stream();
            } else {
                moq.start_padding_stream();
            }
        }
        info!("toggled padding stream on all connections (SIGUSR1)");
    }

    // --usr1-unpub: withdraw (PUBLISH_NAMESPACE_DONE) each namespace this
    // connection actually announced (tracked in `announced_namespaces`,
    // populated by the announce_namespaces phase above) — same post-handle
    // placement as --usr1-toggle-padding-stream above.
    if !usr1_unpub.is_empty() {
        let (conns, _) = r.endpoint.mut_conns_and_app_data();
        for (cid, conn) in conns.iter_mut() {
            let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
            for ns in &usr1_unpub {
                if let Some(rid) = conn.app_data.announced_namespaces.remove(ns) {
                    moq.publish_namespace_done(rid);
                    info!("conn {cid} withdrew namespace {ns} (SIGUSR1)");
                }
            }
        }
    }

    // post handle phase
    for_each_moq_handle(&mut r.endpoint, |moq, _| {
        moq.post_handle();
    });
}

/// run f for every moq_handle
fn for_each_moq_handle<F>(e: &mut Endpoint<ConnAppData, AppData>, mut f: F)
where 
    F: FnMut(&mut MoqHandle, &mut AppData)
{
    let (conns, app_data) = e.mut_conns_and_app_data();
    for (_, conn) in conns {
        let Some(mut moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else { continue };
        f(&mut moq, app_data);
    }
     
}

/// Accepts a PUBLISH if a matching `--sub`/`--sub-passive` entry for `cid` is
/// waiting for it (sets its `track_alias` so `drain_client_sub_tracks` can
/// start reading objects). Failing that, also accepts it if it matches a
/// `--pub-passive` namespace/global wildcard — dynamically registering a
/// passive sub track for it, the same way a concrete `--sub-passive` entry
/// would have — otherwise rejects it as uninteresting.
fn accept_or_reject_track_publish(moq: &mut MoqHandle, ad: &mut AppData, cid: ClientId) {
    let pub_passive_wildcards = ad.pub_passive_wildcards.clone();
    moq.process_track_publish_requests(|_request_id, cm| {
        let nt = cm.namespace_trackname.clone();
        let alias = cm.track_alias;
        let subs = ad.client_sub_tracks.entry(cid).or_default();
        if let Some(st) = subs.iter_mut().find(|s| s.track == nt && s.track_alias.is_none()) {
            st.request_id = Some(0);
            st.track_alias = Some(alias);
            info!("conn {cid} accept publish {nt}");
            return PublishRequestAction::Accept(Parameters(vec![]));
        }
        if pub_passive_wildcards.iter().any(|p| p.matches(&nt)) {
            subs.push(ClientSubTrack::from_accepted_publish(nt.clone(), alias));
            info!("conn {cid} accept publish {nt} (--pub-passive wildcard)");
            return PublishRequestAction::Accept(Parameters(vec![]));
        }
        info!("conn {cid} reject publish {nt}");
        PublishRequestAction::Reject(REQUEST_ERROR_UNINTERESTED)
    });
}

/// Drives every `--sub`/`--sub-passive` entry for `cid` forward one tick:
/// sends SUBSCRIBE for active entries that haven't yet (passive entries wait
/// for an incomi ng PUBLISH — see `accept_or_reject_track_publish` — to set
/// their `track_alias` instead), polls for the resulting alias, then drains
/// any objects available on it into the local track cache (and its file
/// outputs, if configured).
fn drain_client_sub_tracks(moq: &mut MoqHandle, ad: &mut AppData, cid: ClientId) {
    let (cache_depth, cache_max_obj, cache_avg_obj) = (ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj);
    let Some(sub_tracks) = ad.client_sub_tracks.get_mut(&cid) else { return };
    for st in sub_tracks.iter_mut() {
        if st.request_id.is_none() {
            if st.passive { continue; } // wait for incoming PUBLISH to set track_alias
            match moq.subscribe(&st.track, st.priority, st.delivery_timeout, st.filter.unwrap_or(FilterType::NextGroupStart)) {
                Ok(rid) => { st.request_id = Some(rid); info!("conn {cid} subscribe {}", st.track); }
                Err(e) => { error!("subscribe {}: {e:?}", st.track); continue; }
            }
        }
        if st.track_alias.is_none() {
            if let Some(rid) = st.request_id {
                match moq.poll_subscribe_response(rid) {
                    Some(Ok((alias, _))) => { st.track_alias = Some(alias); info!("conn {cid} accepted subscribe {}", st.track); }
                    Some(Err(e)) => { error!("subscribe rejected {}: {e}", st.track); }
                    None => continue,
                }
            }
        }
        let Some(alias) = st.track_alias else { continue };
        let track_nt = st.track.clone();
        'objects: loop {
            if st.obj_hdr.is_none() {
                match moq.read_obj_hdr(alias) {
                    Ok(h) => st.obj_hdr = Some(h),
                    Err(moq::Error::Done | moq::Error::Fin) => break,
                    Err(e) => { error!("read_obj_hdr: {e:?}"); break; }
                }
            }
            let payload_len = st.obj_hdr.as_ref().unwrap().payload_len();
            while st.obj_buf.len() < payload_len {
                let mut tmp = vec![0u8; payload_len - st.obj_buf.len()];
                match moq.read_obj_pld(&mut tmp, alias) {
                    Ok(n) => st.obj_buf.extend_from_slice(&tmp[..n]),
                    Err(moq::Error::Done) => break 'objects,
                    Err(_) => { st.obj_hdr = None; st.obj_buf.clear(); continue 'objects; }
                }
            }
            if st.obj_buf.len() < payload_len { break; }
            let ext_hdrs = st.obj_hdr.take().unwrap().extension_headers().clone();
            let payload = st.obj_buf.split_to(payload_len).freeze();
            let props = LocProperties::from_kvps(&ext_hdrs);
            if !ad.local_tracks.contains_key(&track_nt) {
                ad.local_tracks.insert(track_nt.clone(), LocalTrack::new(cache_depth, cache_max_obj, cache_avg_obj));
            }
            let local = ad.local_tracks.get_mut(&track_nt).unwrap();
            if let Some(cfg) = props.codec_config() {
                if local.codec_config.is_none() {
                    local.is_audio = cfg.len() >= 4 && !is_video_fourcc(&cfg[..4]);
                    local.codec_config = Some(cfg.to_vec());
                }
            }
            let is_sync = props.codec_config().is_some();
            let decode_time_us = props.timestamp().map(|Timestamp(t)| t).unwrap_or(0);
            local.push_sample(&payload, is_sync, decode_time_us);
            let cache = &local.cache;
            for out in &mut local.file_outputs { out.drain(cache); }
        }
    }
}

// ── server connection handler ─────────────────────────────────────────────────

fn process_server_conn(cid: ClientId, mut moq: MoqHandle, ad: &mut AppData) {
    // Collect subscription decisions without calling moq inside the closure
    // (process_subscription_requests takes &mut moq, preventing a second borrow for accept_subscription).
    enum SubDecision {
        AcceptLocal { request_id: RequestId, nt: NamespaceTrackname, filter: FilterType, priority: u8, delivery_timeout: Option<u64> },
        QueueRelay { request_id: RequestId, nt: NamespaceTrackname, filter: FilterType, priority: u8, delivery_timeout: Option<u64> },
    }
    let mut decisions: Vec<SubDecision> = Vec::new();
    moq.process_subscription_requests(|request_id, cm, _track_alias| {
        let request_id = *request_id;
        let nt = cm.namespace_trackname.clone();
        // Explicitly registered track (--pub-passive / --pub) — serve locally.
        // (--pub-passive's namespace/global wildcard form is a *different*
        // policy — accepting an incoming PUBLISH push dynamically, see
        // `accept_or_reject_track_publish` — and must not also short-circuit
        // SUBSCRIBE handling here, or a pure relay with `--pub-passive "*"`
        // would swallow every SUBSCRIBE into an empty, unfed local track
        // instead of relay-forwarding it.)
        if ad.local_tracks.contains_key(&nt) {
            decisions.push(SubDecision::AcceptLocal { request_id, nt, filter: cm.filter_type, priority: cm.subscriber_priority, delivery_timeout: cm.delivery_timeout() });
            return SubscriptionRequestAction::Keep;
        }
        // Known namespace (via another peer's announcement or --upstream),
        // matched by a --sub-passive namespace/global wildcard: queue it for
        // relay forwarding. The fallback case (no namespace match but
        // --upstream is configured) is resolved against `ad.upstream_cid`
        // when actually forwarded (see the `QueueRelay` decision below); if
        // upstream rejects it too, the downstream subscriber is rejected
        // then (Phase 4).
        if ad.sub_passive_wildcards.iter().any(|p| p.matches(&nt)) && (ad.namespaces.contains_key(nt.namespace()) || ad.upstream_cid.is_some()) {
            // `process_subscription_requests` re-invokes this closure on every
            // tick for any request still `Keep`-pending, which a still-pending
            // relay subscription is until the upstream publisher accepts it
            // (potentially many ticks later) — guard against re-queueing the
            // same request_id every tick in the meantime.
            let already_queued = ad.relay_subscriptions.get(&nt)
                .is_some_and(|sub| sub.subscribers.iter().any(|s| s.client_id == cid && s.request_id == request_id));
            if !already_queued {
                decisions.push(SubDecision::QueueRelay { request_id, nt, filter: cm.filter_type, priority: cm.subscriber_priority, delivery_timeout: cm.delivery_timeout() });
            }
            return SubscriptionRequestAction::Keep;
        }
        info!("conn {cid} reject subscription {nt}");
        SubscriptionRequestAction::Reject(REQUEST_ERROR_DOES_NOT_EXIST)
    });
    for decision in decisions {
        match decision {
            SubDecision::AcceptLocal { request_id, nt, filter, priority, delivery_timeout } => {
                accept_local_subscription(&mut moq, ad, cid, request_id, &nt, filter, priority, delivery_timeout);
            }
            SubDecision::QueueRelay { request_id, nt, filter, priority, delivery_timeout } => {
                let sub = ad.relay_subscriptions.entry(nt.clone()).or_insert_with(|| {
                    // Prefer a known publisher of this namespace; fall back to
                    // --upstream if unknown. `publisher` may still end up
                    // `None` here if neither is available yet — Phase 4
                    // re-resolves it on every pass once a namespace becomes
                    // known, and gives up (rejecting subscribers) only if
                    // upstream itself doesn't have it either.
                    let pub_id = ad.namespaces.get(nt.namespace()).copied().or(ad.upstream_cid);
                    RelaySubscription {
                        publisher: pub_id.map(|client_id| RelayPublisherInfo { client_id, request_id: None, track_alias: None, largest_location: None, delivery_timeout: None, max_cache_duration: None }),
                        subscribers: Vec::new(),
                        cache: PackedObjectCache::new(ad.cache_depth, ad.cache_max_obj, ad.cache_avg_obj),
                        subgroup_canceled: false,
                    }
                });
                let start_location = match filter {
                    FilterType::NextGroupStart => sub.cache.latest().map(|o: CachedObjectRef| Location { group: o.location().group, object: u64::MAX }),
                    FilterType::LargestObject  => sub.cache.latest().map(|o: CachedObjectRef| o.location()),
                    _ => None,
                };
                sub.subscribers.push(RelaySubscriberInfo { client_id: cid, request_id, subscriber_priority: priority, delivery_timeout, track_alias: None, publisher_gone: false, location: start_location, remaining: 0 });
                info!("conn {cid} queued relay subscription {nt}");
            }
        }
    }
    moq.process_namespace_publish_requests(|_request_id, cm| {
        let ns = cm.track_namespace().clone();
        let want = !ad.sub_passive_wildcards.is_empty()
            || ad.relay_namespace.iter().any(|p| p.matches_namespace(&ns));
        if want {
            ad.namespaces.insert(ns.clone(), cid);
            info!("conn {cid} accept namespace {ns}");
            NamespacePublishRequestAction::Accept
        } else {
            NamespacePublishRequestAction::Keep
        }
    });

    // Lazily init sub tracks for this server connection the first time we see it.
    if !ad.client_sub_tracks.contains_key(&cid) && !ad.sub_track_templates.is_empty() {
        ad.client_sub_tracks.insert(cid, ad.sub_track_templates.iter().map(ClientSubTrack::from_template).collect());
    }

    // Accept incoming PUBLISH if a matching --sub/--sub-passive entry exists,
    // or it matches a --pub-passive namespace/global wildcard.
    accept_or_reject_track_publish(&mut moq, ad, cid);

    // Drain objects from accepted PUBLISH streams.
    drain_client_sub_tracks(&mut moq, ad, cid);
}

fn process_upstream_conn(cid: ClientId, mut moq: MoqHandle, ad: &mut AppData) {
    moq.process_namespace_publish_requests(|_request_id, cm| {
        let ns = cm.track_namespace().clone();
        ad.namespaces.entry(ns.clone()).or_insert(cid);
        info!("conn {cid} upstream announced namespace {ns}");
        NamespacePublishRequestAction::Accept
    });
}

/// Looks up `cid`'s MoQ session handle, if the connection still exists and
/// has completed its WebTransport/MoQ session setup. The single-call
/// replacement for the `conns.get_mut(cid)` + `moq_helper.moq_handle(...)`
/// pair repeated throughout the relay/forwarding code below.
fn moq_handle_for(conns: &mut ConnMap<ConnAppData>, cid: ClientId) -> Option<MoqHandle<'_>> {
    let conn = conns.get_mut(cid)?;
    conn.app_data.moq_helper.moq_handle(&mut conn.conn)
}

// ── relay subscription forwarding ─────────────────────────────────────────────

fn forward_relay_subscription(nt: &NamespaceTrackname, sub: &mut RelaySubscription, conns: &mut ConnMap<ConnAppData>) {
    let Some((pub_id, pub_ta)) = sub.publisher.as_ref().and_then(|p| p.track_alias.map(|ta| (p.client_id, ta))) else { return };
    if !sub.has_accepted_subscribers() { return; }
    loop {
        // If the publisher's subgroup stream was reset/dropped mid-object, the partial payload
        // is incomplete and there's no stream left to finish it. Cancel the cache entry so
        // is_complete() reports true (instead of staying incomplete forever with nothing to read
        // it from) and reset any subscriber that still had unforwarded data for it; those
        // subscribers keep their location so they appear "done" with this object and pick up
        // the next header normally.
        if sub.subgroup_canceled {
            sub.subgroup_canceled = false;
            let canceled_loc = sub.cache.latest().map(|o: CachedObjectRef| o.location());
            if let Some(mut obj) = sub.cache.latest_mut() { obj.cancel(); }
            for s in &mut sub.subscribers {
                if s.location != canceled_loc || s.remaining == 0 { continue; }
                let Some(sub_ta) = s.track_alias else { s.remaining = 0; continue };
                if let Some(mut moq) = moq_handle_for(conns, s.client_id) {
                    error!("reset stream for subscriber {} on {nt}: upstream subgroup canceled", s.client_id);
                    moq.reset_current_track_stream(sub_ta);
                }
                s.remaining = 0;
            }
        }
        if !sub.cache.is_empty() {
            for s in &mut sub.subscribers {
                let Some(sub_ta) = s.track_alias else { continue };
                let Some(mut moq) = moq_handle_for(conns, s.client_id) else { continue };
                if s.remaining > 0 && sub.cache.find(s.location.unwrap()).is_none() {
                    moq.reset_current_track_stream(sub_ta); s.remaining = 0;
                }
                let obj = if s.remaining > 0 { sub.cache.find(s.location.unwrap()) } else { sub.cache.next_after(s.location) };
                let Some(obj) = obj else { continue };
                if s.remaining == 0 {
                    match moq.send_obj_hdr_with(Some(obj.location().group), None, Some(obj.location().object), obj.payload_len(), &obj.ext_hdrs(), sub_ta, Some(obj.received_at())) {
                        Ok(()) => { s.location = Some(obj.location()); s.remaining = obj.payload_len(); }
                        Err(moq::Error::ObjectExpired) => { s.location = Some(obj.location()); s.remaining = 0; continue; }
                        Err(moq::Error::InsufficientCapacity) => continue,
                        Err(e) => { error!("relay send_obj_hdr_with on {nt}: {e:?}"); continue; }
                    }
                }
                let read_pos = obj.payload_len() - s.remaining;
                let available = obj.readable_from(read_pos);
                if !available.is_empty() {
                    if let Ok(n) = moq.send_obj_pld(available, sub_ta) { s.remaining -= n; }
                }
            }
        }
        let mut progress = false;
        if let Some(mut moq) = moq_handle_for(conns, pub_id) {
            let in_progress = sub.cache.latest().is_some_and(|o: CachedObjectRef| !o.is_complete());
            if in_progress {
                let mut obj = sub.cache.latest_mut().unwrap();
                match moq.read_obj_pld(obj.unfilled(), pub_ta) {
                    Ok(n) => { obj.advance_write_pos(n); progress = true; }
                    Err(moq::Error::Done) => {}
                    Err(_) => { sub.subgroup_canceled = true; progress = true; }
                }
            } else {
                match moq.read_obj_hdr(pub_ta) {
                    Ok(hdr) => {
                        let group = moq.subgroup_header(pub_ta)
                            .map_or_else(|| sub.cache.latest().map_or(0, |o: CachedObjectRef| o.location().group), |sg| sg.group_id());
                        let _ = sub.cache.begin(Location { group, object: hdr.id() }, hdr.extension_headers().clone(), hdr.payload_len());
                        progress = true;
                    }
                    Err(moq::Error::Done) => {}
                    Err(moq::Error::Fin) => { sub.publisher = None; for s in &mut sub.subscribers { s.publisher_gone = true; } break; }
                    Err(e) => { error!("relay read_obj_hdr on {nt}: {e:?}"); break; }
                }
            }
        }
        if !progress { break; }
    }
}

// ── local track forwarding ────────────────────────────────────────────────────

fn forward_local_track(track: &mut LocalTrack, conns: &mut ConnMap<ConnAppData>) {
    const BATCH: usize = 4;
    for als in track.aliases.iter_mut() {
        for _ in 0..BATCH {
            if als.remaining > 0 {
                if track.cache.find(als.last_sent.unwrap()).is_none() {
                    if let Some(mut moq) = moq_handle_for(conns, als.conn_id) {
                        moq.reset_current_track_stream(als.alias);
                    }
                    als.remaining = 0;
                } else {
                    let obj = track.cache.find(als.last_sent.unwrap()).unwrap();
                    let read_pos = obj.payload_len() - als.remaining;
                    let available = obj.readable_from(read_pos);
                    if !available.is_empty()
                        && let Some(mut moq) = moq_handle_for(conns, als.conn_id)
                        && let Ok(n) = moq.send_obj_pld(available, als.alias) {
                        als.remaining -= n;
                    }
                    continue;
                }
            }
            let Some(obj) = track.cache.next_after(als.last_sent) else { break };
            let loc = obj.location();
            let received_at = obj.received_at();
            let Some(mut moq) = moq_handle_for(conns, als.conn_id) else { break };
            match moq.send_obj_hdr_with(Some(loc.group), None, Some(loc.object), obj.payload_len(), &obj.ext_hdrs(), als.alias, Some(received_at)) {
                Ok(()) => {}
                Err(moq::Error::UnfinishedPayload) => {
                    moq.reset_current_track_stream(als.alias);
                    let _ = moq.send_obj_hdr_with(Some(loc.group), None, Some(loc.object), obj.payload_len(), &obj.ext_hdrs(), als.alias, Some(received_at));
                }
                Err(moq::Error::ObjectExpired) => { als.last_sent = Some(loc); als.remaining = 0; continue; }
                Err(moq::Error::InsufficientCapacity) => { debug!("backpressure on stream open, retrying"); break; }
                Err(e) => { error!("send_obj_hdr_with: {e:?}"); break; }
            }
            let available = obj.readable_from(0);
            let sent = match moq.send_obj_pld(available, als.alias) {
                Ok(n) => n,
                Err(moq::Error::Done | moq::Error::InsufficientCapacity) => 0,
                Err(e) => { error!("send_obj_pld: {e:?}"); 0 }
            };
            als.last_sent = Some(loc);
            als.remaining = obj.payload_len() - sent;
            if als.remaining > 0 { break; }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscriber(delivery_timeout: Option<u64>) -> RelaySubscriberInfo {
        RelaySubscriberInfo {
            client_id: ClientId::default(),
            request_id: RequestId::default(),
            subscriber_priority: 128,
            delivery_timeout,
            track_alias: None,
            publisher_gone: false,
            location: None,
            remaining: 0,
        }
    }

    #[test]
    fn aggregate_delivery_timeout_empty_is_none() {
        assert_eq!(aggregate_delivery_timeout(&[]), None);
    }

    #[test]
    fn aggregate_delivery_timeout_takes_the_max() {
        let subs = [subscriber(Some(1_000)), subscriber(Some(5_000)), subscriber(Some(2_000))];
        assert_eq!(aggregate_delivery_timeout(&subs), Some(5_000));
    }

    #[test]
    fn aggregate_delivery_timeout_any_none_dominates() {
        // A subscriber with no delivery_timeout wants "never expire," which is
        // more lenient than any finite value, so it should win regardless of
        // the other subscribers' values or their order.
        let subs = [subscriber(Some(5_000)), subscriber(None), subscriber(Some(9_000))];
        assert_eq!(aggregate_delivery_timeout(&subs), None);
    }

    #[test]
    fn aggregate_delivery_timeout_single_subscriber() {
        assert_eq!(aggregate_delivery_timeout(&[subscriber(Some(42))]), Some(42));
        assert_eq!(aggregate_delivery_timeout(&[subscriber(None)]), None);
    }
}
