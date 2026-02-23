use crate::args::PublishArgs;
use log::{error, info};
use partial_borrow::{SplitOff, prelude::*};
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{EndpointConfig, quiche};
use quiche_mio_runner::{Socket, quiche_endpoint};
use quiche_moq as moq;
use quiche_moq::wire::{KeyValuePair, KeyValuePairs, Location, NamespaceTrackname, REQUEST_ERROR_DOES_NOT_EXIST, TrackAlias, version_to_name};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use quiche_moq::PublishStatus;
use std::fs;
use std::io;
use std::os::unix::io::AsRawFd;
use url::Url;

struct AppData {
    args: PublishArgs,
}

#[derive(PartialBorrow)]
struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
    namespace_trackname: NamespaceTrackname,
    announced: bool,
    logged_connect: bool,
    track_aliases: Vec<TrackAlias>,
    input_fd: i32,
    line_buf: Vec<u8>,
    /// Number of objects consumed from input so far (even before any subscriber joined).
    next_object_id: u64,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn run_publish(args: &PublishArgs) {
    let mut endpoint = Endpoint::new(None, EndpointConfig::default(), AppData { args: args.clone(), });

    let socket = Socket::bind("127.0.0.1:0").unwrap();

    let url = Url::parse(&args.url).unwrap();
    let peer_addr = *url.socket_addrs(|| Some(443)).unwrap().first().unwrap();

    let keylog = args.ssl_key_log_file.as_ref().map(|p| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
    });

    info!("connect to {}", peer_addr);

    // Determine input fd: default to stdin
    let input_fd = match &args.input {
        None => io::stdin().as_raw_fd(),
        Some(p) if p.to_str().unwrap() == "-" => io::stdin().as_raw_fd(),
        Some(p) => {
            use std::os::unix::io::AsRawFd;
            let f = std::fs::File::open(p).unwrap();
            f.as_raw_fd()
        }
    };

    // Set input fd to non-blocking
    unsafe {
        let flags = libc::fcntl(input_fd, libc::F_GETFL);
        libc::fcntl(input_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    let icid = endpoint.connect(
        None,
        socket.local_addr,
        peer_addr,
        &mut {
            let mut c = quiche::Config::new(PROTOCOL_VERSION).unwrap();
            MoqWebTransportHelper::configure_quic(&mut c);
            c.verify_peer(false);
            if keylog.is_some() {
                c.log_keys()
            }
            c
        },
        ConnAppData {
            moq_helper: MoqWebTransportHelper::new_client(
                args.url.parse().unwrap(),
                moq::Config::default(),
            ),
            namespace_trackname: args.namespace_trackname.parse().unwrap(),
            announced: false,
            logged_connect: false,
            track_aliases: Vec::new(),
            input_fd,
            line_buf: Vec::new(),
            next_object_id: 0,
        },
        None,
        None,
    );

    if let Some(keylog) = keylog {
        endpoint
            .conn_mut(icid)
            .unwrap()
            .conn
            .set_keylog(Box::new(keylog));
    }

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        endpoint,
        None,
    );

    // Register stdin with mio so poll wakes up when stdin has data
    runner.registry().register_external(
        &mut mio::unix::SourceFd(&input_fd),
        mio::Interest::READABLE,
        (),
    );

    runner.register_socket(socket);

    runner.run();
}

fn post_handle_recvs(r: &mut Runner) {
    for icid in &mut r.endpoint.conn_index_iter() {
        let (Some(conn), appdata) = r.endpoint.conn_with_app_data_mut(icid) else {
            continue;
        };
        let (ad1, ad2) = SplitOff::<partial!(ConnAppData mut moq_helper, ! *)>::split_off_mut(&mut conn.app_data);
        ad1.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(mut moq) = ad1.moq_helper.moq_handle(&mut conn.conn) else {
            assert!(!conn.conn.is_timed_out());
            assert!(!conn.conn.is_closed());
            assert!(conn.conn.local_error().is_none());
            assert!(conn.conn.peer_error().is_none());
            continue;
        };
        // log connection
        if let Some(version) = moq.version() && !*ad2.logged_connect {
            let peer_addr = moq.quic().path_stats().next().map(|s| s.peer_addr).unwrap();
            info!("Server connected {peer_addr:?} v{}", version_to_name(version));
            *ad2.logged_connect = true;
        }
        post_handle_recvs_conn(
            moq,
            ad2,
            appdata,
        );
    }
}

