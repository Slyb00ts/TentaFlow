// =============================================================================
// Plik: tests/criterion_a_accept_connect.rs
// Opis: Iroh kryterium (a) — sanity check ze iroh w ogole sie linkuje i
//       NodeId mozna utworzyc. Pelny accept/connect test wymaga uzgodnienia
//       z aktualnym iroh::net::Endpoint API (pre-1.0, czeste zmiany).
//
//       Status: scaffold. TODO: po stabilizacji iroh API albo decyzji o
//       innym version pin, dopisac real network round-trip test.
// =============================================================================

use tentaflow_iroh_spike::accept_connect;

#[test]
fn iroh_links_and_creates_node_id() {
    assert!(accept_connect::iroh_module_loads());
}

#[tokio::test]
async fn build_endpoint_stub_returns_ok() {
    assert!(accept_connect::build_endpoint_stub().await.is_ok());
}
