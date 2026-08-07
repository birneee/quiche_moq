//! Non-working sketch — designs the target syntax before anything behind it
//! exists. `MoqServer`, `MoqSession`, `MoqTrack`, `Publish` are all stubs.

use moq_tokio::{MoqServer, MoqSession};

#[tokio::test]
async fn subscribe_receives_objects() {
    let _ = env_logger::builder().is_test(true).try_init();

    // create server
    let mut server = MoqServer::listen("127.0.0.1:0").await.unwrap();
    let addr = server.local_addr();

    // connect client via udp loopback
    let (session, receive_loop): (MoqSession, _) = MoqSession::connect(addr, moq_tokio::config()).await.unwrap();
    tokio::spawn(receive_loop);

    // client send subscription — doesn't resolve until the server accepts
    // it below, so run it concurrently rather than awaiting it inline here.
    let track_handle = tokio::spawn(async move { session.subscribe("namespace--track").await });

    // server verifies the request before accepting it — e.g. checking the
    // track is one it actually has, same as moq_core's own accept/reject
    // split (see `SubscriptionRequestAction` elsewhere in this workspace).
    let request = server.next_subscription_request().await.unwrap();
    assert_eq!(request.namespace_trackname(), "namespace--track");
    let publish = request.accept().await.unwrap();

    let mut track = track_handle.await.unwrap().unwrap();

    // server send "hello" and then "world" as separate objects
    publish.send_object(b"hello").await.unwrap();
    publish.send_object(b"world").await.unwrap();

    assert_eq!(track.next_object().await.unwrap().payload, b"hello");
    assert_eq!(track.next_object().await.unwrap().payload, b"world");
}