use quiche::h3;
use std::time::Duration;
use crate::{MoqTransportSession, QuicheTransport};
use quiche_webtransport as wt;

/// A session together with the transport its calls take:
/// `moq.session.subscribe(.., &mut moq.t)`.
pub struct MoqHandle<'a> {
    pub session: &'a mut moq_core::MoqTransportSession,
    pub t: QuicheTransport<'a>,
}

impl<'a> MoqHandle<'a> {
    pub fn new(
        session: &'a mut MoqTransportSession,
        quic: &'a mut quiche::Connection,
        h3: &'a mut h3::Connection,
        wt: &'a mut wt::Connection,
    ) -> Self {
        let (session, t) = session.core(wt, h3, quic);
        Self { session, t }
    }

    /// Drives h3, WebTransport and the session in order.
    pub fn poll_all(&mut self) {
        self.t.poll_webtransport();
        let readable = self.t.readable_streams();
        self.session.handle(&mut self.t, &readable, moq_core::Timestamp::now());
    }

    /// See `MoqTransportSession::timeout`.
    pub fn timeout(&self) -> Option<Duration> {
        self.session.timeout_at().map(|at| at.saturating_duration_since(moq_core::Timestamp::now()))
    }
}
