use moq_wire::{KeyValuePair, KeyValuePairValue, KeyValuePairs};

/// Timestamp property ID (§2.3.1.1). Even → varint.
pub const TIMESTAMP_ID: u64 = 0x10;
/// Timescale property ID (§2.3.1.2). Even → varint.
pub const TIMESCALE_ID: u64 = 0x08;
/// Video Frame Marking property ID (§2.3.2.2). Odd → length-prefixed bytes.
pub const VIDEO_FRAME_MARKING_ID: u64 = 0x09;
/// Video Config property ID (§2.3.2.1). Odd → length-prefixed bytes.
pub const VIDEO_CONFIG_ID: u64 = 0x0D;
/// Audio Config property ID (§2.3.3.1). Odd → length-prefixed bytes.
pub const AUDIO_CONFIG_ID: u64 = 0x0F;
/// Audio Level property ID (§2.3.3.2). Even → varint.
pub const AUDIO_LEVEL_ID: u64 = 0x0C;

/// Timestamp of the encoded media frame (§2.3.1.1).
///
/// Without a Timescale property the unit is microseconds since Unix epoch.
/// With a Timescale property the unit is Timestamp / Timescale seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timestamp(pub u64);

/// Number of Timestamp units per second (§2.3.1.2).
///
/// Common values: 1_000_000 (µs), 48_000 (48kHz audio), 90_000 (video).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timescale(pub u64);

/// Video frame marking flags per [RFC 9626] (§2.3.2.2).
///
/// Carries the raw RFC 9626 frame-marking bytes as sent on the wire (1-4
/// bytes), not a packed integer:
///
/// ```text
/// byte 0: S E I D B TID[2] TID[1] TID[0]
/// byte 1: LID
/// bytes 2-3: TL0PICIDX (optional)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrameMarking(pub Vec<u8>);

impl VideoFrameMarking {
    /// Independent frame (I) flag from byte 0 — set on keyframes.
    pub fn is_independent(&self) -> bool {
        self.0.first().is_some_and(|b0| b0 & 0x20 != 0)
    }
}

/// Video codec configuration "extradata" (§2.3.2.1).
///
/// Maps to `VideoDecoderConfig.description` in WebCodecs (e.g.
/// `AVCDecoderConfigurationRecord` for H.264).
///
/// Wire shape used by this codebase: `[FourCC (4 bytes)][stsd sample-entry
/// body]`. The FourCC (e.g. `b"avc1"`) identifies the codec; the body is
/// everything inside the stsd entry box after its 8-byte header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoConfig(pub Vec<u8>);

/// Audio codec configuration "extradata" (§2.3.3.1).
///
/// Maps to `AudioDecoderConfig.description` in WebCodecs.
///
/// Same wire shape as [`VideoConfig`]: `[FourCC (4 bytes)][stsd sample-entry
/// body]` (e.g. FourCC `b"mp4a"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfig(pub Vec<u8>);

/// Audio level and voice-activity indicator per [RFC 6464] §3 (§2.3.3.2).
///
/// Bit layout (LSBs of the varint):
/// ```text
/// bit 7 : V – voice activity flag (1 = active)
/// bits 6-0 : level – magnitude (0 = loudest, 127 = digital silence)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioLevel(pub u8);

impl AudioLevel {
    pub fn voice_active(self) -> bool { self.0 & 0x80 != 0 }
    pub fn level(self) -> u8 { self.0 & 0x7F }
}

/// A single typed LOC Property.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocProperty {
    /// Timestamp. See [`TIMESTAMP_ID`].
    Timestamp(Timestamp),
    /// Timescale. See [`TIMESCALE_ID`].
    Timescale(Timescale),
    /// Video Frame Marking. See [`VIDEO_FRAME_MARKING_ID`].
    VideoFrameMarking(VideoFrameMarking),
    /// Video Config. See [`VIDEO_CONFIG_ID`].
    VideoConfig(VideoConfig),
    /// Audio Config. See [`AUDIO_CONFIG_ID`].
    AudioConfig(AudioConfig),
    /// Audio Level. See [`AUDIO_LEVEL_ID`].
    AudioLevel(AudioLevel),
}

impl LocProperty {
    pub fn id(&self) -> u64 {
        match self {
            LocProperty::Timestamp(_) => TIMESTAMP_ID,
            LocProperty::Timescale(_) => TIMESCALE_ID,
            LocProperty::VideoFrameMarking(_) => VIDEO_FRAME_MARKING_ID,
            LocProperty::VideoConfig(_) => VIDEO_CONFIG_ID,
            LocProperty::AudioConfig(_) => AUDIO_CONFIG_ID,
            LocProperty::AudioLevel(_) => AUDIO_LEVEL_ID,
        }
    }

