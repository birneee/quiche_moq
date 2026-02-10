extern crate core;

use crate::args::PublishArgs;
use log::info;
use quiche_mio_runner as runner;
use quiche_mio_runner::quiche_endpoint::quiche::PROTOCOL_VERSION;
use quiche_mio_runner::quiche_endpoint::{EndpointConfig, quiche};
use quiche_mio_runner::{Socket, quiche_endpoint};
use quiche_moq as moq;
use quiche_moq_webtransport_helper::{MoqWebTransportHelper, MoqHandle};
use std::fs;
use url::Url;
use quiche_moq::PublishStatus;
use quiche_moq::wire::NamespaceTrackname;

struct ConnAppData {
    moq_helper: MoqWebTransportHelper,
    namespace_trackname: NamespaceTrackname,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, ()>;
type Runner = runner::Runner<ConnAppData, (), ()>;

#[allow(clippy::field_reassign_with_default)]
pub(crate) fn run_publish(args: &PublishArgs) {
    let mut endpoint = Endpoint::new(None, EndpointConfig::default(), ());

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

    let icid = endpoint.connect(
        None,
        socket.local_addr,
        peer_addr,
        &mut {
            let mut c = quiche::Config::new(PROTOCOL_VERSION).unwrap();
            MoqWebTransportHelper::configure_quic(&mut c);
            c.verify_peer(false);
            c.set_max_idle_timeout(1000);
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

fn post_handle_recvs(r: &mut Runner) {
    for icid in &mut r.endpoint.conn_index_iter() {
        let Some(conn) = r.endpoint.conn_mut(icid) else {
            continue;
        };
        conn.app_data.moq_helper.on_post_handle_recvs(&mut conn.conn);
        let Some(moq) = conn.app_data.moq_helper.moq_handle(&mut conn.conn) else {
            // Not ready yet - verify QUIC connection is healthy
            assert!(!conn.conn.is_timed_out());
            assert!(!conn.conn.is_closed());
            assert!(conn.conn.local_error().is_none());
            assert!(conn.conn.peer_error().is_none());
            continue;
        };
        post_handle_recvs_conn(moq, &conn.app_data.namespace_trackname);
    }
}

fn post_handle_recvs_conn(
    mut moq: MoqHandle,
    namespace_trackname: &NamespaceTrackname,
) {
    match moq.publish_namespace_status(namespace_trackname.namespace()) {
        PublishStatus::Unknown => {
            moq.publish_namespace(namespace_trackname.namespace().0.0.clone())
                .unwrap()
        }
        PublishStatus::Pending => {
            info!("pending")
        }
        PublishStatus::Accepted => {
            info!("accepted")
        }
    }
}
