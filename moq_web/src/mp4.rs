//! Minimal parsers for the pieces of an ISO-BMFF `stsd` entry (`avcC`,
//! `esds` boxes and MPEG-4 audio descriptors) needed to configure
//! `VideoDecoder`/`AudioDecoder` from LoC codec-config extension headers.

pub(crate) fn find_box_anywhere<'a>(data: &'a [u8], box_type: &[u8; 4]) -> Option<&'a [u8]> {
    for i in 4..data.len().saturating_sub(3) {
        if &data[i..i + 4] != box_type {
            continue;
        }
        let size =
            u32::from_be_bytes(data[i - 4..i].try_into().ok()?) as usize;
        if size >= 8 && i - 4 + size <= data.len() {
            return Some(&data[i + 4..i - 4 + size]);
        }
    }
    None
}

pub(crate) fn find_asc_in_descriptors(data: &[u8]) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < data.len() {
        let tag = data[pos];
        pos += 1;
        let mut size = 0usize;
        loop {
            if pos >= data.len() {
                return None;
            }
            let b = data[pos];
            pos += 1;
            size = (size << 7) | ((b & 0x7F) as usize);
            if b & 0x80 == 0 {
                break;
            }
        }
        let end = pos + size;
        if end > data.len() {
            return None;
        }
        match tag {
            0x05 => return Some(data[pos..end].to_vec()),
            0x03 => {
                if size < 3 {
                    pos = end;
                    continue;
                }
                let flags = data[pos + 2];
                let mut skip = 3usize;
                if flags & 0x80 != 0 {
                    skip += 2;
                }
                if flags & 0x40 != 0 && pos + skip < end {
                    skip += 1 + data[pos + skip] as usize;
                }
                if flags & 0x20 != 0 {
                    skip += 2;
                }
                if let Some(asc) = find_asc_in_descriptors(&data[pos + skip..end]) {
                    return Some(asc);
                }
            }
            0x04 => {
                if size > 13
                    && let Some(asc) = find_asc_in_descriptors(&data[pos + 13..end]) {
                        return Some(asc);
                    }
            }
            _ => {}
        }
        pos = end;
    }
    None
}