fn post_handle_recvs_conn(
    mut moq: MoqHandle,
    conn_app_data: &mut partial!(ConnAppData mut *, ! moq_helper),
    app_data: &AppData,
) {
    // Handle namespace publishing
    match moq.publish_namespace_status(conn_app_data.namespace_trackname.namespace()) {
        PublishStatus::Unknown => {
            moq.publish_namespace(conn_app_data.namespace_trackname.namespace().0.0.clone())
                .unwrap();
            info!("publishing namespace {}", conn_app_data.namespace_trackname.namespace());
        }
        PublishStatus::Pending => {}
        PublishStatus::Accepted => {
            if !*conn_app_data.announced {
                info!("announced namespace {} successfully", conn_app_data.namespace_trackname.namespace());
                *conn_app_data.announced = true;
            }
        }
    }

    // Handle incoming subscriptions
    loop {
        let Some((request_id, subscription)) = moq.subscription_inbox_next() else {
            break;
        };
        let request_id = *request_id;
        if subscription.namespace_trackname != *conn_app_data.namespace_trackname {
            info!("rejecting subscription to unknown track: {}", subscription.namespace_trackname);
            moq.reject_subscription(request_id, REQUEST_ERROR_DOES_NOT_EXIST);
            continue;
        }
        info!("accepting subscription to {}", *conn_app_data.namespace_trackname);
        let largest = (*conn_app_data.next_object_id > 0).then(|| Location { group: 0, object: *conn_app_data.next_object_id - 1 });
        let track_alias = moq.accept_subscription(request_id, largest);
        conn_app_data.track_aliases.push(track_alias);
    }

    // Read available input data (non-blocking, drain until EAGAIN)
    let mut tmp = [0u8; 4096];
    loop {
        let n = unsafe {
            libc::read(*conn_app_data.input_fd, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len())
        };
        if n > 0 {
            conn_app_data.line_buf.extend_from_slice(&tmp[..n as usize]);
        } else {
            break;
        }
    }

    let sep_bytes = app_data.args.separator.as_bytes();
    assert!(!sep_bytes.is_empty());
    // Separate input and send each as an object
    while !conn_app_data.line_buf.is_empty() {
        let pos = conn_app_data.line_buf.windows(sep_bytes.len()).position(|w| w == sep_bytes);
        let Some(pos) = pos else { break; };
        let mut payload: Vec<u8> = conn_app_data.line_buf.drain(..pos + sep_bytes.len()).collect();
        payload.truncate(pos);
        if payload.is_empty() {
            continue;
        }
        let obj_id = *conn_app_data.next_object_id;
        *conn_app_data.next_object_id += 1;
        let extension_headers = if app_data.args.timestamp {
            let micros = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros() as u64;
            // 0x4000 is the first type ID in the first-come-first-served range (draft-16 §13)
            KeyValuePairs::from(vec![KeyValuePair::new_varint(0x4000, micros).unwrap()])
        } else {
            KeyValuePairs::new()
        };
        for &track_alias in conn_app_data.track_aliases.iter() {
            if let Err(e) = moq.send_obj_hdr_with(Some(0), None, Some(obj_id), payload.len(), &extension_headers, track_alias) {
                error!("send obj hdr error: {:?}", e);
                continue;
            }
            if let Err(e) = moq.send_obj_pld(payload.as_slice(), track_alias) {
                error!("send obj pld error: {:?}", e);
            }
        }
    }
}
