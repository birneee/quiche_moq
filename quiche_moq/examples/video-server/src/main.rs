extern crate core;
mod moof;
mod mp4_shared_track_state;
mod mp4_track_state;

use crate::mp4_shared_track_state::Mp4SharedTrackState;
use crate::mp4_track_state::Mp4TrackState;
use log::{debug, info};
use mio::unix::pipe::Receiver;
use mio::Interest;
use quiche_mio_runner::quiche_endpoint::quiche::{h3, PROTOCOL_VERSION};
use quiche_mio_runner::quiche_endpoint::{quiche, Conn, EndpointConfig, ServerConfig};
use quiche_h3_utils::{hdrs_to_strings, ALPN_HTTP_3};
use quiche_mio_runner as runner;
use quiche_mio_runner::{quiche_endpoint, Socket};
use quiche_moq as moq;
use quiche_moq::{Config, MoqTransportSession};
use quiche_webtransport as wt;
use std::collections::HashMap;
use std::io;
use std::process::{Command, Stdio};
use boring::ssl::{SslContextBuilder, SslMethod};
use quiche_moq::wire::TrackAlias;
use quiche_utils::cert::load_or_generate_keys;

struct ConnAppData {
    h3_conn: Option<h3::Connection>,
    moq_session: Option<moq::MoqTransportSession>,
    wt_conn: quiche_webtransport::Connection,
    tracks: HashMap<TrackAlias, Mp4TrackState>,
}

impl Default for ConnAppData {
    fn default() -> Self {
        Self {
            h3_conn: None,
            moq_session: None,
            wt_conn: quiche_webtransport::Connection::new(true),
            tracks: Default::default(),
        }
    }
}

struct AppData {
    video_in: Receiver,
    track: Mp4SharedTrackState,
}

type Endpoint = quiche_endpoint::Endpoint<ConnAppData, AppData>;
type Runner = runner::Runner<ConnAppData, AppData, ()>;

fn main() {
    env_logger::builder().format_timestamp_nanos().init();

    let mut ffmpeg_child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-v", "quiet",
            "-re",
            "-f", "lavfi",
            "-i", "testsrc=rate=30:size=1920x1080",
            "-vf", "settb=AVTB,setpts='trunc(PTS/1K)*1K+st(1,trunc(RTCTIME/1K))-1K*trunc(ld(1)/1K)',drawtext=text='%{localtime}.%{eif\\:1M*t-1K*trunc(t*1K)\\:d}:fontcolor=red:fontsize=20'",
            "-c:v", "libx264",
            "-an",
            "-f", "mp4",
            "-movflags", "empty_moov+frag_every_frame+separate_moof+omit_tfhd_offset",
            "-", // write to stdout
        ])
        // .args([
        //     "-hide_banner",
        //     "-v", "quiet",
        //     "-re",
        //     "-f", "lavfi",
        //     "-i", "testsrc=rate=30:size=1920x1080",
        //     "-vf", "settb=AVTB,setpts='trunc(PTS/1K)*1K+st(1,trunc(RTCTIME/1K))-1K*trunc(ld(1)/1K)',drawtext=text='%{localtime}.%{eif\\:1M*t-1K*trunc(t*1K)\\:d}:fontcolor=red:fontsize=20'",
        //     "-c:v", "libx264",
        //     "-an",
        //     "-f", "mpegts",
        //     "-", // write to stdout
        // ])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn().unwrap();

    let video_in = ffmpeg_child
        .stdout
        .take()
        .expect("failed to capture stdout");
    let video_in = mio::unix::pipe::Receiver::from(video_in);
    video_in.set_nonblocking(true).unwrap();

    let endpoint = Endpoint::new(
        Some({
            let mut c = ServerConfig::default();
            c.client_config = {
                let mut c = quiche::Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
                    let (cert, key) = load_or_generate_keys(&None, &None);
                    let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
                    b.set_private_key(&key).unwrap();
                    b.set_certificate(&cert).unwrap();
                    b
                }).unwrap();
                c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
                c.set_initial_max_streams_bidi(100);
                c.set_initial_max_streams_uni(100);
                c.set_initial_max_data(10000000);
                c.set_initial_max_stream_data_bidi_remote(1000000);
                c.set_initial_max_stream_data_bidi_local(1000000);
                c.set_initial_max_stream_data_uni(1000000);
                c.enable_dgram(true, 100, 100);
                c.set_max_idle_timeout(30000);
                c.set_initial_congestion_window_packets(1000);
                c
            };
            c
        }),
        {
            let c = EndpointConfig::default();
            c
        },
        AppData {
            video_in,
            track: Mp4SharedTrackState::new(),
        },
    );

    let socket = Socket::bind("0.0.0.0:8080".parse().unwrap(), false, false, false).unwrap();
    info!("start server on {}", socket.local_addr);

    let mut runner = Runner::new(
        {
            let mut c = runner::Config::default();
            c.post_handle_recvs = post_handle_recvs;
            c
        },
        endpoint,
        None,
    );

    runner.registry.register_external(
        &mut runner.endpoint.app_data_mut().video_in,
        Interest::READABLE,
        (),
    );

    runner.register_socket(socket);

    runner.run();
}

