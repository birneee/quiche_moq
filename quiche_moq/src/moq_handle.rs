use quiche::h3;
use crate::{MoqTransportSession, PollResult};
use quiche_webtransport as wt;

/// Temporary handle that bundles all connection references for ergonomic API calls.
pub struct MoqHandle<'a> {
    pub session: &'a mut MoqTransportSession,
    pub quic: &'a mut quiche::Connection,
    pub h3: &'a mut h3::Connection,
    pub wt: &'a mut wt::Connection,
}

impl<'a> MoqHandle<'a> {
    pub fn new(
        session: &'a mut MoqTransportSession,
        quic: &'a mut quiche::Connection,
        h3: &'a mut h3::Connection,
        wt: &'a mut wt::Connection,
    ) -> Self {
        Self { session, quic, h3, wt }
    }

    /// Close the MoQ session cleanly. draft-ietf-moq-transport-16 Section 3.4:
    /// "When WebTransport is used, the session is closed using the
    /// CLOSE_WEBTRANSPORT_SESSION capsule." Use `wire::NO_ERROR` for a clean shutdown.
    pub fn close(&mut self, error_code: u32, reason: &str) {
        let _ = self.wt.close_session(self.session.webtransport_session_id.into(), error_code, reason, self.quic, self.h3);
    }

    /// Drive wt and moq in order, draining h3 reset events first.
    pub fn poll_all(&mut self) -> PollResult {
        loop {
            match self.h3.poll(self.quic) {
                Err(h3::Error::Done) => break,
                // MoQ stream resets are handled at the QUIC level by session.poll.
                Ok((_, h3::Event::Reset(_))) => {}
                other => panic!("unexpected h3 event: {:?}", other),
            }
        }
        self.wt.poll(self.h3, self.quic);
        self.session.handle(self.wt, self.h3, self.quic)
    }
}