    /// Convert to a [`KeyValuePair`], returning an error if the ID parity
    /// constraint is violated (which cannot happen for the known variants).
    pub fn to_kvp(&self) -> KeyValuePair {
        match self {
            LocProperty::Timestamp(Timestamp(v)) => {
                KeyValuePair::new_varint(TIMESTAMP_ID, *v).unwrap()
            }
            LocProperty::Timescale(Timescale(v)) => {
                KeyValuePair::new_varint(TIMESCALE_ID, *v).unwrap()
            }
            LocProperty::VideoFrameMarking(VideoFrameMarking(bytes)) => {
                KeyValuePair::new_bytes(VIDEO_FRAME_MARKING_ID, bytes.clone()).unwrap()
            }
            LocProperty::VideoConfig(VideoConfig(data)) => {
                KeyValuePair::new_bytes(VIDEO_CONFIG_ID, data.clone()).unwrap()
            }
            LocProperty::AudioConfig(AudioConfig(data)) => {
                KeyValuePair::new_bytes(AUDIO_CONFIG_ID, data.clone()).unwrap()
            }
            LocProperty::AudioLevel(AudioLevel(v)) => {
                KeyValuePair::new_varint(AUDIO_LEVEL_ID, *v as u64).unwrap()
            }
        }
    }

    /// Try to decode a [`KeyValuePair`] as a known LOC Property.
    ///
    /// Returns `None` if the ID is not a recognised LOC property.
    pub fn from_kvp(kvp: &KeyValuePair) -> Option<Self> {
        match (kvp.ty(), kvp.value()) {
            (TIMESTAMP_ID, KeyValuePairValue::Varint(v)) => {
                Some(LocProperty::Timestamp(Timestamp(*v)))
            }
            (TIMESCALE_ID, KeyValuePairValue::Varint(v)) => {
                Some(LocProperty::Timescale(Timescale(*v)))
            }
            (VIDEO_FRAME_MARKING_ID, KeyValuePairValue::Bytes(data)) => {
                Some(LocProperty::VideoFrameMarking(VideoFrameMarking(data.clone())))
            }
            (VIDEO_CONFIG_ID, KeyValuePairValue::Bytes(data)) => {
                Some(LocProperty::VideoConfig(VideoConfig(data.clone())))
            }
            (AUDIO_CONFIG_ID, KeyValuePairValue::Bytes(data)) => {
                Some(LocProperty::AudioConfig(AudioConfig(data.clone())))
            }
            (AUDIO_LEVEL_ID, KeyValuePairValue::Varint(v)) => {
                Some(LocProperty::AudioLevel(AudioLevel(*v as u8)))
            }
            _ => None,
        }
    }
}

/// A collection of LOC Properties for one MOQ object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocProperties(Vec<LocProperty>);

impl LocProperties {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn push(&mut self, prop: LocProperty) {
        self.0.push(prop);
    }

    pub fn iter(&self) -> impl Iterator<Item = &LocProperty> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return the first Timestamp property if present.
    pub fn timestamp(&self) -> Option<Timestamp> {
        self.0.iter().find_map(|p| {
            if let LocProperty::Timestamp(ts) = p { Some(*ts) } else { None }
        })
    }

    /// Return the first Timescale property if present.
    pub fn timescale(&self) -> Option<Timescale> {
        self.0.iter().find_map(|p| {
            if let LocProperty::Timescale(ts) = p { Some(*ts) } else { None }
        })
    }

    /// Return the first VideoFrameMarking property if present.
    pub fn video_frame_marking(&self) -> Option<&VideoFrameMarking> {
        self.0.iter().find_map(|p| {
            if let LocProperty::VideoFrameMarking(fm) = p { Some(fm) } else { None }
        })
    }

    /// Return the first VideoConfig property if present.
    pub fn video_config(&self) -> Option<&VideoConfig> {
        self.0.iter().find_map(|p| {
            if let LocProperty::VideoConfig(cfg) = p { Some(cfg) } else { None }
        })
    }

    /// Return the first AudioConfig property if present.
    pub fn audio_config(&self) -> Option<&AudioConfig> {
        self.0.iter().find_map(|p| {
            if let LocProperty::AudioConfig(cfg) = p { Some(cfg) } else { None }
        })
    }

    /// Return the codec config, whether carried as VideoConfig or
    /// AudioConfig — for callers that only need "is there codec extradata
    /// on this object" without caring which media type it's for.
    pub fn codec_config(&self) -> Option<&[u8]> {
        self.video_config()
            .map(|c| c.0.as_slice())
            .or_else(|| self.audio_config().map(|c| c.0.as_slice()))
    }

    /// Return the first AudioLevel property if present.
    pub fn audio_level(&self) -> Option<AudioLevel> {
        self.0.iter().find_map(|p| {
            if let LocProperty::AudioLevel(lvl) = p { Some(*lvl) } else { None }
        })
    }

