//! From arguments to a running `Runner`.

use super::*;

pub fn run(args: Args) {
    let r = build_runner(&args);
    finish_run(r);
}

/// What the arguments set up before any connection: tracks, namespaces,
/// file inputs and outputs.
pub(super) struct ArgSetup {
    local_tracks: HashMap<NamespaceTrackname, LocalTrack>,
    local_namespaces: HashSet<Namespace>,
    active_pub_tracks: Vec<NamespaceTrackname>,
    file_inputs: Vec<FileInputState>,
    file_input_fds: Vec<RawFd>,
    file_output_fds: Vec<RawFd>,
    pub_passive_wildcards: Vec<TrackPattern>,
    sub_passive_wildcards: Vec<TrackPattern>,
}

pub(super) fn setup_from_args(args: &Args) -> ArgSetup {
    let mut local_tracks: HashMap<NamespaceTrackname, LocalTrack> = HashMap::new();
    let mut local_namespaces: HashSet<Namespace> = HashSet::new();
    let mut active_pub_tracks: Vec<NamespaceTrackname> = Vec::new();
    let mut file_inputs: Vec<FileInputState> = Vec::new();
    let mut file_input_fds: Vec<RawFd> = Vec::new();
    let mut pub_passive_wildcards: Vec<TrackPattern> = Vec::new();

    // --pub: active PUBLISH (ns--track), announce (namespace only), or a
    // passive standby track buffer served on SUBSCRIBE — reads only from the
    // local object cache (see --in below for feeding it from a file).
    // Namespace/global wildcard entries carry no fixed track and instead
    // become a policy (any incoming PUBLISH push for a matching track is
    // dynamically accepted and re-served — see
    // `answer_publish`; a SUBSCRIBE for it only succeeds
    // once that's happened, since that's what actually populates
    // `local_tracks`). Namespace announcement is a separate, explicit
    // concern (--pub <NS>) — serving a track here doesn't imply announcing
    // its namespace.
    for pa in &args.publish {
        match &pa.target {
            PubTarget::Namespace(ns) => { local_namespaces.insert(ns.clone()); }
            PubTarget::Track(pattern) => {
                let Some(nt) = pattern.as_exact() else {
                    pub_passive_wildcards.push(pattern.clone());
                    continue;
                };
                local_tracks.entry(nt.clone())
                    .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj, pa.max_cache_duration))
                    .datagram |= pa.datagram;
                if !pa.passive { active_pub_tracks.push(nt.clone()); }
            }
        }
    }

    // --in: feeds a track's local object cache from a file, independent of
    // whether it's also --pub'd. No #file= means stdin.
    for ia in &args.r#in {
        local_tracks.entry(ia.track.clone())
            .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj, None));
        let path = ia.file.as_deref().unwrap_or(std::path::Path::new("-"));
        let file = open_input(path);
        let fd = file.as_raw_fd();
        if is_epoll_compatible(&file) { file_input_fds.push(fd); }
        file_inputs.push(FileInputState::new(file, ia.track.clone(), ia.delimiter));
    }

    // --sub: only wildcard patterns need registering here (a relay-forward
    // policy). Concrete patterns are handled by sub_track_templates below,
    // which creates the local_tracks entry itself once a candidate
    // connection is known.
    let mut sub_passive_wildcards: Vec<TrackPattern> = Vec::new();
    for sub_arg in &args.subscribe {
        if sub_arg.track.as_exact().is_none() {
            sub_passive_wildcards.push(sub_arg.track.clone());
        }
    }

    // --out: drains a track's local object cache to a file, independent of
    // whether it's also --sub'd. No #file= means stdout.
    let mut file_output_fds: Vec<RawFd> = Vec::new();
    for oa in &args.out {
        let track = local_tracks.entry(oa.track.clone())
            .or_insert_with(|| LocalTrack::new(args.cache_depth, args.cache_max_obj, args.cache_avg_obj, None));
        let path = oa.file.as_deref().unwrap_or(std::path::Path::new("-"));
        let file = open_output(path);
        let fd = file.as_raw_fd();
        if is_epoll_compatible(&file) { file_output_fds.push(fd); }
        track.file_outputs.push(FileOutputState::new(file, oa.output_kind.clone()));
    }
    ArgSetup {
        local_tracks, local_namespaces, active_pub_tracks, file_inputs, file_input_fds, file_output_fds,
        pub_passive_wildcards, sub_passive_wildcards,
    }
}

