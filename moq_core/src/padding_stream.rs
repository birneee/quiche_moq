use crate::transport::{StreamError, Transport};
use log::trace;
use octets::OctetsMut;
use moq_wire::{MAX_VARINT_LEN, PADDING_STREAM_TYPE_ID, RESET_STREAM_CODE_CANCELED};

pub(crate) enum OutPaddingStream {
    Requested,
    Type { stream_id: u64 },
    Data { stream_id: u64 },
}

impl OutPaddingStream {
    pub fn new() -> Self {
        Self::Requested
    }

    pub fn handle<T: Transport>(&mut self, t: &mut T) {
        'outer: loop {
            match self {
                Self::Requested => {
                    let stream_id = t.open_uni_stream().unwrap();
                    t.set_stream_priority(stream_id, u8::MAX, true);
                    *self = Self::Type { stream_id };
                    #[cfg(feature = "qlog")]
                    if let Some(qlog) = t.qlog_streamer() {
                        qlog.add_event_now(qlog::events::JsonEvent {
                            time: 0.0,
                            importance: qlog::events::EventImportance::Core,
                            name: "moqt:padding_stream_started".into(),
                            data: serde_json::json!({ "stream_id": stream_id }),
                        }).ok();
                    }
                }
                Self::Type { stream_id } => {
                    let mut raw = [0u8; MAX_VARINT_LEN];
                    let len = {
                        let mut buf = OctetsMut::with_slice(&mut raw);
                        buf.put_varint(PADDING_STREAM_TYPE_ID).unwrap();
                        buf.off()
                    };
                    t.send_stream_if_capacity(*stream_id, &raw[..len], false).unwrap();
                    *self = Self::Data { stream_id: *stream_id };
                }
                Self::Data { stream_id } => {
                    let buf = [0u8; 1 << 16];
                    loop {
                        match t.send_stream(*stream_id, &buf, false) {
                            Ok(0) => break 'outer,
                            Ok(n) => trace!("sent {n} bytes on padding stream"),
                            Err(StreamError::Done) => break 'outer,
                            Err(e) => unimplemented!("{:?}", e),
                        }
                    }
                }
            }
        }
    }

    /// Cancel the padding stream. No-op if not yet opened.
    pub fn stop<T: Transport>(&self, t: &mut T) {
        if let Self::Type { stream_id } | Self::Data { stream_id } = self {
            t.reset_stream(*stream_id, RESET_STREAM_CODE_CANCELED as u64);
            #[cfg(feature = "qlog")]
            if let Some(qlog) = t.qlog_streamer() {
                qlog.add_event_now(qlog::events::JsonEvent {
                    time: 0.0,
                    importance: qlog::events::EventImportance::Core,
                    name: "moqt:padding_stream_stopped".into(),
                    data: serde_json::json!({ "stream_id": stream_id }),
                }).ok();
            }
        }
    }
}
