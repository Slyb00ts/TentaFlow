// =============================================================================
// Plik: sync/baseline_transport/tests.rs
// Opis: Testy maszyny stanow transportu baseline-adopt. Joiner i donor biegna
//       jednoczesnie nad in-memory duplex strumieniem (`DuplexFrameStream`) — bez
//       prawdziwej sieci — wiec cala sekwencja (elect -> ack -> header -> chunki
//       -> import) jest realnie przetestowana. Pokrywa: happy-path do Completed
//       po obu stronach, mismatch w BaselineAck, oraz uszkodzony chunk.
// =============================================================================

use super::*;
use crate::crypto::SettingsCipher;
use crate::db::{self, DbPool};
use crate::mesh::security::MeshSecurity;
use crate::sync::core_baseline::{load_adopt_state, BaselinePhase, BaselineRole};
use std::sync::Arc;
use tentaflow_protocol::mesh::BaselineEpoch;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

// =============================================================================
// Fake stream (in-memory duplex)
// =============================================================================

/// `FrameStream` nad tokio `DuplexStream` — ten sam len-prefixed CBOR wire format
/// co iroh, ale w pamieci. Pozwala uruchomic joiner i donor jako dwa taski bez
/// gniazd.
struct DuplexFrameStream {
    inner: DuplexStream,
}

impl DuplexFrameStream {
    fn pair() -> (Self, Self) {
        let (a, b) = tokio::io::duplex(1024 * 1024);
        (Self { inner: a }, Self { inner: b })
    }
}

#[async_trait]
impl FrameStream for DuplexFrameStream {
    async fn read_raw(&mut self, label: &str) -> LedgerResult<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.inner
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| transport_err(label, format!("read len: {e}")))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_BASELINE_FRAME_BYTES {
            return Err(transport_err(
                label,
                format!("frame too large: {len} bytes"),
            ));
        }
        let mut body = vec![0u8; len];
        self.inner
            .read_exact(&mut body)
            .await
            .map_err(|e| transport_err(label, format!("read body: {e}")))?;
        Ok(body)
    }

    async fn write_raw(&mut self, body: &[u8], label: &str) -> LedgerResult<()> {
        self.inner
            .write_all(&(body.len() as u32).to_be_bytes())
            .await
            .map_err(|e| transport_err(label, format!("write len: {e}")))?;
        self.inner
            .write_all(body)
            .await
            .map_err(|e| transport_err(label, format!("write body: {e}")))?;
        Ok(())
    }

    async fn finish(&mut self) -> LedgerResult<()> {
        self.inner
            .shutdown()
            .await
            .map_err(|e| transport_err("finish", format!("{e}")))?;
        Ok(())
    }
}

// =============================================================================
// Fixtures
// =============================================================================

fn test_cipher() -> Arc<SettingsCipher> {
    Arc::new(SettingsCipher::new(&[7u8; 32]))
}

/// Goly pool z wyczyszczonymi seedami platformowymi — single-org fixture.
fn new_pool() -> DbPool {
    let pool = db::init(std::path::Path::new(":memory:")).expect("init test DB");
    {
        let conn = pool.write().unwrap();
        for table in [
            "node_user_assignments",
            "user_identity_keys",
            "sync_explicit_shares",
            "org_memberships",
            "sync_user_org_profiles",
            "sync_resource_acl",
            "sync_policies",
            "group_members",
            "flow_model_bindings",
            "flows",
            "user_groups",
            "sync_nodes",
            "user_accounts",
            "roles",
            "organizations",
        ] {
            conn.execute(&format!("DELETE FROM {table}"), []).unwrap();
        }
    }
    pool
}

