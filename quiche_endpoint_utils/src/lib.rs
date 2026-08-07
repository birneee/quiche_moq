mod sigint;
pub use sigint::SignalSource;

use quiche_mio_runner::quiche_endpoint::{
    make_qlog_writer,
    quiche::{Connection, ConnectionId},
};
use std::time::{SystemTime, UNIX_EPOCH};

/// Enable qlog if QLOGDIR environment variable is set.
pub fn setup_qlog<TAppData>(conn: &mut Connection, role: &str, scid: &ConnectionId, _app_data: &TAppData) {
    if let Some(dir) = std::env::var_os("QLOGDIR") {
        let id = format!("{:?}", scid);
        let writer = make_qlog_writer(&dir, role, &id);
        conn.set_qlog(
            Box::new(writer),
            format!("quiche-{} qlog", role),
            format!("quiche-{} qlog id={}", role, id),
        );

        if let Some(qlog) = conn.qlog_streamer() {
            let t = SystemTime::now();
            qlog.add_event_now(qlog::events::JsonEvent {
                time: 0.0,
                importance: qlog::events::EventImportance::Core,
                name: "reference_time".into(),
                data: serde_json::json!({
                    "reference_time": t.duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs_f64() * 1000.0)
                        .unwrap_or(0.0),
                }),
            })
            .ok();
        }
    }
}