    /// Encode all LOC Properties as a [`KeyValuePairs`] for use in an MOQ
    /// Object Header.
    pub fn to_kvps(&self) -> KeyValuePairs {
        self.0.iter().map(|p| p.to_kvp()).collect::<Vec<_>>().into()
    }

    /// Decode LOC Properties from a [`KeyValuePairs`] object header.
    ///
    /// Unknown properties are silently skipped.
    pub fn from_kvps(kvps: &KeyValuePairs) -> Self {
        let props = kvps
            .iter()
            .filter_map(LocProperty::from_kvp)
            .collect();
        LocProperties(props)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_wire::{MOQ_VERSION_DRAFT_16, Version};

    fn roundtrip(props: &LocProperties, version: Version) -> LocProperties {
        let kvps = props.to_kvps();
        // serialize → deserialize via KeyValuePairs byte round-trip
        let byte_len = kvps.byte_length(version);
        let mut buf = vec![0u8; byte_len];
        let mut b = octets::OctetsMut::with_slice(&mut buf);
        use moq_wire::ToBytes;
        kvps.to_bytes(&mut b, version).unwrap();

        let mut b = octets::Octets::with_slice(&buf);
        use moq_wire::FromBytes;
        let decoded_kvps = KeyValuePairs::from_bytes(&mut b, (version, props.0.len() as u64)).unwrap();
        LocProperties::from_kvps(&decoded_kvps)
    }

    #[test]
    fn timestamp_roundtrip() {
        let mut props = LocProperties::new();
        props.push(LocProperty::Timestamp(Timestamp(1_700_000_000_000_000)));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.timestamp(), Some(Timestamp(1_700_000_000_000_000)));
    }

    #[test]
    fn timescale_roundtrip() {
        let mut props = LocProperties::new();
        props.push(LocProperty::Timescale(Timescale(90_000)));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.timescale(), Some(Timescale(90_000)));
    }

    #[test]
    fn video_frame_marking_roundtrip() {
        // byte0=0xE0 (S|E|I set), byte1=0x00 (LID=0), bytes2-3=TL0PICIDX
        let fm = VideoFrameMarking(vec![0xE0, 0x00, 0x00, 0x02]);
        let mut props = LocProperties::new();
        props.push(LocProperty::VideoFrameMarking(fm.clone()));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        let got = decoded.video_frame_marking().unwrap();
        assert_eq!(*got, fm);
        assert!(got.is_independent());
    }

    #[test]
    fn video_config_roundtrip() {
        let cfg = vec![0x01, 0x64, 0x00, 0x1f, 0xff];
        let mut props = LocProperties::new();
        props.push(LocProperty::VideoConfig(VideoConfig(cfg.clone())));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.video_config().unwrap().0, cfg);
        assert_eq!(decoded.codec_config(), Some(cfg.as_slice()));
    }

    #[test]
    fn audio_config_roundtrip() {
        let cfg = vec![b'm', b'p', b'4', b'a', 0x12, 0x10];
        let mut props = LocProperties::new();
        props.push(LocProperty::AudioConfig(AudioConfig(cfg.clone())));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.audio_config().unwrap().0, cfg);
        assert_eq!(decoded.codec_config(), Some(cfg.as_slice()));
    }

    #[test]
    fn audio_level_roundtrip() {
        let lvl = AudioLevel(0b1000_0000 | 42); // voice active, level=42
        let mut props = LocProperties::new();
        props.push(LocProperty::AudioLevel(lvl));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.audio_level(), Some(lvl));
        assert!(decoded.audio_level().unwrap().voice_active());
        assert_eq!(decoded.audio_level().unwrap().level(), 42);
    }

    #[test]
    fn timestamp_and_audio_level_are_no_longer_ambiguous() {
        // draft-ietf-moq-loc-04 gives Audio Level its own ID (0x0C), distinct
        // from Timestamp (0x10) — draft-02 had them share an ID.
        let mut props = LocProperties::new();
        props.push(LocProperty::Timestamp(Timestamp(123)));
        props.push(LocProperty::AudioLevel(AudioLevel(42)));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.timestamp(), Some(Timestamp(123)));
        assert_eq!(decoded.audio_level(), Some(AudioLevel(42)));
    }

    #[test]
    fn multiple_properties_roundtrip() {
        let mut props = LocProperties::new();
        props.push(LocProperty::Timestamp(Timestamp(42_000)));
        props.push(LocProperty::Timescale(Timescale(1_000_000)));
        props.push(LocProperty::VideoConfig(VideoConfig(vec![0xAB, 0xCD])));
        let decoded = roundtrip(&props, MOQ_VERSION_DRAFT_16);
        assert_eq!(decoded.timestamp(), Some(Timestamp(42_000)));
        assert_eq!(decoded.timescale(), Some(Timescale(1_000_000)));
        assert_eq!(decoded.video_config().unwrap().0, &[0xAB, 0xCD]);
    }
}