fn seed_donor_org(pool: &DbPool) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO organizations \
            (org_id, name, slug, status, created_at) \
         VALUES ('org-donor', 'Donor Org', 'donor', 'active', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES ('role-user', 'user', '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_accounts \
            (id, username, password_hash, display_name, email, is_active, is_admin, role) \
         VALUES ('u-donor-1', 'donor_user', 'hash', 'Donor User', 'donor@example.com', 1, 0, 'user')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO org_memberships \
            (org_id, user_id, role_id, granted_at, granted_by) \
         VALUES ('org-donor', 'u-donor-1', 'role-user', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'seed')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO flows \
            (id, name, description, is_default, service_type, flow_json, status) \
         VALUES ('f-donor', 'Flow', NULL, 0, NULL, '{}', 'active')",
        [],
    )
    .unwrap();
}

/// Joiner z wlasna org + userem o INNYM emailu (zostanie dolaczony do org dawcy).
fn seed_joiner_org(pool: &DbPool) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO organizations \
            (org_id, name, slug, status, created_at) \
         VALUES ('org-joiner', 'Joiner Org', 'joiner', 'active', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO roles (role_id, name, permissions_json, created_at) \
         VALUES ('role-user', 'user', '[]', strftime('%Y-%m-%dT%H:%M:%SZ','now'))",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO user_accounts \
            (id, username, password_hash, display_name, email, is_active, is_admin, role) \
         VALUES ('u-joiner-1', 'joiner_user', 'hash', 'Joiner User', 'joiner@example.com', 1, 0, 'user')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO org_memberships \
            (org_id, user_id, role_id, granted_at, granted_by) \
         VALUES ('org-joiner', 'u-joiner-1', 'role-user', strftime('%Y-%m-%dT%H:%M:%SZ','now'), 'seed')",
        [],
    )
    .unwrap();
}

/// Wstawia `peer_node_id` do `trusted_nodes` BEZPOSREDNIO (bez `add_trusted_key`,
/// ktory odpala `ensure_default_core_sync_policies` z FK na domyslna org skasowana
/// przez `new_pool`). `MeshSecurity::new` wczyta ten wiersz do mapy trusted, wiec
/// `is_trusted` zwroci true bez side-effectow naruszajacych FK fixture'a.
fn insert_trusted_node(pool: &DbPool, node_id: &str, public_key_hex: &str) {
    let conn = pool.write().unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO trusted_nodes \
            (node_id, public_key, hostname, approved_by, approved_at, is_active) \
         VALUES (?1, ?2, 'joiner-host', 'test', datetime('now'), 1)",
        rusqlite::params![node_id, public_key_hex],
    )
    .unwrap();
}

/// Buduje `MeshSecurity` dawcy z `peer_node_id` juz w `trusted_nodes`. Zwraca
/// `(security, donor_node_id)`. `donor_node_id` to ed25519 hex tozsamosci dawcy.
fn donor_security(
    pool: DbPool,
    peer_node_id: &str,
    peer_pubkey_hex: &str,
) -> (Arc<MeshSecurity>, String) {
    insert_trusted_node(&pool, peer_node_id, peer_pubkey_hex);
    let security = MeshSecurity::new(pool, test_cipher()).expect("security new");
    let donor_node_id = security.ed25519_public_key_hex();
    (Arc::new(security), donor_node_id)
}

/// Generuje realny Ed25519 klucz i zwraca `(node_id_hex, public_key_hex)` gdzie
/// `public_key_hex` ma 128 hex znakow (ed25519 || x25519 placeholder), zgodnie z
/// `PUBLIC_KEY_HEX_LEN`. Pierwsze 64 hex == node_id.
fn gen_identity() -> (String, String) {
    let signing = ed25519_dalek::SigningKey::generate(&mut rand_core_06::OsRng);
    let node_id = hex::encode(signing.verifying_key().as_bytes());
    let public_key_hex = format!("{node_id}{}", "00".repeat(32));
    (node_id, public_key_hex)
}

// =============================================================================
// Testy
// =============================================================================

