extern crate core;

mod error;
mod in_stream;
mod in_track;
mod out_stream;
mod out_track;
mod pending_subscribe;
mod session;
mod config;
pub mod test_utils;

//reexport dependency
pub use quiche_moq_wire as wire;

pub use config::Config;
pub use error::Error;
pub use error::Result;
pub use session::MoqTransportSession;
pub use session::PublishStatus;
pub use session::SubscriptionRequestAction;
pub use quiche_utils::stream_id::StreamID;


#[cfg(test)]
mod test {
    use crate::test_utils::_init_moq_pipe;
    use crate::Config;
    use quiche::h3;
    use quiche_moq_wire::{MOQ_VERSION_DRAFT_07, MOQ_VERSION_DRAFT_08, MOQ_VERSION_DRAFT_09, MOQ_VERSION_DRAFT_10, MOQ_VERSION_DRAFT_11, MOQ_VERSION_DRAFT_12, MOQ_VERSION_DRAFT_13, MOQ_VERSION_DRAFT_14, MOQ_VERSION_DRAFT_15, MOQ_VERSION_DRAFT_16, Version};

    macro_rules! test_webtransport_moq_versions {
        ($($name:ident: $version:expr,)*) => {
        $(
            #[test]
            fn $name() {
                test_webtransport_moq($version);
            }
        )*
        }
    }

    test_webtransport_moq_versions! {
        test_webtransport_moq_draft07: MOQ_VERSION_DRAFT_07,
        test_webtransport_moq_draft08: MOQ_VERSION_DRAFT_08,
        test_webtransport_moq_draft09: MOQ_VERSION_DRAFT_09,
        test_webtransport_moq_draft10: MOQ_VERSION_DRAFT_10,
        test_webtransport_moq_draft11: MOQ_VERSION_DRAFT_11,
        test_webtransport_moq_draft12: MOQ_VERSION_DRAFT_12,
        test_webtransport_moq_draft13: MOQ_VERSION_DRAFT_13,
        test_webtransport_moq_draft14: MOQ_VERSION_DRAFT_14,
        test_webtransport_moq_draft15: MOQ_VERSION_DRAFT_15,
        test_webtransport_moq_draft16: MOQ_VERSION_DRAFT_16,
        //test_webtransport_moq_lite01: MOQ_VERSION_LITE_01_BY_KIXELATED,
    }

    fn test_webtransport_moq(version: Version) {
        let mut config: Config = Default::default();
        config.setup_version = version;

        let (mut pipe, mut c_h3, mut c_wt, mut c_moq, mut s_h3, mut s_wt, mut s_moq) = _init_moq_pipe(config);

        c_moq.subscribe(
            &"n1--t1".parse().unwrap(),
            &mut c_wt,
            &mut pipe.client,
        ).unwrap();

        pipe.advance().unwrap();

        s_moq.poll(&mut s_wt, &mut s_h3, &mut pipe.server);
        let (request_id, _subscription) = s_moq.subscription_inbox_next().unwrap();
        let track_alias = s_moq.accept_subscription(*request_id, None, &mut s_wt, &mut pipe.server);
        s_moq
            .send_obj(
                b"hello",
                track_alias,
                &mut s_wt,
                &mut s_h3,
                &mut pipe.server,
            )
            .unwrap();

        pipe.advance().unwrap();

        assert!(matches!(c_h3.poll(&mut pipe.client), Err(h3::Error::Done)));
        c_wt.poll(&mut c_h3, &mut pipe.client);
        c_moq.poll(&mut c_wt, &mut c_h3, &mut pipe.client);
        let track_alias = *c_moq.readable().first().unwrap();
        let _hdr = c_moq
            .read_obj_hdr(track_alias, &mut c_wt, &mut c_h3, &mut pipe.client)
            .unwrap();
        let mut buf = [0u8; 10];
        let n = c_moq
            .read_obj_pld(
                &mut buf,
                track_alias,
                &mut c_wt,
                &mut c_h3,
                &mut pipe.client,
            )
            .unwrap();
        let pld = &buf[..n];
        assert_eq!(&pld, b"hello");
    }
}
