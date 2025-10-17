use std::sync::OnceLock;
use boring::asn1::Asn1Time;
use boring::hash::MessageDigest;
use boring::pkey::{PKey, Private};
use boring::rsa::Rsa;
use boring::ssl::{SslContextBuilder, SslMethod};
use boring::x509::{X509NameBuilder, X509};
use boring::x509::extension::SubjectAlternativeName;
use quiche::test_utils::Pipe;
use quiche::{h3, Config, PROTOCOL_VERSION};
use quiche_h3_utils::ALPN_HTTP_3;

/// generate private and public key pair for testing
pub fn key_pair() -> &'static (PKey<Private>, X509) {
    static KEY_PAIR: OnceLock<(PKey<Private>, X509)> = OnceLock::new();
    KEY_PAIR.get_or_init(|| {
        let rsa = Rsa::generate(2048).unwrap();
        let pkey = PKey::from_rsa(rsa).unwrap();

        // Build X.509 certificate
        let mut name_builder = X509NameBuilder::new().unwrap();
        name_builder.append_entry_by_text("CN", "localhost").unwrap();

        let mut builder = X509::builder().unwrap();
        builder.set_version(2).unwrap();
        builder.set_pubkey(&pkey).unwrap();

        builder.set_not_before(Asn1Time::days_from_now(0).unwrap().as_ref()).unwrap();
        builder.set_not_after(Asn1Time::days_from_now(365).unwrap().as_ref()).unwrap();

        let san = SubjectAlternativeName::new()
            .dns("localhost")
            .build(&builder.x509v3_context(None, None))
            .unwrap();
        builder.append_extension(san).unwrap();

        builder.sign(&pkey, MessageDigest::sha256()).unwrap();
        let cert = builder.build();

        (pkey, cert)
    })
}

pub fn _init_webtransport_pipe() -> (
    Pipe,
    h3::Connection,
    crate::Connection,
    h3::Connection,
    crate::Connection,
    crate::SessionId,
) {
    let mut pipe = Pipe::with_config(&mut {
        let (key, cert) = key_pair();
        let mut c = Config::with_boring_ssl_ctx_builder(PROTOCOL_VERSION, {
            let mut b = SslContextBuilder::new(SslMethod::tls()).unwrap();
            b.set_private_key(key).unwrap();
            b.set_certificate(cert).unwrap();
            b
        }).unwrap();
        c.set_initial_max_streams_uni(5);
        c.set_initial_max_streams_bidi(2);
        c.set_initial_max_data(10000000);
        c.set_initial_max_stream_data_bidi_remote(1000000);
        c.set_initial_max_stream_data_bidi_local(1000000);
        c.set_initial_max_stream_data_uni(1000000);
        c.set_application_protos(&[ALPN_HTTP_3]).unwrap();
        c.verify_peer(false);
        c
    })
        .unwrap();

    pipe.handshake().unwrap();

    assert!(pipe.client.is_established());
    assert_eq!(pipe.client.application_proto(), ALPN_HTTP_3);

    let mut c_h3 = h3::Connection::with_transport(&mut pipe.client, &{
        let mut c = h3::Config::new().unwrap();
        crate::configure_h3(&mut c).unwrap();
        c
    })
        .unwrap();

    assert!(pipe.server.is_established());
    assert_eq!(pipe.server.application_proto(), ALPN_HTTP_3);

    let mut s_h3 = h3::Connection::with_transport(&mut pipe.server, &{
        let mut c = h3::Config::new().unwrap();
        crate::configure_h3(&mut c).unwrap();
        c
    })
        .unwrap();

    pipe.advance().unwrap();

    assert!(matches!(c_h3.poll(&mut pipe.client), Err(h3::Error::Done)));
    assert!(matches!(s_h3.poll(&mut pipe.server), Err(h3::Error::Done)));
    assert!(crate::webtransport_enabled_by_server(&c_h3));

    let mut c_wt = crate::Connection::new();
    let wt_session_id = c_wt.connect_session(
        &mut c_h3,
        &mut pipe.client,
        "https://example.org/".parse().unwrap(),
    );

    let mut s_wt = crate::Connection::new();

    pipe.advance().unwrap();

    let Ok((stream_id, h3::Event::Headers { list, .. })) = s_h3.poll(&mut pipe.server) else {
        unreachable!()
    };
    s_wt.recv_hdrs(stream_id, &list);
    assert!(s_wt.established(wt_session_id));
    s_wt.poll(&mut s_h3, &mut pipe.server);

    pipe.advance().unwrap();

    let Ok((stream_id, h3::Event::Headers { list, .. })) = c_h3.poll(&mut pipe.client) else {
        unreachable!()
    };
    c_wt.recv_hdrs(stream_id, &list);
    assert!(c_wt.established(wt_session_id));

    (pipe, c_h3, c_wt, s_h3, s_wt, wt_session_id)
}
