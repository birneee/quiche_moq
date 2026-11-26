//! End-to-end test of `moq_tokio`'s public API, exercised over a real
//! WebTransport-over-QUIC connection.

use std::future::Future;
use std::time::Duration;

use moq_tokio::{MoqServer, MoqSession};

/// Fails a test that hangs, e.g. on a lost reply, instead of blocking the run.
async fn within_deadline<T>(test: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(30), test).await.expect("test deadline")
}

#[tokio::test]
async fn subscribe_receives_objects() {
    let _ = env_logger::builder().is_test(true).try_init();
    within_deadline(async {
        // create server
        let mut server = MoqServer::listen("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr();

        // connect client via udp loopback
        let (session, receive_loop): (MoqSession, _) = MoqSession::connect(addr, moq_tokio::config()).await.unwrap();
        tokio::spawn(receive_loop);

        // client send subscription — doesn't resolve until the server accepts
        // it below, so run it concurrently rather than awaiting it inline here.
        let subscriber = session.clone();
        let track_handle = tokio::spawn(async move { subscriber.subscribe("namespace--track").await });

        // server verifies the request before accepting it — e.g. checking the
        // track is one it actually has
        let request = server.next_subscription_request().await.unwrap();
        assert_eq!(request.namespace_trackname(), "namespace--track");
        let publish = request.accept().await.unwrap();

        let mut track = track_handle.await.unwrap().unwrap();

        // server send "hello" and then "world" as separate objects
        publish.send_object(b"hello").await.unwrap();
        publish.send_object(b"world").await.unwrap();

        assert_eq!(track.next_object().await.unwrap().payload, b"hello");
        assert_eq!(track.next_object().await.unwrap().payload, b"world");

        let stats = track.objects.stats();
        assert_eq!((stats.objects, stats.bytes, stats.discarded, stats.skipped, stats.ready), (2, 10, 0, 0, 0));
        assert_eq!(session.stats().received_tracks, 1);
    }).await
}

#[tokio::test]
async fn dropped_request_is_rejected_without_other_traffic() {
    within_deadline(async {
        let mut server = MoqServer::listen("127.0.0.1:0").await.unwrap();
        let (session, receive_loop): (MoqSession, _) = MoqSession::connect(server.local_addr(), moq_tokio::config()).await.unwrap();
        tokio::spawn(receive_loop);
        let track = tokio::spawn(async move { session.subscribe("namespace--track").await });

        drop(server.next_subscription_request().await.unwrap());

        let response = tokio::time::timeout(Duration::from_secs(5), track).await.expect("rejection must not wait for other traffic");
        assert!(matches!(response.unwrap(), Err(moq_tokio::SubscribeError::Rejected(_))));
    }).await
}