#[tokio::test]
async fn happy_path_joiner_and_donor_reach_completed() {
    let (joiner_node_id, joiner_pubkey) = gen_identity();

    // Donor pool seedowany; joiner pool z wlasna org.
    let donor_pool = new_pool();
    seed_donor_org(&donor_pool);
    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);

    let (security, donor_node_id) = donor_security(donor_pool, &joiner_node_id, &joiner_pubkey);

    // Joiner musi wyjsc jako joiner — wymuszamy relacje przez wybor node_id.
    // decide_roles: nizszy node_id jest dawca. donor_node_id pochodzi z losowego
    // klucza; wymuszamy by joiner_node_id > donor_node_id nie jest gwarantowane,
    // wiec nadpisujemy local id dawcy nizszym, a joinera wyzszym poprzez fakt, ze
    // donor zna wlasny ed25519 hex. Sprawdzamy elekcje i jak trzeba zamieniamy.
    // Tu po prostu liczymy faktyczne role i asercja sprawdza spojnosc.
    let (donor, joiner) = decide_roles(&donor_node_id, &joiner_node_id, None);
    // Test wymaga by donor_node_id byl dawca. Jesli losowo wyszlo odwrotnie,
    // pomijamy (deterministyczna elekcja jest osobno testowana w core_baseline).
    if donor != donor_node_id {
        // Zamieniamy role: powtorka z wymuszeniem niemozliwa bez kontroli kluczy,
        // wiec generujemy do skutku.
        return reroll_happy_path().await;
    }
    assert_eq!(joiner, joiner_node_id);

    let (mut joiner_stream, mut donor_stream) = DuplexFrameStream::pair();
    let cipher = test_cipher();

    let donor_task = {
        let security = Arc::clone(&security);
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            run_donor_session(
                &mut donor_stream,
                &security,
                &donor_node_id,
                &joiner_node_id,
            )
            .await
        })
    };

    let joiner_pool_for_task = joiner_pool.clone();
    let donor_node_id_for_joiner = donor_node_id.clone();
    let joiner_task = tokio::spawn(async move {
        run_joiner_session(
            &mut joiner_stream,
            &joiner_pool_for_task,
            &joiner_node_id,
            &donor_node_id_for_joiner,
            &cipher,
            0,
        )
        .await
    });

    let donor_res = donor_task.await.expect("donor task join");
    let joiner_res = joiner_task.await.expect("joiner task join");

    donor_res.expect("donor session ok");
    let report = joiner_res.expect("joiner session ok");
    assert_eq!(report.donor_org_id, "org-donor");

    // Joiner ma teraz org dawcy.
    {
        let conn = joiner_pool.read().unwrap();
        let has_donor_org: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM organizations WHERE org_id = 'org-donor')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_donor_org, "joiner adopted donor org");
        let has_donor_user: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM user_accounts WHERE id = 'u-donor-1')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_donor_user, "joiner adopted donor user");
    }

    // Obie strony: faza Completed.
    let donor_state = load_adopt_state(&security.db)
        .unwrap()
        .expect("donor state");
    assert_eq!(donor_state.role, BaselineRole::Donor);
    assert_eq!(donor_state.phase, BaselinePhase::Completed);

    let joiner_state = load_adopt_state(&joiner_pool)
        .unwrap()
        .expect("joiner state");
    assert_eq!(joiner_state.role, BaselineRole::Joiner);
    assert_eq!(joiner_state.phase, BaselinePhase::Completed);
}

