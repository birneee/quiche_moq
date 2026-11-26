//! Arbitrary streams from the peer are read or dropped, they never panic.

use crate::test_utils::_init_webtransport_pipe;

const ROUNDS: usize = 300;

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

    fn bytes(&mut self, max_len: u64) -> Vec<u8> {
        let len = self.below(max_len);
        (0..len).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn arbitrary_peer_streams_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..ROUNDS {
        let (mut pipe, mut c_h3, mut c_wt, mut s_h3, mut s_wt, session_id) = _init_webtransport_pipe();
        for _ in 0..1 + rng.below(4) {
            let Ok(stream_id) = c_wt.open_stream(session_id, &mut c_h3, &mut pipe.client, rng.below(2) == 0) else { continue };
            let mut bytes = Vec::new();
            // mostly the right session ID, the stream's own header
            if rng.below(4) != 0 {
                bytes.push(session_id as u8);
            }
            bytes.extend(rng.bytes(32));
            if rng.below(2) == 0 {
                bytes.truncate(rng.below(3) as usize);
            }
            let _ = pipe.client.stream_send(stream_id, &bytes, rng.below(2) == 0);
            if rng.below(8) == 0 {
                let _ = pipe.client.stream_shutdown(stream_id, quiche::Shutdown::Write, rng.below(16));
            }
        }
        // capsules or garbage on the CONNECT stream
        if rng.below(4) == 0 {
            let _ = c_h3.send_body(&mut pipe.client, session_id, &rng.bytes(16), rng.below(2) == 0);
        }
        for _ in 0..3 {
            let _ = pipe.advance();
            s_wt.poll_h3(&mut s_h3, &mut pipe.server);
            s_wt.poll(&mut s_h3, &mut pipe.server);
            let mut buf = [0u8; 16];
            for session in s_wt.readable_sessions() {
                for stream_id in s_wt.readable_streams(session) {
                    while s_wt.recv_stream(stream_id, session, &mut s_h3, &mut pipe.server, &mut buf).is_ok() {}
                }
            }
            while c_h3.poll(&mut pipe.client).is_ok() {}
        }
    }
}

#[test]
fn arbitrary_connect_requests_never_panic() {
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    // a valid CONNECT, each header kept, dropped or replaced
    const VALID: [(&[u8], &[u8]); 7] = [
        (b":method", b"CONNECT"),
        (b":protocol", b"webtransport"),
        (b":scheme", b"https"),
        (b":authority", b"example.org"),
        (b":path", b"/"),
        (b"wt-available-protocols", b"\"moqt-16\", \"moqt-18\""),
        (b"sec-webtransport-http3-draft02", b"1"),
    ];
    const VALUES: [&[u8]; 5] = [b"\"moqt-16\"", b"(\"x\"", b"\"", b"", b"GET"];
    for _ in 0..ROUNDS {
        let (mut pipe, mut c_h3, mut s_h3) = crate::test_utils::_init_h3_pipe();
        let mut s_wt = crate::Connection::new(true);
        let mut headers = Vec::new();
        for &(name, value) in &VALID {
            match rng.below(8) {
                0 => {}
                1 => headers.push(quiche::h3::Header::new(name, &rng.bytes(12))),
                2 => headers.push(quiche::h3::Header::new(name, VALUES[rng.below(VALUES.len() as u64) as usize])),
                _ => headers.push(quiche::h3::Header::new(name, value)),
            }
        }
        let _ = c_h3.send_request(&mut pipe.client, &headers, rng.below(2) == 0);
        for _ in 0..2 {
            let _ = pipe.advance();
            s_wt.poll_h3(&mut s_h3, &mut pipe.server);
            s_wt.poll(&mut s_h3, &mut pipe.server);
        }
    }
}
