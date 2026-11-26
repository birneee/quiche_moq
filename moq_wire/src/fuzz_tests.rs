//! Decoding arbitrary peer bytes returns errors, never panics.

use crate::prelude::*;
use crate::bytes::FromBytes;
use crate::control_message::ControlMessageEnum;
use crate::object::ObjectHeader;
use crate::subgroup::SubgroupHeader;
use crate::{Version, SUPPORTED_MOQ_VERSIONS};

const ROUNDS: usize = 20_000;

/// xorshift64, deterministic so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// A known message type with a matching length, so the body decoder runs.
fn control_message(rng: &mut Rng, version: Version) -> Vec<u8> {
    let len = rng.below(96) as usize;
    let body = rng.bytes(len);
    let mut buf = vec![];
    let mut b = version.writer(&mut buf);
    b.varint(rng.below(0x50)).unwrap();
    let body = |b: &mut crate::Writer| {
        b.bytes(&body);
        Ok::<_, crate::EncodeError>(())
    };
    match version.message_length_is_fixed_u16() {
        true => b.with_u16_length(body),
        false => b.with_varint_length(body),
    }
    .unwrap();
    buf
}

#[test]
fn control_messages_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for &version in SUPPORTED_MOQ_VERSIONS {
        for _ in 0..ROUNDS {
            let bytes = control_message(&mut rng, version);
            let _ = ControlMessageEnum::from_bytes(&mut version.reader(&bytes), version);
        }
    }
}

#[test]
fn data_streams_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    for &version in SUPPORTED_MOQ_VERSIONS {
        for _ in 0..ROUNDS {
            let len = rng.below(64) as usize;
            let mut bytes = rng.bytes(len);
            if let Some(ty) = bytes.first_mut() {
                *ty = rng.below(0x40) as u8;
            }
            let mut b = version.reader(&bytes);
            let Ok(subgroup) = SubgroupHeader::from_bytes(&mut b, version) else { continue };
            let mut previous = None;
            while let Ok(object) = ObjectHeader::decode(&mut b, version, &subgroup, previous) {
                previous = Some(object.id());
            }
        }
    }
}