/// Pomocniczy re-roll: generuje tozsamosci do skutku tak, by `donor_node_id` byl
/// leksykograficznie nizszy (dawca). Wywolywany gdy pierwszy losowy klucz wyszedl
/// odwrotnie — gwarantuje deterministyczny przebieg happy-path bez petli w tescie
/// glownym.
async fn reroll_happy_path() {
    // Generuj joiner key, potem donor security do skutku az donor < joiner.
    let (joiner_node_id, joiner_pubkey) = gen_identity();

    loop {
        let donor_pool = new_pool();
        seed_donor_org(&donor_pool);
        // Tymczasowy security tylko po to, by poznac donor_node_id z losowego
        // klucza ed25519 zapisanego w tym poolu.
        let probe = MeshSecurity::new(donor_pool.clone(), test_cipher()).expect("security probe");
        let donor_node_id = probe.ed25519_public_key_hex();
        drop(probe);
        if donor_node_id >= joiner_node_id {
            continue;
        }
        let (security, donor_node_id) = donor_security(donor_pool, &joiner_node_id, &joiner_pubkey);

        let joiner_pool = new_pool();
        seed_joiner_org(&joiner_pool);

        let (mut joiner_stream, mut donor_stream) = DuplexFrameStream::pair();
        let cipher = test_cipher();

        let donor_task = {
            let security = Arc::clone(&security);
            let donor_node_id = donor_node_id.clone();
            let joiner_node_id = joiner_node_id.clone();
            tokio::spawn(async move {
                run_donor_session(
                    &mut donor_stream,
                    &security,
                    &donor_node_id,
                    &joiner_node_id,
                )
                .await
            })
        };
        let joiner_pool_for_task = joiner_pool.clone();
        let donor_node_id_for_joiner = donor_node_id.clone();
        let jni = joiner_node_id.clone();
        let joiner_task = tokio::spawn(async move {
            run_joiner_session(
                &mut joiner_stream,
                &joiner_pool_for_task,
                &jni,
                &donor_node_id_for_joiner,
                &cipher,
                0,
            )
            .await
        });

        donor_task.await.unwrap().expect("donor ok");
        let report = joiner_task.await.unwrap().expect("joiner ok");
        assert_eq!(report.donor_org_id, "org-donor");

        let joiner_state = load_adopt_state(&joiner_pool)
            .unwrap()
            .expect("joiner state");
        assert_eq!(joiner_state.phase, BaselinePhase::Completed);
        return;
    }
}

/// Regression: a mandated donor (epoch-reconcile / admin-forced adopt) must win
/// the election even when the JOINER's node_id is lexicographically lower. Before
/// the explicit-proposal fix the joiner aborted pre-flight with "local election
/// disagrees" and the mesh could never converge.
#[tokio::test]
async fn joiner_with_lower_node_id_completes_full_session() {
    let donor_pool = new_pool();
    seed_donor_org(&donor_pool);
    // Probe security only to learn the donor's random ed25519 node_id; the key is
    // persisted in the pool, so `donor_security` below yields the same id.
    let probe = MeshSecurity::new(donor_pool.clone(), test_cipher()).expect("security probe");
    let donor_node_id = probe.ed25519_public_key_hex();
    drop(probe);

    // Force the failing ordering: joiner id strictly LOWER than the donor's.
    let (joiner_node_id, joiner_pubkey) = loop {
        let (id, pk) = gen_identity();
        if id < donor_node_id {
            break (id, pk);
        }
    };

    let (security, donor_node_id) = donor_security(donor_pool, &joiner_node_id, &joiner_pubkey);
    assert!(joiner_node_id < donor_node_id);

    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);

    let (mut joiner_stream, mut donor_stream) = DuplexFrameStream::pair();
    let cipher = test_cipher();

    let donor_task = {
        let security = Arc::clone(&security);
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            run_donor_session(
                &mut donor_stream,
                &security,
                &donor_node_id,
                &joiner_node_id,
            )
            .await
        })
    };

    let joiner_pool_for_task = joiner_pool.clone();
    let donor_node_id_for_joiner = donor_node_id.clone();
    let joiner_task = tokio::spawn(async move {
        run_joiner_session(
            &mut joiner_stream,
            &joiner_pool_for_task,
            &joiner_node_id,
            &donor_node_id_for_joiner,
            &cipher,
            0,
        )
        .await
    });

    donor_task.await.unwrap().expect("donor session ok");
    let report = joiner_task
        .await
        .unwrap()
        .expect("joiner with lower node_id must complete the session");
    assert_eq!(report.donor_org_id, "org-donor");

    let donor_state = load_adopt_state(&security.db)
        .unwrap()
        .expect("donor state");
    assert_eq!(donor_state.role, BaselineRole::Donor);
    assert_eq!(donor_state.phase, BaselinePhase::Completed);

    let joiner_state = load_adopt_state(&joiner_pool)
        .unwrap()
        .expect("joiner state");
    assert_eq!(joiner_state.role, BaselineRole::Joiner);
    assert_eq!(joiner_state.phase, BaselinePhase::Completed);
}

