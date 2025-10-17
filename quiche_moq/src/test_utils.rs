use crate::{Config, MoqTransportSession};
use quiche::h3;
use quiche::test_utils::Pipe;
use quiche_webtransport as wt;
use quiche_webtransport::test_utils::_init_webtransport_pipe;

pub fn _init_moq_pipe(config: Config) -> (
    Pipe,
    h3::Connection,
    wt::Connection,
    MoqTransportSession,
    h3::Connection,
    wt::Connection,
    MoqTransportSession,
) {
    let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

    let mut c_moq = MoqTransportSession::connect(
        wt_session_id.into(),
        &mut c_h3,
        &mut pipe.client,
        &mut c_wt,
        config,
    );

    pipe.advance().unwrap();

    assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
    s_wt.poll(&mut s_h3, &mut pipe.server);
    let session_id = *s_wt.readable_sessions().first().unwrap();
    let mut s_moq = MoqTransportSession::accept(session_id.into(), config);
    s_moq.poll(&mut pipe.server, &mut s_h3, &mut s_wt);
    assert!(s_moq.initialized());

    pipe.advance().unwrap();

    c_wt.poll(&mut c_h3, &mut pipe.client);
    c_moq.poll(&mut pipe.client, &mut c_h3, &mut c_wt);
    assert!(c_moq.initialized());

    (pipe, c_h3, c_wt, c_moq, s_h3, s_wt, s_moq)
}
