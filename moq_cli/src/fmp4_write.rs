//! Generic fragmented MP4 writer for LOC subscriber output.
//!
//! Codec-agnostic: the stsd sample-entry body is taken verbatim from the LOC
//! `VideoConfig` property (`[FourCC(4)][stsd entry body]`).  The FourCC determines
//! the track type (video → vmhd, audio → smhd) and the handler name.

// ── public API ────────────────────────────────────────────────────────────────

/// Write the `ftyp` box (once at the start of the stream).
pub fn ftyp() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(b"isom");
    put_u32(&mut c, 0x200);
    c.extend_from_slice(b"isom");
    c.extend_from_slice(b"iso6");
    c.extend_from_slice(b"msdh");
    bwrap(b"ftyp", c)
}

/// Write a `moov` box for one or more tracks.
///
/// `tracks` is a slice of `(timescale, codec_config)` where `codec_config` is
/// `[FourCC(4)][stsd sample-entry body]` as delivered in the LOC `VideoConfig` property.
/// Track IDs are assigned 1-based in slice order.
pub fn moov(tracks: &[(u32, &[u8])]) -> Vec<u8> {
    let mut m = Vec::new();
    // Use the timescale of the first track for the movie header.
    m.extend_from_slice(&mvhd(tracks.first().map(|(ts, _)| *ts).unwrap_or(1_000_000)));
    for (i, (timescale, codec_config)) in tracks.iter().enumerate() {
        m.extend_from_slice(&trak((i + 1) as u32, *timescale, codec_config));
    }
    m.extend_from_slice(&mvex(tracks.len() as u32));
    bwrap(b"moov", m)
}

/// Write one `moof`+`mdat` fragment for a single sample.
///
/// `seq`         – monotonically increasing across all fragments in the stream.
/// `track_id`    – 1-based track ID matching the moov.
/// `decode_time` – in the track's timescale units.
/// `duration`    – sample duration in the same units (0 = receiver infers from next tfdt).
/// `is_sync`     – true for group-start / random-access frames.
pub fn fragment(seq: u32, track_id: u32, decode_time: u64, duration: u32, is_sync: bool, payload: &[u8]) -> Vec<u8> {
    let payload_len = payload.len() as u32;
    let trun_data = trun_sample(payload_len, duration, is_sync);
    let trun_len = (8 + trun_data.len()) as u32;

    let tfhd_len = 8 + 8u32;
    let tfdt_len = 8 + 12u32;
    let traf_content = tfhd_len + tfdt_len + trun_len;
    let traf_len = 8 + traf_content;
    let mfhd_len = 8 + 8u32;
    let moof_len = 8 + mfhd_len + traf_len;
    let data_offset = moof_len as i32 + 8;

    let mut out = Vec::with_capacity(moof_len as usize + 8 + payload.len());

    // moof
    let mut moof_c = Vec::new();
    // mfhd
    let mut mfhd_c = Vec::new();
    put_u8(&mut mfhd_c, 0); put_zeros(&mut mfhd_c, 3);
    put_u32(&mut mfhd_c, seq);
    moof_c.extend_from_slice(&bwrap(b"mfhd", mfhd_c));
    // traf
    let mut traf_c = Vec::new();
    let mut tfhd_c = Vec::new();
    put_u8(&mut tfhd_c, 0); put_u24(&mut tfhd_c, 0x020000);
    put_u32(&mut tfhd_c, track_id);
    traf_c.extend_from_slice(&bwrap(b"tfhd", tfhd_c));
    let mut tfdt_c = Vec::new();
    put_u8(&mut tfdt_c, 1); put_zeros(&mut tfdt_c, 3);
    put_u64(&mut tfdt_c, decode_time);
    traf_c.extend_from_slice(&bwrap(b"tfdt", tfdt_c));
    // trun with data_offset patched
    let mut trun_box = Vec::new();
    put_u32(&mut trun_box, (8 + trun_data.len()) as u32);
    trun_box.extend_from_slice(b"trun");
    trun_box.extend_from_slice(&trun_data);
    trun_box[16..20].copy_from_slice(&data_offset.to_be_bytes());
    traf_c.extend_from_slice(&trun_box);
    moof_c.extend_from_slice(&bwrap(b"traf", traf_c));
    out.extend_from_slice(&bwrap(b"moof", moof_c));

    // mdat
    put_u32(&mut out, 8 + payload.len() as u32);
    out.extend_from_slice(b"mdat");
    out.extend_from_slice(payload);
    out
}