#[tokio::test]
async fn joiner_aborts_on_ack_role_mismatch() {
    let (joiner_node_id, _) = gen_identity();
    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);
    let cipher = test_cipher();

    // Fake donor pisze BaselineAck o NIEZGODNYM donorze. Joiner musi przerwac.
    let donor_node_id = {
        // gwarantuj relacje: donor < joiner aby decide_roles ustalil donora = peer
        let mut d;
        loop {
            let (cand, _) = gen_identity();
            if cand < joiner_node_id {
                d = cand;
                break;
            }
        }
        d
    };

    let (mut joiner_stream, mut fake_donor) = DuplexFrameStream::pair();

    let fake = {
        let donor_node_id = donor_node_id.clone();
        tokio::spawn(async move {
            // odbierz Elect, odeslij zly Ack (inny donor)
            let _elect: BaselineElect = read_frame(&mut fake_donor, "elect").await.unwrap();
            let bad_ack = BaselineAck {
                accepted: true,
                donor: format!("{donor_node_id}-WRONG"),
                joiner: "whoever".to_string(),
                epoch: 0,
            };
            write_frame(&mut fake_donor, &bad_ack, "ack").await.unwrap();
        })
    };

    let res = run_joiner_session(
        &mut joiner_stream,
        &joiner_pool,
        &joiner_node_id,
        &donor_node_id,
        &cipher,
        0,
    )
    .await;
    fake.await.unwrap();

    let err = res.expect_err("ack mismatch must abort");
    assert!(
        format!("{err}").contains("role mismatch"),
        "expected role mismatch error, got: {err}"
    );

    // Joiner NIE zaimportowal — wlasna org nietknieta, brak org dawcy.
    let conn = joiner_pool.read().unwrap();
    let has_joiner_org: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM organizations WHERE org_id = 'org-joiner')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(has_joiner_org, "joiner org must be untouched on abort");
}

#[tokio::test]
async fn joiner_detects_corrupted_chunk() {
    let (joiner_node_id, _) = gen_identity();
    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);
    let cipher = test_cipher();

    let donor_node_id = loop {
        let (cand, _) = gen_identity();
        if cand < joiner_node_id {
            break cand;
        }
    };

    // Donor pool do realnego snapshotu (capture wymaga tabel).
    let donor_pool = new_pool();
    seed_donor_org(&donor_pool);
    let snapshot = capture_baseline_snapshot(
        &donor_pool,
        BaselineEpoch {
            counter: 0,
            origin_node: donor_node_id.clone(),
        },
        &test_cipher(),
    )
    .unwrap();
    let raw = serialize_snapshot(&snapshot).unwrap();
    let header = build_baseline_header(&snapshot, &raw);
    let mut chunks = chunk_snapshot(&raw);
    assert!(!chunks.is_empty());

    let (mut joiner_stream, mut fake_donor) = DuplexFrameStream::pair();

    let fake = {
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            let _elect: BaselineElect = read_frame(&mut fake_donor, "elect").await.unwrap();
            let ack = BaselineAck {
                accepted: true,
                donor: donor_node_id.clone(),
                joiner: joiner_node_id.clone(),
                epoch: 0,
            };
            write_frame(&mut fake_donor, &ack, "ack").await.unwrap();
            write_frame(&mut fake_donor, &header, "header")
                .await
                .unwrap();
            // Uszkadzamy bajt w pierwszym chunku ZACHOWUJAC content_hash — joiner
            // wykryje to przez per-chunk hash przy skladaniu.
            if let Some(b) = chunks[0].bytes.first_mut() {
                *b ^= 0xFF;
            }
            for chunk in &chunks {
                write_frame(&mut fake_donor, chunk, "chunk").await.unwrap();
                let _ack: BaselineChunkAck =
                    read_frame(&mut fake_donor, "chunk_ack").await.unwrap();
            }
        })
    };

    let res = run_joiner_session(
        &mut joiner_stream,
        &joiner_pool,
        &joiner_node_id,
        &donor_node_id,
        &cipher,
        0,
    )
    .await;
    fake.await.unwrap();

    let err = res.expect_err("corrupted chunk must be detected");
    assert!(
        format!("{err}").contains("content hash mismatch"),
        "expected hash mismatch, got: {err}"
    );
}