fn post_handle_recvs(runner: &mut Runner) {
    for icid in &mut runner.endpoint.conn_index_iter() {
        let Some(conn) = runner.endpoint.conn_mut(icid) else { continue };
        let quic_conn = &mut conn.conn;
        let h3_conn = match conn.app_data.h3_conn.as_mut() {
            Some(v) => v,
            None => {
                if !quic_conn.is_established() && !quic_conn.is_in_early_data() {
                    continue; // not ready for h3 yet
                }
                assert_eq!(quic_conn.application_proto(), ALPN_HTTP_3);
                conn.app_data.h3_conn = Some(h3::Connection::with_transport(
                    quic_conn,
                    &{
                        let mut c = h3::Config::new().unwrap();
                        wt::configure_h3(&mut c).unwrap();
                        c
                    },
                ).expect("Unable to create HTTP/3 connection, check the server's uni stream limit and window size"));
                conn.app_data.h3_conn.as_mut().unwrap()
            }
        };
        loop {
            match h3_conn.poll(quic_conn) {
                Ok((stream_id, h3::Event::Headers { list, .. })) => {
                    debug!(
                        "h3 stream {} received headers: {:?}",
                        stream_id,
                        hdrs_to_strings(&list)
                    );
                    conn.app_data.wt_conn.recv_hdrs(stream_id, &list);
                }
                Ok(e) => unimplemented!("{:?}", e),
                Err(h3::Error::Done) => break,
                Err(e) => unimplemented!("{:?}", e),
            }
        }
        let wt_conn = &mut conn.app_data.wt_conn;
        wt_conn.poll(h3_conn, quic_conn);

        let moq = match conn.app_data.moq_session.as_mut() {
            Some(moq) => moq,
            None => {
                let session_id = match wt_conn.readable_sessions().first() {
                    None => break,
                    Some(v) => *v,
                };
                conn.app_data.moq_session = Some(MoqTransportSession::accept(session_id.into(), Config::default()));
                conn.app_data.moq_session.as_mut().unwrap()
            }
        };
        moq.poll(quic_conn, h3_conn, wt_conn);
        while let Some(request_id) = moq.next_pending_received_subscription() {
            let sub = moq.pending_received_subscription(request_id);
            assert_eq!(sub.track_namespace, [b"testsrc"]);
            assert_eq!(sub.track_name, b"mp4");
            let track_alias = moq.accept_subscription(quic_conn, wt_conn, request_id);
            conn.app_data
                .tracks
                .insert(track_alias, Mp4TrackState::new(track_alias));
        }
    }
    send_video(runner);
}

// send mp4 boxes to all subscribers
fn send_video(runner: &mut Runner) {
    loop {
        let app_data = runner.endpoint.app_data_mut();
        match app_data
            .track
            .read_next(&mut app_data.video_in)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            Err(e) => unimplemented!("{:?}", e),
        }

        for icid in &mut runner.endpoint.conn_index_iter() {
            let (conn, app_data) = runner.endpoint.conn_with_app_data_mut(icid);
            let Some(conn) = conn else { continue };
            let quic = &mut conn.conn;
            let Some(h3) = conn.app_data.h3_conn.as_mut() else {
                continue;
            };
            let wt = &mut conn.app_data.wt_conn;
            let Some(moq) = conn.app_data.moq_session.as_mut() else {
                continue;
            };
            for ta in moq.writable() {
                let mp4_track = conn.app_data.tracks.get_mut(&ta).unwrap();
                mp4_track.send(&app_data.track, moq, wt, h3, quic)
            }
        }
    }
}