// ── track type detection ──────────────────────────────────────────────────────

fn is_video(fourcc: &[u8]) -> bool {
    matches!(fourcc, b"avc1" | b"avc3" | b"avc2"
        | b"hvc1" | b"hev1" | b"dvhe" | b"dvh1"
        | b"vp08" | b"vp09" | b"av01")
}

// ── moov box builders ─────────────────────────────────────────────────────────

fn trak(track_id: u32, timescale: u32, codec_config: &[u8]) -> Vec<u8> {
    if codec_config.len() < 4 { return Vec::new(); }
    let fourcc = &codec_config[..4];
    let entry_body = &codec_config[4..];
    let video = is_video(fourcc);

    let mut c = Vec::new();
    c.extend_from_slice(&tkhd(track_id, video));
    c.extend_from_slice(&mdia(timescale, fourcc, entry_body, video));
    bwrap(b"trak", c)
}

fn mvhd(timescale: u32) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 0); put_u32(&mut c, 0);
    put_u32(&mut c, timescale);
    put_u32(&mut c, 0);
    put_u32(&mut c, 0x00010000); put_u16(&mut c, 0x0100);
    put_zeros(&mut c, 10);
    c.extend_from_slice(&[
        0x00,0x01,0x00,0x00, 0,0,0,0, 0,0,0,0,
        0,0,0,0, 0x00,0x01,0x00,0x00, 0,0,0,0,
        0,0,0,0, 0,0,0,0, 0x40,0x00,0x00,0x00,
    ]);
    put_zeros(&mut c, 24);
    put_u32(&mut c, 0xFF); // next track id (large enough)
    bwrap(b"mvhd", c)
}

fn tkhd(track_id: u32, video: bool) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_u24(&mut c, 3);
    put_u32(&mut c, 0); put_u32(&mut c, 0);
    put_u32(&mut c, track_id);
    put_u32(&mut c, 0); put_u32(&mut c, 0);
    put_zeros(&mut c, 8);
    put_u16(&mut c, 0); put_u16(&mut c, 0);
    put_u16(&mut c, if video { 0 } else { 0x0100 }); // volume: 0 for video, 1.0 for audio
    put_zeros(&mut c, 2);
    c.extend_from_slice(&[
        0x00,0x01,0x00,0x00, 0,0,0,0, 0,0,0,0,
        0,0,0,0, 0x00,0x01,0x00,0x00, 0,0,0,0,
        0,0,0,0, 0,0,0,0, 0x40,0x00,0x00,0x00,
    ]);
    put_u32(&mut c, 0); put_u32(&mut c, 0); // width, height (unknown)
    bwrap(b"tkhd", c)
}

fn mdia(timescale: u32, fourcc: &[u8], entry_body: &[u8], video: bool) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&mdhd(timescale));
    c.extend_from_slice(&hdlr(video));
    c.extend_from_slice(&minf(fourcc, entry_body, video));
    bwrap(b"mdia", c)
}

fn mdhd(timescale: u32) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 0); put_u32(&mut c, 0);
    put_u32(&mut c, timescale);
    put_u32(&mut c, 0);
    put_u16(&mut c, 0x55C4); put_u16(&mut c, 0);
    bwrap(b"mdhd", c)
}

fn hdlr(video: bool) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 0);
    if video { c.extend_from_slice(b"vide"); c.extend_from_slice(b"\0\0\0\0\0\0\0\0\0\0\0\0VideoHandler\0"); }
    else      { c.extend_from_slice(b"soun"); c.extend_from_slice(b"\0\0\0\0\0\0\0\0\0\0\0\0SoundHandler\0"); }
    bwrap(b"hdlr", c)
}

fn minf(fourcc: &[u8], entry_body: &[u8], video: bool) -> Vec<u8> {
    let mut c = Vec::new();
    if video { c.extend_from_slice(&vmhd()); } else { c.extend_from_slice(&smhd()); }
    c.extend_from_slice(&dinf());
    c.extend_from_slice(&stbl(fourcc, entry_body));
    bwrap(b"minf", c)
}

fn vmhd() -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_u24(&mut c, 1);
    put_u16(&mut c, 0); put_zeros(&mut c, 6);
    bwrap(b"vmhd", c)
}