#[tokio::test]
async fn joiner_rejects_header_over_hard_cap_without_buffering() {
    let (joiner_node_id, _) = gen_identity();
    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);
    let cipher = test_cipher();

    let donor_node_id = loop {
        let (cand, _) = gen_identity();
        if cand < joiner_node_id {
            break cand;
        }
    };

    let (mut joiner_stream, mut fake_donor) = DuplexFrameStream::pair();

    // Fake donor sends a valid Elect/Ack then a header declaring a total ABOVE the
    // local hard cap. If the joiner trusted the donor-declared limits it would start
    // buffering 64 KiB chunks; instead it must reject at the header and never read a
    // single chunk. We assert that by NEVER sending a chunk and checking the joiner
    // still aborts (so it cannot be blocked waiting for chunk bytes).
    let fake = {
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            let _elect: BaselineElect = read_frame(&mut fake_donor, "elect").await.unwrap();
            let ack = BaselineAck {
                accepted: true,
                donor: donor_node_id.clone(),
                joiner: joiner_node_id.clone(),
                epoch: 0,
            };
            write_frame(&mut fake_donor, &ack, "ack").await.unwrap();
            let header = BaselineHeader {
                schema_version: 1,
                epoch: 0,
                tables: Vec::new(),
                row_counts: Vec::new(),
                total_bytes: BASELINE_MAX_TOTAL_BYTES + 1,
                max_bytes: BASELINE_MAX_TOTAL_BYTES + 1,
                content_hash: [0u8; 32],
            };
            write_frame(&mut fake_donor, &header, "header")
                .await
                .unwrap();
            // Intentionally send NO chunks; the joiner must already be aborting.
        })
    };

    let res = run_joiner_session(
        &mut joiner_stream,
        &joiner_pool,
        &joiner_node_id,
        &donor_node_id,
        &cipher,
        0,
    )
    .await;
    fake.await.unwrap();

    let err = res.expect_err("oversized header must abort");
    assert!(
        format!("{err}").contains("local hard cap"),
        "expected local hard cap rejection, got: {err}"
    );

    // Joiner imported nothing — own org untouched, donor org absent.
    let conn = joiner_pool.read().unwrap();
    let has_joiner_org: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM organizations WHERE org_id = 'org-joiner')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(has_joiner_org, "joiner org must be untouched on abort");
}