pub(super) fn build_runner(args: &Args) -> Runner {
    let server_socket = args.listen.as_ref().map(|l| Socket::bind(format!("0.0.0.0:{}", l.port)).unwrap());
    let client_socket = server_socket.is_none().then(|| Socket::bind("0.0.0.0:0").unwrap());
    let local_addr = server_socket.as_ref().or(client_socket.as_ref()).unwrap().local_addr;

    if let Some(l) = &args.listen { info!("listening on 0.0.0.0:{}", l.port); }

    let ArgSetup {
        local_tracks, local_namespaces, active_pub_tracks, file_inputs, file_input_fds, file_output_fds,
        pub_passive_wildcards, sub_passive_wildcards,
    } = setup_from_args(args);

    let endpoint = Endpoint::new(
        args.listen.as_ref().map(|_| {
            let (cert, key) = load_or_generate_keys(&args.cert, &args.key);
            let moq_cfg = make_moq_config(args);
            let native = args.listen.as_ref().is_some_and(|l| l.native);
            let mut quic_cfg = quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, {
                let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                b.set_private_key(&key).unwrap();
                b.set_certificate(&cert).unwrap();
                b
            }).unwrap();
            configure_quic(&mut quic_cfg, args, args.listen_quic());
            MoqWebTransportHelper::set_application_protos(&mut quic_cfg, &moq_cfg, true, native)
                .unwrap_or_else(|e| panic!("--listen: {e}, see --setup-version"));
            let mut c = ServerConfig::new(move |_| {
                let helper = MoqWebTransportHelper::new_server(moq_cfg.clone());
                ConnAppData::new(if native { helper.with_native_quic() } else { helper }, None)
            });
            c.client_config = quic_cfg;
            c.drop_new_paths = args.no_migration;
            c
        }),
        EndpointConfig { setup_qlog, ..EndpointConfig::default() },
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
            rendezvous: Vec::new(),
            fetches: Vec::new(),
            upstream_cids: HashSet::new(),
            listen_upstream: args.listen.as_ref().is_some_and(|l| l.upstream),
            active_pub_tracks,
            sub_track_templates: args.subscribe.iter()
                .filter_map(|a| a.track.as_exact().map(|nt| SubTrackTemplate { track: nt.clone(), passive: a.passive, multi: a.multi, race: a.race, race_mode: a.race_mode, options: a.options }))
                .collect(),
            client_active_pub_tracks: HashMap::new(),
            shutdown_requested: false,
            usr1_requested: false,
            usr2_requested: false,
            sigusr1_actions: args.sigusr1.clone(),
            sigusr2_actions: args.sigusr2.clone(),
            padding_stream_connect_only: args.padding_stream_connect_only,
            prewarm: args.prewarm,
            unpub_namespaces: args.unpub.clone(),
            sigusr1_source: None,
            sigusr2_source: None,
            embed_timestamps: args.timestamp,
            qlog_dir: args.qlog_dir.clone(),
            qlog_level: args.qlog_level,
            reconnect: args.reconnect.then(|| (args.clone(), local_addr)),
            pending_reconnects: Vec::new(),
            #[cfg(test)]
            status: Default::default(),
        },
    );

    let mut r = Runner::new(
        runner::Config::<ConnAppData, AppData, i32> {
            post_handle_recvs,
            on_external_event: Some(on_file_event),
            ..Default::default()
        },
        endpoint,
    );

    for conn in &args.connect {
        if conn.upstream {
            connect_upstream(&mut r, args, local_addr, &conn.url).unwrap_or_else(|e| panic!("{e}"));
        } else {
            info!("connecting to {}", conn.url);
            let (url, peer_addr) = resolve_peer(&conn.url, local_addr).unwrap_or_else(|e| panic!("{e}"));
            connect_outbound(&mut r, args, local_addr, url, peer_addr).unwrap_or_else(|e| panic!("{e}"));
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
    let mut sigusr2 = SignalSource::new(&[libc::SIGUSR2]);
    r.registry().register_external(&mut sigusr2, Interest::READABLE, USR2_EVENT);
    r.endpoint.app_data_mut().sigusr2_source = Some(sigusr2);
    r
}

pub(super) fn finish_run(mut r: Runner) {
    r.run();
}

pub(super) fn open_input(path: &std::path::Path) -> File {
    use std::os::unix::fs::OpenOptionsExt;
    let path = if path.as_os_str() == "-" { std::path::Path::new("/dev/stdin") } else { path };
    std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK)
        .open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"))
}