fn smhd() -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u16(&mut c, 0); put_u16(&mut c, 0); // balance, reserved
    bwrap(b"smhd", c)
}

fn dinf() -> Vec<u8> {
    let mut url_c = Vec::new();
    put_u8(&mut url_c, 0); put_u24(&mut url_c, 1);
    let mut dref_c = Vec::new();
    put_u8(&mut dref_c, 0); put_zeros(&mut dref_c, 3);
    put_u32(&mut dref_c, 1);
    dref_c.extend_from_slice(&bwrap(b"url ", url_c));
    bwrap(b"dinf", bwrap(b"dref", dref_c))
}

fn stbl(fourcc: &[u8], entry_body: &[u8]) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&stsd(fourcc, entry_body));
    c.extend_from_slice(&empty_box(b"stts"));
    c.extend_from_slice(&empty_box(b"stsc"));
    c.extend_from_slice(&empty_box_with_entry(b"stsz")); // stsz needs sample_size + count
    c.extend_from_slice(&empty_box(b"stco"));
    bwrap(b"stbl", c)
}

fn stsd(fourcc: &[u8], entry_body: &[u8]) -> Vec<u8> {
    // Wrap the verbatim stsd entry body in a box with the codec FourCC.
    let entry = {
        let mut v = Vec::with_capacity(8 + entry_body.len());
        put_u32(&mut v, (8 + entry_body.len()) as u32);
        v.extend_from_slice(&fourcc[..4]);
        v.extend_from_slice(entry_body);
        v
    };
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 1); // entry_count
    c.extend_from_slice(&entry);
    bwrap(b"stsd", c)
}

fn empty_box(name: &[u8; 4]) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 0);
    bwrap(name, c)
}

fn empty_box_with_entry(name: &[u8; 4]) -> Vec<u8> {
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_zeros(&mut c, 3);
    put_u32(&mut c, 0); // sample_size
    put_u32(&mut c, 0); // sample_count
    bwrap(name, c)
}

fn mvex(track_count: u32) -> Vec<u8> {
    let mut c = Vec::new();
    for track_id in 1..=track_count {
        let mut trex_c = Vec::new();
        put_u8(&mut trex_c, 0); put_zeros(&mut trex_c, 3);
        put_u32(&mut trex_c, track_id);
        put_u32(&mut trex_c, 1); // default_sample_description_index
        put_zeros(&mut trex_c, 12); // default duration/size/flags = 0
        c.extend_from_slice(&bwrap(b"trex", trex_c));
    }
    bwrap(b"mvex", c)
}

// ── trun helper ───────────────────────────────────────────────────────────────

fn trun_sample(size: u32, duration: u32, is_sync: bool) -> Vec<u8> {
    let flags: u32 = 0x000001 | 0x000004 | 0x000100 | 0x000200;
    let mut c = Vec::new();
    put_u8(&mut c, 0); put_u24(&mut c, flags);
    put_u32(&mut c, 1); // sample_count
    put_u32(&mut c, 0); // data_offset placeholder (patched in fragment())
    let fsf: u32 = if is_sync { 0 } else { 0x00010000 };
    put_u32(&mut c, fsf);
    put_u32(&mut c, duration);
    put_u32(&mut c, size);
    c
}

// ── write helpers ─────────────────────────────────────────────────────────────

fn bwrap(box_type: &[u8; 4], content: Vec<u8>) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + content.len());
    put_u32(&mut v, (8 + content.len()) as u32);
    v.extend_from_slice(box_type);
    v.extend_from_slice(&content);
    v
}

fn put_u32(v: &mut Vec<u8>, n: u32) { v.extend_from_slice(&n.to_be_bytes()); }
fn put_u24(v: &mut Vec<u8>, n: u32) { v.extend_from_slice(&n.to_be_bytes()[1..]); }
fn put_u16(v: &mut Vec<u8>, n: u16) { v.extend_from_slice(&n.to_be_bytes()); }
fn put_u64(v: &mut Vec<u8>, n: u64) { v.extend_from_slice(&n.to_be_bytes()); }
fn put_u8(v: &mut Vec<u8>, n: u8)   { v.push(n); }
fn put_zeros(v: &mut Vec<u8>, n: usize) { v.extend(std::iter::repeat_n(0u8, n)); }
