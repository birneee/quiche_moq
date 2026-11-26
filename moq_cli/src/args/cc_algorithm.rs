use quiche_mio_runner::quiche_endpoint::quiche;

/// QUIC congestion control algorithm (see --cc-algorithm) — mirrors
/// `quiche::CongestionControlAlgorithm`'s variants that this build actually supports.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CcAlgorithm {
    /// Reno congestion control.
    Reno,
    /// CUBIC congestion control (quiche's default).
    Cubic,
    /// BBRv2, from quiche's gcongestion branch.
    Bbr,
}

impl From<CcAlgorithm> for quiche::CongestionControlAlgorithm {
    fn from(algo: CcAlgorithm) -> Self {
        match algo {
            CcAlgorithm::Reno => quiche::CongestionControlAlgorithm::Reno,
            CcAlgorithm::Cubic => quiche::CongestionControlAlgorithm::CUBIC,
            CcAlgorithm::Bbr => quiche::CongestionControlAlgorithm::Bbr2Gcongestion,
        }
    }
}