#[tokio::test]
async fn joiner_aborts_when_stream_overshoots_declared_total() {
    let (joiner_node_id, _) = gen_identity();
    let joiner_pool = new_pool();
    seed_joiner_org(&joiner_pool);
    let cipher = test_cipher();

    let donor_node_id = loop {
        let (cand, _) = gen_identity();
        if cand < joiner_node_id {
            break cand;
        }
    };

    // Header declares a tiny total (1 chunk worth) but the donor streams a SECOND
    // chunk. The joiner must abort the moment the running byte count passes the
    // declared total — before it buffers the overshoot — proving memory is bounded
    // during reception, not only at reassembly.
    let chunk_bytes = vec![0xABu8; 4 * 1024];
    // Declare a total between one and two chunks so the receive loop keeps reading
    // after the first chunk and then overshoots on the second — exercising the
    // in-loop byte-count abort (memory bounded during reception), not reassembly.
    let declared_total = chunk_bytes.len() as u64 + 1;
    let chunk_hash = *blake3::hash(&chunk_bytes).as_bytes();
    let chunk0 = BaselineChunk {
        seq: 0,
        content_hash: chunk_hash,
        bytes: chunk_bytes.clone(),
    };
    let chunk1 = BaselineChunk {
        seq: 1,
        content_hash: chunk_hash,
        bytes: chunk_bytes,
    };
    let header = BaselineHeader {
        schema_version: 1,
        epoch: 0,
        tables: Vec::new(),
        row_counts: Vec::new(),
        total_bytes: declared_total,
        max_bytes: BASELINE_MAX_TOTAL_BYTES,
        content_hash: [0u8; 32],
    };

    let (mut joiner_stream, mut fake_donor) = DuplexFrameStream::pair();

    let fake = {
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            let _elect: BaselineElect = read_frame(&mut fake_donor, "elect").await.unwrap();
            let ack = BaselineAck {
                accepted: true,
                donor: donor_node_id.clone(),
                joiner: joiner_node_id.clone(),
                epoch: 0,
            };
            write_frame(&mut fake_donor, &ack, "ack").await.unwrap();
            write_frame(&mut fake_donor, &header, "header")
                .await
                .unwrap();
            // First chunk fills the declared total and is ACKed.
            write_frame(&mut fake_donor, &chunk0, "chunk")
                .await
                .unwrap();
            let _ack: BaselineChunkAck = read_frame(&mut fake_donor, "chunk_ack").await.unwrap();
            // Second chunk overshoots — joiner must NACK and abort.
            write_frame(&mut fake_donor, &chunk1, "chunk")
                .await
                .unwrap();
            let nack: BaselineChunkAck = read_frame(&mut fake_donor, "chunk_ack").await.unwrap();
            assert!(!nack.ok, "joiner must NACK the overshooting chunk");
        })
    };

    let res = run_joiner_session(
        &mut joiner_stream,
        &joiner_pool,
        &joiner_node_id,
        &donor_node_id,
        &cipher,
        0,
    )
    .await;
    fake.await.unwrap();

    let err = res.expect_err("stream overshoot must abort");
    assert!(
        format!("{err}").contains("exceeds header total_bytes"),
        "expected total_bytes overshoot rejection, got: {err}"
    );
}

#[tokio::test]
async fn donor_rejects_untrusted_peer() {
    let (joiner_node_id, joiner_pubkey) = gen_identity();
    let donor_pool = new_pool();
    seed_donor_org(&donor_pool);
    // Security BEZ dodania joinera do trusted.
    let security = Arc::new(MeshSecurity::new(donor_pool, test_cipher()).expect("security"));
    let donor_node_id = security.ed25519_public_key_hex();
    let _ = joiner_pubkey;

    let (mut joiner_stream, mut donor_stream) = DuplexFrameStream::pair();

    let donor_task = {
        let security = Arc::clone(&security);
        let donor_node_id = donor_node_id.clone();
        let joiner_node_id = joiner_node_id.clone();
        tokio::spawn(async move {
            run_donor_session(
                &mut donor_stream,
                &security,
                &donor_node_id,
                &joiner_node_id,
            )
            .await
        })
    };

    // Joiner-strona pisze Elect, odbiera NACK.
    let elect = BaselineElect {
        node_id: joiner_node_id.clone(),
        proposed_donor: String::new(),
        epoch_seen: 0,
        sender_op_count: 0,
    };
    write_frame(&mut joiner_stream, &elect, "elect")
        .await
        .unwrap();
    let ack: BaselineAck = read_frame(&mut joiner_stream, "ack").await.unwrap();
    assert!(!ack.accepted, "untrusted peer must get rejection");

    let donor_res = donor_task.await.unwrap();
    let err = donor_res.expect_err("donor must reject untrusted");
    assert!(
        format!("{err}").contains("untrusted"),
        "expected untrusted error, got: {err}"
    );
}
