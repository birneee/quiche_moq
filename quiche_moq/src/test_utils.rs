use crate::{Config, MoqHandle, MoqTransportSession};
use quiche::h3;
use quiche::test_utils::Pipe;
use quiche_webtransport as wt;
use quiche_webtransport::test_utils::_init_webtransport_pipe;

pub struct MoqPipe {
    pub pipe: Pipe,
    c_h3: h3::Connection,
    c_wt: wt::Connection,
    c_moq: MoqTransportSession,
    s_h3: h3::Connection,
    s_wt: wt::Connection,
    s_moq: MoqTransportSession,
}

impl MoqPipe {
    pub fn client(&mut self) -> MoqHandle<'_> {
        MoqHandle::new(&mut self.c_moq, &mut self.pipe.client, &mut self.c_h3, &mut self.c_wt)
    }

    pub fn server(&mut self) -> MoqHandle<'_> {
        MoqHandle::new(&mut self.s_moq, &mut self.pipe.server, &mut self.s_h3, &mut self.s_wt)
    }

    pub fn advance(&mut self) -> Result<(), quiche::Error> {
        // Flush state set by app logic (or a prior handle) before the next handle().
        self.client().post_handle();
        self.server().post_handle();
        self.pipe.advance()?;
        self.client().poll_all();
        self.server().poll_all();
        // dirty=true after poll_all; caller does app logic then calls post_handle.
        Ok(())
    }
}

pub fn init_moq_pipe(config: Config) -> MoqPipe {
    let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, wt_session_id) = _init_webtransport_pipe();

    let mut c_moq = MoqTransportSession::connect(
        wt_session_id.into(),
        &mut c_h3,
        &mut pipe.client,
        &mut c_wt,
        config.clone(),
    ).unwrap();

    pipe.advance().unwrap();

    assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
    s_wt.poll(&mut s_h3, &mut pipe.server);
    let session_id = *s_wt.readable_sessions().first().unwrap();
    let mut s_moq = MoqTransportSession::accept(session_id.into(), config.clone()).unwrap();
    s_moq.handle(&mut s_wt, &mut s_h3, &mut pipe.server);
    s_moq.post_handle(&mut pipe.server, &mut s_h3, &mut s_wt);
    assert!(s_moq.initialized());

    pipe.advance().unwrap();

    c_wt.poll(&mut c_h3, &mut pipe.client);
    c_moq.handle(&mut c_wt, &mut c_h3, &mut pipe.client);
    c_moq.post_handle(&mut pipe.client, &mut c_h3, &mut c_wt);
    assert!(c_moq.initialized());

    MoqPipe { pipe, c_h3, c_wt, c_moq, s_h3, s_wt, s_moq }
}
