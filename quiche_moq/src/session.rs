use crate::config::Config;
use moq_core::Result;
use crate::transport::QuicheTransport;
use moq_core::Timestamp;
use quiche::h3;
use quiche_utils::stream_id::StreamID;
use quiche_webtransport as wt;

/// A `moq_core` session on one WebTransport session of a quiche connection.
pub struct MoqTransportSession {
    pub(crate) inner: moq_core::MoqTransportSession,
    pub(crate) webtransport_session_id: StreamID,
}

impl MoqTransportSession {
    /// Opens the control channel and sends CLIENT_SETUP.
    pub fn connect(
        session_id: StreamID,
        h3: &mut h3::Connection,
        quic: &mut quiche::Connection,
        wt: &mut wt::Connection,
        config: Config,
    ) -> Result<MoqTransportSession> {
        let mut t = QuicheTransport::new(session_id.into(), wt, h3, quic);
        let inner = moq_core::MoqTransportSession::connect(&mut t, config)?;
        Ok(Self { inner, webtransport_session_id: session_id })
    }

    /// Server side, waits for CLIENT_SETUP.
    pub fn accept(session_id: StreamID, config: Config) -> Result<MoqTransportSession> {
        let inner = moq_core::MoqTransportSession::accept(config)?;
        Ok(Self { inner, webtransport_session_id: session_id })
    }

    /// The `moq_core` session, for calls without a transport.
    pub fn session(&self) -> &moq_core::MoqTransportSession {
        &self.inner
    }

    /// The `moq_core` session, and the `Transport` its calls take.
    pub fn core<'a>(
        &'a mut self,
        wt: &'a mut wt::Connection,
        h3: &'a mut h3::Connection,
        quic: &'a mut quiche::Connection,
    ) -> (&'a mut moq_core::MoqTransportSession, QuicheTransport<'a>) {
        (&mut self.inner, QuicheTransport::new(self.webtransport_session_id.into(), wt, h3, quic))
    }

    /// Processes what arrived on the connection, see `moq_core::MoqTransportSession::handle`.
    pub fn handle(&mut self, wt: &mut wt::Connection, h3: &mut h3::Connection, quic: &mut quiche::Connection) {
        let readable = wt.readable_streams(self.webtransport_session_id.into()).to_vec();
        let (session, mut t) = self.core(wt, h3, quic);
        session.handle(&mut t, &readable, Timestamp::now());
    }
}
