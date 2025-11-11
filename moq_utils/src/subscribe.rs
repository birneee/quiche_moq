extern crate core;

use crate::args::SubscribeArgs;
use crate::h3::ALPN_HTTP_3;
use bytes::{BufMut, BytesMut};
use log::{debug, error, info};
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{EndpointConfig, quiche};
use quiche_mio_runner::{Socket, quiche_endpoint};
use quiche_moq as moq;
use quiche_moq::wire::object::ObjectHeader;
use quiche_moq::wire::{RequestId, TrackAlias};
use std::fs;
use std::fs::File;
use std::io::Write;
use url::Url;
use quiche_moq::client_helper::{ClientHelper, State};

struct ConnAppData {
    client_helper: ClientHelper,
    args: SubscribeArgs,
    moq_request_id: Option<RequestId>,
    track_alias: Option<TrackAlias>,
    obj_hdr: Option<ObjectHeader>,
    obj_buf: BytesMut,
    output: Option<File>,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, ()>;
type Runner = runner::Runner<ConnAppData, (), ()>;

pub(crate) fn run_subscribe(args: &SubscribeArgs) {
    let mut endpoint = Endpoint::new(
        None,
        {
            let c = EndpointConfig::default();
            c
        },
        (),
    );

    let socket = Socket::bind("0.0.0.0:0".parse().unwrap(), false, false, false).unwrap();

    let url = Url::parse(&args.url).unwrap();
    let peer_addr = url
        .socket_addrs(|| Some(443))
        .unwrap()
        .first()
        .unwrap()
        .clone();

    let keylog = args.ssl_key_log_file.as_ref().map(|p| {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap()
    });

    let output = match &args.output {
        Some(o) if o.to_str().unwrap() == "-" => Some(File::create("/dev/stdout").unwrap()),
        Some(o) => Some(File::create(&o).unwrap()),
        None => None,
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
            ClientHelper::configure_quic(&mut c);
            if keylog.is_some() {
                c.log_keys()
            }
            c
        },
        ConnAppData {
            client_helper: ClientHelper::new(url, args.setup_version.into()),
            args: args.clone(),
            moq_request_id: None,
            track_alias: None,
            obj_hdr: None,
            obj_buf: Default::default(),
            output,
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
    'connLoop: for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else {
            continue 'connLoop;
        };
        let quic_conn = &mut conn.conn;
        conn.app_data.client_helper.on_post_handle_recvs(quic_conn);
        let State::Moq {
            h3_conn,
            wt_conn,
            moq_session,
            ..
        } = &mut conn.app_data.client_helper.state
        else {
            continue 'connLoop;
        };
        if !moq_session.initialized() {
            continue 'connLoop;
        }
        let request_id = match conn.app_data.moq_request_id {
            Some(v) => v,
            None => {
                match moq_session.subscribe(
                    quic_conn,
                    wt_conn,
                    vec![conn.app_data.args.namespace.as_bytes().to_vec()],
                    conn.app_data.args.trackname.as_bytes().to_vec(),
                ) {
                    Ok(request_id) => {
                        conn.app_data.moq_request_id = Some(request_id);
                        request_id
                    }
                    Err(moq::Error::RequestBlocked) => {
                        error!("request blocked");
                        continue;
                    }
                    Err(e) => unimplemented!("{:?}", e),
                }
            }
        };
        let track_alias = match conn.app_data.track_alias {
            Some(v) => v,
            None => {
                match moq_session.poll_subscribe_response(request_id) {
                    Some(Ok((track_alias, _cm))) => {
                        info!(
                            "subscribed to: {} {}",
                            conn.app_data.args.namespace, conn.app_data.args.trackname
                        );
                        conn.app_data.track_alias = Some(track_alias);
                        track_alias
                    }
                    Some(Err(e)) => unimplemented!("{:?}", e),
                    None => continue, // no answer yet
                }
            }
        };
        let obj_hdr = match &conn.app_data.obj_hdr {
            Some(v) => v,
            None => match moq_session.read_obj_hdr(track_alias, wt_conn, h3_conn, quic_conn) {
                Ok(obj_hdr) => {
                    debug!("{:?}", obj_hdr);
                    conn.app_data.obj_hdr = Some(obj_hdr);
                    conn.app_data.obj_hdr.as_ref().unwrap()
                }
                Err(moq::Error::Done) => continue,
                Err(e) => unimplemented!("{:?}", e),
            },
        };
        while conn.app_data.obj_buf.len() < obj_hdr.payload_len() {
            let buf = &mut conn.app_data.obj_buf;
            buf.reserve(obj_hdr.payload_len());
            let chunk = buf.chunk_mut();
            let slice = unsafe { &mut *(chunk as *mut _ as *mut [u8]) };
            let n = match moq_session.read_obj_pld(slice, track_alias, wt_conn, h3_conn, quic_conn)
            {
                Ok(v) => v,
                Err(moq::Error::Done) => continue 'connLoop,
                Err(e) => unimplemented!("{:?}", e),
            };
            unsafe { buf.advance_mut(n) }
        }
        debug!("finish obj");
        if !conn.app_data.args.separator.is_empty() {
            let separator = conn.app_data.args.separator.as_bytes();
            conn.app_data.obj_buf.extend_from_slice(separator);
        }
        if let Some(output) = &mut conn.app_data.output {
            output.write_all(conn.app_data.obj_buf.as_ref()).unwrap();
        }
        conn.app_data.obj_hdr = None;
        conn.app_data.obj_buf.clear();
    }
    //todo collect garbage
}