pub(super) fn open_output(path: &std::path::Path) -> File {
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

pub(super) fn is_epoll_compatible(file: &File) -> bool {
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
pub(super) fn make_keepalive(args: &Args) -> Option<KeepaliveHandler> {
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
pub(super) fn setup_qlog(conn: &mut quiche::Connection, role: &str, scid: &quiche::ConnectionId, app_data: &AppData) {
    let Some(dir) = app_data.qlog_dir.as_ref() else { return };
    let id = format!("{:?}", scid);
    let writer = make_qlog_writer(dir.as_os_str(), role, &id);
    // set_qlog_with_level_and_name_filter, not set_qlog: the latter pins
    // quiche's default (Base), which includes `stream_data_moved` —
    // thousands of events per connection per second at this testbed's
    // object rates, and serializing them dominates the writer's CPU.
    conn.set_qlog_with_level_and_name_filter(
        Box::new(writer),
        format!("quiche-{role} qlog"),
        format!("quiche-{role} qlog id={id}"),
        app_data.qlog_level.level(),
        app_data.qlog_level.name_filter(),
    );
}

/// Opens the keylog file configured via `--ssl-key-log-file` / `SSLKEYLOGFILE`,
/// for an outbound (--connect / #upstream) connection. Same as the `moq_utils`
/// pattern: `cfg.log_keys()` must be called on the `quiche::Config` *before*
/// `connect()`, and `conn.set_keylog(...)` *after* it returns (connect() never
/// processes received bytes itself, so no keys are derived in between).
pub(super) fn open_keylog(args: &Args) -> Option<File> {
    let path = args.ssl_key_log_file.as_ref()?;
    Some(std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap())
}

/// The `moq::Config` of every connection, restricted to `--setup-version` if set.
pub(super) fn make_moq_config(args: &Args) -> moq::Config {
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
    cfg.padding_stream_max_bytes_per_sec = args.padding_stream_rate;
    if let Some(max_request_id) = args.max_request_id {
        cfg.out_max_request_id = max_request_id;
    }
    cfg
}

/// Opens an outbound QUIC connection to `url_str`, shared by every
/// `--connect` target (which differ only in whether the caller adds the
/// resulting `ClientId` to `AppData::upstream_cids`).
/// - a moqt:// URL runs the session on native QUIC (draft-ietf-moq-transport-18 §3.1.4), an https one on WebTransport
pub(super) fn connect_outbound(r: &mut Runner, args: &Args, local_addr: std::net::SocketAddr, url: Url, peer_addr: std::net::SocketAddr) -> Result<ClientId, String> {
    let keylog = open_keylog(args);
    let moq_config = make_moq_config(args);
    let native_quic = url.scheme() == "moqt";
    let mut cfg = quiche::Config::new(quiche::PROTOCOL_VERSION).unwrap();
    configure_quic(&mut cfg, args, args.connect_quic());
    MoqWebTransportHelper::set_application_protos(&mut cfg, &moq_config, !native_quic, native_quic)
        .map_err(|e| format!("{url}: {e}, see --setup-version"))?;
    cfg.verify_peer(false);
    if keylog.is_some() { cfg.log_keys(); }
    let cid = r.endpoint.connect(None, local_addr, peer_addr, &mut cfg,
        ConnAppData::new(MoqWebTransportHelper::new_client(url, moq_config), make_keepalive(args)),
        None, None);
    if let Some(keylog) = keylog {
        r.endpoint.conn_mut(cid).unwrap().conn.set_keylog(Box::new(keylog));
    }
    Ok(cid)
}

/// QUIC settings both sides share, plus `settings`.
pub(super) fn configure_quic(cfg: &mut quiche::Config, args: &Args, settings: QuicSettings) {
    MoqWebTransportHelper::configure_quic(cfg);
    cfg.set_max_idle_timeout(args.timeout);
    if let Some(packets) = settings.init_cwnd { cfg.set_initial_congestion_window_packets(packets); }
    if let Some(v) = settings.max_pacing_rate { cfg.set_max_pacing_rate(v); }
    if let Some(algo) = settings.cc_algorithm { cfg.set_cc_algorithm(algo.into()); }
    if let Some(v) = settings.max_data { cfg.set_initial_max_data(v); }
    if let Some(v) = settings.max_stream_data {
        cfg.set_initial_max_stream_data_bidi_remote(v);
        cfg.set_initial_max_stream_data_bidi_local(v);
        cfg.set_initial_max_stream_data_uni(v);
    }
    if let Some(v) = settings.max_streams_uni { cfg.set_initial_max_streams_uni(v); }
    if let Some(v) = settings.max_streams_bidi { cfg.set_initial_max_streams_bidi(v); }
    if let Some(v) = args.active_conn_id_limit { cfg.set_active_connection_id_limit(v); }
    if let Some(v) = args.max_cwnd { cfg.set_max_congestion_window_packets(v); }
}

/// `url_str` and its address of `local_addr`'s IP version, else any.
pub(super) fn resolve_peer(url_str: &str, local_addr: std::net::SocketAddr) -> Result<(Url, std::net::SocketAddr), String> {
    let url = Url::parse(url_str).map_err(|e| format!("invalid URL {url_str}: {e}"))?;
    let addrs = url.socket_addrs(|| Some(443)).map_err(|e| format!("cannot resolve {url_str}: {e}"))?;
    let peer_addr = addrs.iter().find(|a| a.is_ipv4() == local_addr.is_ipv4()).or(addrs.first())
        .copied()
        .ok_or_else(|| format!("{url_str} resolved to no address"))?;
    Ok((url, peer_addr))
}

/// Outbound `#upstream` connection: a `--sub` candidate, re-dialed on close if `--reconnect`.
pub(super) fn connect_upstream(r: &mut Runner, args: &Args, local_addr: std::net::SocketAddr, url: &str) -> Result<(), String> {
    let (resolved, peer_addr) = resolve_peer(url, local_addr)?;
    info!("connecting to upstream {url}");
    let cid = connect_outbound(r, args, local_addr, resolved, peer_addr)?;
    if args.reconnect {
        r.endpoint.conn_mut(cid).unwrap().app_data.reconnect_url = Some(url.to_owned());
    }
    let ad = r.endpoint.app_data_mut();
    ad.upstream_cids.insert(cid);
    sync_sub_track_templates(ad);
    Ok(())
}

/// Spawn the node in a background thread, returns once its sockets are bound.
/// Drop the returned `StopHandle` to shut it down.
/// The `JoinHandle` can be used to wait for the thread to exit after stopping.
#[cfg(test)]
pub fn run_stoppable(args: Args) -> (std::thread::JoinHandle<()>, StopHandle) {
    let (stop_r, stop_w) = mio::net::UnixStream::pair().unwrap();
    let (bound_tx, bound_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        // like run(), with the stop socket as extra close source
        run_with_extra_close(args, stop_r, bound_tx);
    });
    // a node that panics while building drops the sender, its join reports the panic
    let status = bound_rx.recv().unwrap_or_default();
    (handle, StopHandle { _stop: stop_w, status })
}

#[cfg(test)]
pub struct StopHandle {
    _stop: mio::net::UnixStream,
    status: std::sync::Arc<std::sync::Mutex<super::NodeStatus>>,
}

#[cfg(test)]
impl StopHandle {
    /// Waits until the node's subscription to `track` is accepted.
    pub fn wait_subscribed(&self, track: &NamespaceTrackname) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.status.lock().unwrap().subscribed.contains(track) {
            assert!(Instant::now() < deadline, "no subscription to {track} within 5s");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
pub(super) fn run_with_extra_close(args: Args, extra: mio::net::UnixStream, bound: std::sync::mpsc::Sender<std::sync::Arc<std::sync::Mutex<super::NodeStatus>>>) {
    let mut r = build_runner(&args);
    let _ = bound.send(r.endpoint.app_data().status.clone());
    // Same graceful path as a real SIGINT, so tests exercise the actual shutdown logic.
    r.registry().register_external_owned(extra, Interest::READABLE, SHUTDOWN_EVENT);
    r.run();
}

pub(super) fn on_file_event(endpoint: &mut Endpoint<ConnAppData, AppData>, fd: &i32) {
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
    if *fd == USR2_EVENT {
        let ad = endpoint.app_data_mut();
        if let Some(ref source) = ad.sigusr2_source { source.drain(); }
        ad.usr2_requested = true;
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
