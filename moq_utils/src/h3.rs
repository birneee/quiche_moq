use quiche_mio_runner::quiche_endpoint::quiche;
use quiche_mio_runner::quiche_endpoint::quiche::h3::NameValue;

pub const ALPN_HTTP_3: &[u8] = b"h3";

/// get human readable headers, for debugging or logging
pub fn hdrs_to_strings(hdrs: &[quiche::h3::Header]) -> Vec<(String, String)> {
    hdrs.iter()
        .map(|h| {
            let name = String::from_utf8_lossy(h.name()).to_string();
            let value = String::from_utf8_lossy(h.value()).to_string();

            (name, value)
        })
        .collect()
}
