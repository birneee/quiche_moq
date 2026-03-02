use std::time::{Instant, SystemTime};

use quiche_mio_runner::quiche_endpoint::{make_qlog_writer, quiche::{Connection, ConnectionId}};



/// Enable qlog if QLOGDIR environment variable is set.
pub fn setup_qlog(conn: &mut Connection, role: &str, scid: &ConnectionId) {
    if let Some(dir) = std::env::var_os("QLOGDIR") {
        let id = format!("{:?}", scid);
        let writer = make_qlog_writer(&dir, role, &id);

        conn.set_qlog_with_details(
            std::boxed::Box::new(writer),
            format!("quiche-{} qlog", role), 
            format!("quiche-{} qlog id={}", role, id), 
            None, 
            Some(SystemTime::now()), 
            Instant::now(),
        );
    }
}