/// Low Overhead Media Container (LOC) properties.
///
/// Implements draft-ietf-moq-loc-04 §2.3.
///
/// LOC Properties are encoded as MOQ Object Properties (key-value pairs) and
/// follow the same parity convention as the MoQ transport KVP format:
///   - even ID → varint value
///   - odd  ID → length-prefixed bytes value
///
/// This crate provides typed wrappers and conversions to/from [`KeyValuePair`].
pub mod property;

pub use property::{
    AudioConfig, AudioLevel, LocProperties, LocProperty, Timescale, Timestamp, VideoConfig,
    VideoFrameMarking, AUDIO_CONFIG_ID, AUDIO_LEVEL_ID, TIMESCALE_ID, TIMESTAMP_ID,
    VIDEO_CONFIG_ID, VIDEO_FRAME_MARKING_ID,
};
