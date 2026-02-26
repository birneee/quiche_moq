extern crate core;

use crate::args::SubscribeArgs;
use bytes::{BufMut, BytesMut};
use log::{debug, error, info};
use quiche_h3_utils::ALPN_HTTP_3;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{EndpointConfig, quiche};
use quiche_mio_runner::{Socket, quiche_endpoint};
use quiche_moq as moq;
use quiche_moq::wire::object::ObjectHeader;
use quiche_moq::wire::{NamespaceTrackname, RequestId, TrackAlias};
use quiche_moq_webtransport_helper::{MoqHandle, MoqWebTransportHelper};
use std::fs;
use std::fs::File;
use std::io::{Write, stdout};
use url::Url;

struct SubscribeState {
    moq_request_id: Option<RequestId>,
    track_alias: Option<TrackAlias>,
    obj_hdr: Option<ObjectHeader>,
    obj_buf: BytesMut,
    namespace_trackname: NamespaceTrackname,
    output: Option<Box<dyn Write>>,
}

struct ConnAppData {
    client_helper: MoqWebTransportHelper,
    args: SubscribeArgs,
    state: SubscribeState,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, ()>;
type Runner = runner::Runner<ConnAppData, (), ()>;

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn run_subscribe(args: &SubscribeArgs) {
    let mut endpoint = Endpoint::new(None, EndpointConfig::default(), ());

    let socket = Socket::bind("0.0.0.0:0").unwrap();

    let url = Url::parse(&args.url).unwrap();
    let peer_addr = *url.socket_addrs(|| Some(443)).unwrap().first().unwrap();

    let keylog = args.ssl_key_log_file.as_ref().map(|p| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
    });

    let output: Option<Box<dyn Write>> = match &args.output {
        None => Some(Box::new(stdout())),
        Some(o) if o.to_str().unwrap() == "-" => Some(Box::new(stdout())),
        Some(o) => Some(Box::new(File::create(o).unwrap())),
    };

    info!("connect to {}", peer_addr);

    let icid = endpoint.connect(
        None,
        socket.local_addr,
        peer_addr,
        &mut {
            let mut c = quiche::Config::new(PROTOCOL_VERSION).unwrap();
            c.verify_peer(false);
            c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
            MoqWebTransportHelper::configure_quic(&mut c);
            c.set_max_idle_timeout(args.timeout);
            if keylog.is_some() {
                c.log_keys()
            }
            c
        },
        ConnAppData {
            client_helper: MoqWebTransportHelper::new_client(url, {
                let mut c = moq::Config::default();
                c.setup_version = args.setup_version.into();
                c
            }),
            args: args.clone(),
            state: SubscribeState {
                moq_request_id: None,
                track_alias: None,
                obj_hdr: None,
                obj_buf: Default::default(),
                namespace_trackname: args.namespace_trackname.parse().unwrap(),
                output,
            },
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

    runner.register_socket(socket);

    runner.run();
}

fn post_handle_recvs(runner: &mut Runner) {
    for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else {
            continue;
        };
        let quic_conn = &mut conn.conn;
        conn.app_data.client_helper.on_post_handle_recvs(quic_conn);
        let Some(moq) = conn.app_data.client_helper.moq_handle(quic_conn) else {
            continue;
        };
        post_handle_recvs_conn(moq, &conn.app_data.args, &mut conn.app_data.state);
    }
    //todo collect garbage
}

/// handle a single connection after receive
fn post_handle_recvs_conn(mut moq: MoqHandle, args: &SubscribeArgs, state: &mut SubscribeState) {
    while let Some((&request_id, cm)) = moq.next_pending_namespace_publish() {
        let namespace = cm.track_namespace().clone();
        info!("namespace announced: {}", namespace);
        moq.accept_namespace_publish(request_id);
    }
    let request_id = match state.moq_request_id {
        Some(v) => v,
        None => match moq.subscribe(&state.namespace_trackname) {
            Ok(request_id) => {
                state.moq_request_id = Some(request_id);
                info!("request subscribe: {}", state.namespace_trackname);
                request_id
            }
            Err(moq::Error::RequestBlocked) => {
                error!("request blocked");
                return;
            }
            Err(e) => unimplemented!("{:?}", e),
        },
    };
    let track_alias = match state.track_alias {
        Some(v) => v,
        None => {
            match moq.poll_subscribe_response(request_id) {
                Some(Ok((track_alias, _cm))) => {
                    info!("subscribe accepted: {}", state.namespace_trackname);
                    state.track_alias = Some(track_alias);
                    track_alias
                }
                Some(Err(e)) => {
                    error!(
                        "subscribe {} rejected with {} - {}",
                        state.namespace_trackname,
                        e.error_code(),
                        e.error_reason(),
                    );
                    return;
                },
                None => return, // no answer yet
            }
        }
    };
    let obj_hdr = match &state.obj_hdr {
        Some(v) => v,
        None => match moq.read_obj_hdr(track_alias) {
            Ok(obj_hdr) => {
                debug!("{:?}", obj_hdr);
                state.obj_hdr = Some(obj_hdr);
                state.obj_hdr.as_ref().unwrap()
            }
            Err(moq::Error::Done) => return,
            Err(e) => unimplemented!("{:?}", e),
        },
    };
    loop {
        if state.obj_buf.len() >= obj_hdr.payload_len() {
            break;
        }
        let buf = &mut state.obj_buf;
        buf.reserve(obj_hdr.payload_len());
        let chunk = buf.chunk_mut();
        let slice = unsafe { &mut *(chunk as *mut _ as *mut [u8]) };
        let n = match moq.read_obj_pld(slice, track_alias) {
            Ok(v) => v,
            Err(moq::Error::Done) => return,
            Err(e) => unimplemented!("{:?}", e),
        };
        unsafe { buf.advance_mut(n) }
    }
    debug!("finish obj");
    if !args.separator.is_empty() {
        let separator = args.separator.as_bytes();
        state.obj_buf.extend_from_slice(separator);
    }
    if let Some(output) = &mut state.output {
        output.write_all(state.obj_buf.as_ref()).unwrap();
    }
    state.obj_hdr = None;
    state.obj_buf.clear();
}
