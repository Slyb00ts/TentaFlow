// =============================================================================
// Plik: tests/criterion_a_accept_connect.rs
// Opis: Iroh kryterium (a) — sanity check ze iroh w ogole sie linkuje i
//       NodeId mozna utworzyc. Pelny accept/connect test wymaga uzgodnienia
//       z aktualnym iroh::net::Endpoint API (pre-1.0, czeste zmiany).
//
//       Status: scaffold. TODO: po stabilizacji iroh API albo decyzji o
//       innym version pin, dopisac real network round-trip test.
// =============================================================================

use tentaflow_iroh_spike::{accept_connect, MESH_ALPN};

#[test]
fn iroh_links_and_creates_node_id() {
    assert!(accept_connect::iroh_module_loads());
}

#[tokio::test]
async fn build_endpoint_stub_returns_ok() {
    assert!(accept_connect::build_endpoint_stub().await.is_ok());
}

#[tokio::test]
#[ignore = "network test — wymaga real UDP bind, uruchamiac --ignored"]
async fn endpoint_round_trip() {
    let server = accept_connect::build_endpoint().await.expect("server endpoint");
    let server_addr = server
        .node_addr()
        .await
        .expect("server node addr");

    let server_handle = tokio::spawn(async move {
        if let Some(incoming) = server.accept().await {
            let conn = incoming.await.expect("connection");
            let (mut send, mut recv) = conn.accept_bi().await.expect("accept_bi");
            let bytes = recv.read_to_end(1024).await.expect("read");
            send.write_all(&bytes).await.expect("write echo");
            send.finish().expect("finish");
            let _ = conn.closed().await;
        }
    });

    let client = accept_connect::build_endpoint().await.expect("client endpoint");
    let conn = client
        .connect(server_addr, MESH_ALPN)
        .await
        .expect("connect");
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
    send.write_all(b"hello iroh").await.expect("write");
    send.finish().expect("finish");
    let response = recv.read_to_end(64).await.expect("read echo");
    assert_eq!(response, b"hello iroh");
    conn.close(0u32.into(), b"ok");

    server_handle.await.expect("server task");
}
