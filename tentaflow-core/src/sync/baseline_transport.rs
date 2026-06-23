// =============================================================================
// Plik: sync/baseline_transport.rs
// Opis: Transport iroh dla baseline-adopt. Przenosi ramki CBOR
//       (BaselineElect/Ack/Header/Chunk/ChunkAck) przez strumien i wola GOTOWA,
//       przetestowana logike z `sync::core_baseline`. Maszyna stanow faz po obu
//       stronach (donor/joiner) plus crash-recovery wznawiane przy starcie.
//       Sama logika importu/snapshotu NIE jest tu duplikowana — to czysty
//       transport + sekwencjonowanie.
// =============================================================================
//
// Sekwencja (jeden bidirectional stream na adopcje):
//
//   JOINER (dialuje ALPN_BASELINE)         DONOR (akceptuje ALPN_BASELINE)
//   ------------------------------         ------------------------------
//   BaselineElect{node_id, donor, epoch} ->
//                                          decide_roles potwierdza ze to ON donor
//                                          begin_adopt_atomic(Donor, ...)
//                                          capture_baseline_snapshot + serialize
//                                       <- BaselineAck{accepted, donor, joiner, epoch}
//   validate_ack_agreement
//   faza Receiving                        <- BaselineHeader
//   petla:  <- BaselineChunk(seq)
//           -> BaselineChunkAck(seq)         (czeka na ACK kazdego chunka)
//   reassemble_chunks (weryfikuje rozmiar/seq/hash calosci)
//   faza Importing -> run_baseline_adopt
//   faza Completed                           faza Completed (donor nie importuje)
//
// Bezpieczenstwo: joiner weryfikuje ze remote node_id streamu == wybrany donor;
// donor weryfikuje ze remote == nadawca BaselineElect ORAZ ze peer jest trusted.
// Snapshot niesie odszyfrowane sekrety — NIGDY nie logowane.

use std::time::Duration;

use async_trait::async_trait;
use tentaflow_protocol::mesh::{
    BaselineAck, BaselineChunk, BaselineChunkAck, BaselineElect, BaselineHeader,
};
use tracing::{info, warn};

use crate::crypto::SettingsCipher;
use crate::db::DbPool;
use crate::mesh::security::MeshSecurity;
use crate::sync::core_baseline::{
    begin_adopt_atomic, build_baseline_header, capture_baseline_snapshot, chunk_snapshot,
    decide_roles, decide_roles_by_content, deserialize_snapshot, import_baseline, load_adopt_state,
    local_role,
    reassemble_chunks, serialize_snapshot, store_adopt_state, validate_ack_agreement,
    BaselineAdoptState, BaselineImportReport, BaselinePhase, BaselineRole, BeginOutcome,
    BASELINE_MAX_TOTAL_BYTES,
};
use crate::sync::ledger::{LedgerResult, SyncLedgerError};

/// Gorny limit pojedynczej ramki CBOR na strumieniu baseline. Naglowek i ramki
/// kontrolne (Elect/Ack/Header/ChunkAck) sa male; chunki snapshotu sa ciete do
/// `BASELINE_CHUNK_BYTES` (48 KiB) + narzut CBOR (seq + 32-bajtowy hash +
/// length-prefix), wiec 64 KiB miesci kazda ramke z zapasem.
const MAX_BASELINE_FRAME_BYTES: usize = 64 * 1024;

/// Timeout na pojedyncza operacje read/write ramki. Snapshot platformowy
/// single-org noda jest maly (KB-MB), wiec kazda ramka leci szybko; 30 s lapie
/// realny stall sieci zamiast wisiec w nieskonczonosc na martwym peerze. Wzorzec
/// jak `send.stopped()` timeout w pairing.rs, tyle ze per-ramka.
const FRAME_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Abstrakcja strumienia ramkowanego (len-prefixed CBOR). Dwie implementacje:
/// `IrohFrameStream` (realny bidirectional stream iroh) oraz `DuplexFrameStream`
/// w testach (in-memory tokio duplex). Dzieki temu maszyna stanow donor/joiner
/// jest testowalna bez prawdziwej sieci.
#[async_trait]
pub trait FrameStream: Send {
    /// Odczytuje pelna ramke (4-bajtowy big-endian len-prefix + body) jako
    /// surowe bajty CBOR. Egzekwuje `MAX_BASELINE_FRAME_BYTES`.
    async fn read_raw(&mut self, label: &str) -> LedgerResult<Vec<u8>>;
    /// Zapisuje pojedyncza ramke (len-prefix + body).
    async fn write_raw(&mut self, body: &[u8], label: &str) -> LedgerResult<()>;
    /// Sygnalizuje koniec wysylki (half-close) i czeka na potwierdzenie odbioru,
    /// by peer nie zgubil koncowki przy szybkim zamknieciu (jak pairing).
    async fn finish(&mut self) -> LedgerResult<()>;
}

fn transport_err(label: &str, msg: impl std::fmt::Display) -> SyncLedgerError {
    SyncLedgerError::Runtime(format!("baseline transport {label}: {msg}"))
}

async fn read_frame<T, S>(stream: &mut S, label: &str) -> LedgerResult<T>
where
    T: serde::de::DeserializeOwned,
    S: FrameStream + ?Sized,
{
    let body = stream.read_raw(label).await?;
    ciborium::de::from_reader(std::io::Cursor::new(body))
        .map_err(|e| SyncLedgerError::Decode(format!("baseline {label} CBOR decode: {e}")))
}

async fn write_frame<T, S>(stream: &mut S, value: &T, label: &str) -> LedgerResult<()>
where
    T: serde::Serialize,
    S: FrameStream + ?Sized,
{
    let mut body = Vec::new();
    ciborium::ser::into_writer(value, &mut body)
        .map_err(|e| SyncLedgerError::Codec(format!("baseline {label} CBOR encode: {e}")))?;
    if body.len() > MAX_BASELINE_FRAME_BYTES {
        return Err(transport_err(
            label,
            format!("frame too large: {} bytes", body.len()),
        ));
    }
    stream.write_raw(&body, label).await
}

// =============================================================================
// Strona joinera
// =============================================================================

/// Pelna sekwencja joinera nad jednym strumieniem. Zaklada ze single-flight stan
/// jest juz armed (faza `Elected` z `begin_baseline_adopt_after_confirm`), a
/// `donor_node_id` to remote node_id zweryfikowany przez wywolujacego (== donor
/// z elekcji ORAZ == iroh remote_id po stronie polaczenia). Wykonuje:
/// Elect -> Ack(validate) -> Receiving -> Header -> chunki(+ack) ->
/// reassemble -> run_baseline_adopt -> Completed.
pub async fn run_joiner_session<S: FrameStream>(
    stream: &mut S,
    db: &DbPool,
    local_node_id: &str,
    donor_node_id: &str,
    cipher: &SettingsCipher,
    epoch_seen: u64,
) -> LedgerResult<BaselineImportReport> {
    // The caller mandates the donor (pairing confirm already elected it;
    // epoch-reconcile picked the winning-epoch carrier; admin ops name it
    // explicitly), so the joiner proposes it — the lowest-id election is only a
    // FALLBACK for proposal-less elects. Proposing keeps both sides converging on
    // the same (donor, joiner) pair regardless of node_id ordering; the guard
    // below is defense-in-depth and only fires if decide_roles rejects the
    // proposal.
    let (donor, joiner) = decide_roles(local_node_id, donor_node_id, Some(donor_node_id));
    if donor != donor_node_id {
        return Err(transport_err(
            "elect",
            format!(
                "local election disagrees: expected donor {donor_node_id}, computed {donor} \
                 (joiner {joiner})"
            ),
        ));
    }

    let elect = BaselineElect {
        node_id: local_node_id.to_string(),
        proposed_donor: donor.clone(),
        epoch_seen,
        // Advertise our content so the donor can settle the role data-aware: if we
        // (the dialer) actually hold MORE content than the peer we proposed as
        // donor, the donor refuses and our pull fails — the peer adopts from us
        // instead. This is what stops an empty node from donating over a populated
        // peer when both sides dial after auto-pairing.
        sender_op_count: crate::sync::runtime::local_op_count() as u64,
    };
    write_frame(stream, &elect, "elect").await?;

    let ack: BaselineAck = read_frame(stream, "ack").await?;
    validate_ack_agreement(&ack, &donor, &joiner, ack.epoch)?;

    let header: BaselineHeader = read_frame(stream, "header").await?;
    // Both `total_bytes` and `max_bytes` are donor-declared, so neither bounds the
    // joiner's memory. The only trustworthy limit is the LOCAL hard cap: reject the
    // header up front so a malicious-but-trusted donor cannot make us buffer chunks
    // toward an attacker-chosen total. We still keep the donor self-consistency check.
    if header.total_bytes > BASELINE_MAX_TOTAL_BYTES {
        return Err(transport_err(
            "header",
            format!(
                "declared snapshot {} bytes exceeds local hard cap {}",
                header.total_bytes, BASELINE_MAX_TOTAL_BYTES
            ),
        ));
    }
    if header.total_bytes > header.max_bytes {
        return Err(transport_err(
            "header",
            format!(
                "declared snapshot {} bytes exceeds donor max_bytes {}",
                header.total_bytes, header.max_bytes
            ),
        ));
    }

    // Liczba chunkow wynika z total_bytes / rozmiar chunka (ostatni krotszy).
    // Czytamy dopoki nie zlozymy zadeklarowanego rozmiaru; reassemble egzekwuje
    // ciaglosc seq, hashe i calkowity rozmiar.
    let mut chunks: Vec<BaselineChunk> = Vec::new();
    let mut received_bytes: u64 = 0;
    while received_bytes < header.total_bytes {
        let chunk: BaselineChunk = read_frame(stream, "chunk").await?;
        // Egzekwuj limit ZANIM odeslemy ACK — uszkodzony/zlosliwy donor nie moze
        // przekroczyc zadeklarowanego rozmiaru ANI lokalnego twardego capa. Cap
        // jest sprawdzany per-chunk, by pamiec nie urosla powyzej limitu nawet gdy
        // header sklamal o total_bytes wzgledem faktycznego strumienia.
        received_bytes = received_bytes.saturating_add(chunk.bytes.len() as u64);
        if received_bytes > BASELINE_MAX_TOTAL_BYTES {
            let ack = BaselineChunkAck {
                seq: chunk.seq,
                ok: false,
            };
            let _ = write_frame(stream, &ack, "chunk_ack").await;
            return Err(transport_err(
                "chunk",
                format!(
                    "received {} bytes exceeds local hard cap {}",
                    received_bytes, BASELINE_MAX_TOTAL_BYTES
                ),
            ));
        }
        if received_bytes > header.total_bytes {
            let ack = BaselineChunkAck {
                seq: chunk.seq,
                ok: false,
            };
            let _ = write_frame(stream, &ack, "chunk_ack").await;
            return Err(transport_err(
                "chunk",
                format!(
                    "received {} bytes exceeds header total_bytes {}",
                    received_bytes, header.total_bytes
                ),
            ));
        }
        let seq = chunk.seq;
        chunks.push(chunk);
        let chunk_ack = BaselineChunkAck { seq, ok: true };
        write_frame(stream, &chunk_ack, "chunk_ack").await?;
    }

    // Sklada + weryfikuje (rozmiar, seq, per-chunk hash, hash calosci). Uszkodzony
    // lub zgubiony chunk jest tu wykrywany — joiner NIGDY nie importuje czesciowego.
    let raw = reassemble_chunks(&chunks, &header)?;

    // Deserializuj raz, by poznac PELNY epoch dawcy (counter + origin_node) — ack
    // niesie tylko counter, a `import_baseline` keyuje single-flight na pelnym
    // `snapshot.epoch`. Przesuwamy stan do `Receiving` z TYM epochem, by faza
    // `Elected` (armed przy confirm z jeszcze-nieznanym epochem dawcy) nie
    // kolidowala z importem jako inny single-flight target. Po tym `import_baseline`
    // sam przesuwa faze do Importing/Imported/Completed atomowo — NIE duplikujemy.
    let snapshot = deserialize_snapshot(&raw)?;
    store_adopt_state(
        db,
        &BaselineAdoptState {
            role: BaselineRole::Joiner,
            peer: donor.clone(),
            epoch: snapshot.epoch.clone(),
            phase: BaselinePhase::Receiving,
        },
    )?;
    let report = import_baseline(db, &snapshot, donor_node_id, local_node_id, cipher)?;

    let _ = stream.finish().await;
    info!(
        donor = %donor_node_id,
        donor_org = %report.donor_org_id,
        chunks = chunks.len(),
        "baseline transport: joiner session completed"
    );
    Ok(report)
}

// =============================================================================
// Strona dawcy
// =============================================================================

/// Pelna sekwencja dawcy nad jednym akceptowanym strumieniem. `remote_node_id`
/// to iroh remote_id polaczenia (zweryfikowany przez wywolujacego). Wykonuje:
/// odbior Elect -> decide_roles potwierdza role dawcy -> begin_adopt_atomic(Donor)
/// -> capture/serialize -> Ack -> Header -> chunki(+czekaj na ack) -> Completed.
/// Donor NIE importuje.
pub async fn run_donor_session<S: FrameStream>(
    stream: &mut S,
    security: &MeshSecurity,
    local_node_id: &str,
    remote_node_id: &str,
) -> LedgerResult<()> {
    let elect: BaselineElect = read_frame(stream, "elect").await?;

    // Anti-spoof: nadawca Elect musi byc tym samym co iroh remote_id streamu.
    if elect.node_id != remote_node_id {
        return Err(transport_err(
            "elect",
            format!(
                "elect node_id {} does not match stream remote_id {remote_node_id}",
                elect.node_id
            ),
        ));
    }
    // Tylko trusted peer (po confirm pairingu) moze pobrac baseline — snapshot
    // niesie odszyfrowane sekrety i pelny stan org.
    if !security.is_trusted(remote_node_id) {
        let nack = BaselineAck {
            accepted: false,
            donor: local_node_id.to_string(),
            joiner: remote_node_id.to_string(),
            epoch: 0,
        };
        let _ = write_frame(stream, &nack, "ack").await;
        return Err(transport_err(
            "elect",
            format!("untrusted peer {remote_node_id} requested baseline"),
        ));
    }

    // Content-aware role decision. The node that HOLDS MORE content is the donor;
    // ties break on the lower node_id. We compare our own ledger op count against
    // the count the requester advertised in `BaselineElect`, so the empty node
    // adopts from the data-holder, never the reverse. If this makes the REQUESTER
    // the rightful donor (it holds more), we refuse: the requester's own
    // reciprocal pull (where it is the donor) is the one that must serve. Both
    // sides feed the identical two `(node_id, op_count)` pairs into the pure
    // decision, so they agree on a single donor without extra negotiation.
    let local_op_count = crate::sync::runtime::local_op_count() as u64;
    let (donor, joiner) = decide_roles_by_content(
        local_node_id,
        local_op_count,
        remote_node_id,
        elect.sender_op_count,
    );
    if donor != local_node_id {
        let nack = BaselineAck {
            accepted: false,
            donor: donor.clone(),
            joiner: joiner.clone(),
            epoch: 0,
        };
        let _ = write_frame(stream, &nack, "ack").await;
        return Err(transport_err(
            "elect",
            format!(
                "content election makes peer the donor (donor={donor}, local_ops={local_op_count}, \
                 peer_ops={}); refusing to donate",
                elect.sender_op_count
            ),
        ));
    }
    if local_role(local_node_id, &donor) != BaselineRole::Donor {
        return Err(transport_err("elect", "local_role disagrees with election"));
    }

    let epoch = crate::sync::runtime::core_epoch();

    // Single-flight: zajmij slot jako Donor. `Resume` (ten sam peer+epoch) tez
    // jest OK — donor moze ponawiac wysylke; capture jest idempotentne (read-only).
    // On refusal send an explicit nack first: bare `?` would just drop the stream
    // and the joiner would only see "connection lost" instead of "donor refused".
    match begin_adopt_atomic(
        &security.db,
        BaselineRole::Donor,
        remote_node_id,
        &epoch,
        BaselinePhase::Elected,
    ) {
        Ok(BeginOutcome::Started | BeginOutcome::Resume(_)) => {}
        Err(err) => {
            let nack = BaselineAck {
                accepted: false,
                donor: donor.clone(),
                joiner: joiner.clone(),
                epoch: 0,
            };
            let _ = write_frame(stream, &nack, "ack").await;
            return Err(err);
        }
    }

    let cipher = security.settings_cipher_ref();
    let snapshot = capture_baseline_snapshot(&security.db, epoch.clone(), cipher)?;
    let raw = serialize_snapshot(&snapshot)?;
    let header = build_baseline_header(&snapshot, &raw);

    let ack = BaselineAck {
        accepted: true,
        donor: donor.clone(),
        joiner: joiner.clone(),
        epoch: epoch.counter,
    };
    write_frame(stream, &ack, "ack").await?;
    write_frame(stream, &header, "header").await?;

    let chunks = chunk_snapshot(&raw);
    for chunk in &chunks {
        let seq = chunk.seq;
        write_frame(stream, chunk, "chunk").await?;
        let chunk_ack: BaselineChunkAck = read_frame(stream, "chunk_ack").await?;
        if chunk_ack.seq != seq || !chunk_ack.ok {
            return Err(transport_err(
                "chunk_ack",
                format!(
                    "joiner rejected chunk seq {seq} (ack seq={}, ok={})",
                    chunk_ack.seq, chunk_ack.ok
                ),
            ));
        }
    }

    // Donor zakonczyl wysylke — utrwal `Completed`. Donor NIE importuje (jego baza
    // jest zrodlem prawdy). Idempotentne przy ponownej wysylce.
    store_adopt_state(
        &security.db,
        &BaselineAdoptState {
            role: BaselineRole::Donor,
            peer: remote_node_id.to_string(),
            epoch: epoch.clone(),
            phase: BaselinePhase::Completed,
        },
    )?;
    let _ = stream.finish().await;
    info!(
        joiner = %remote_node_id,
        chunks = chunks.len(),
        "baseline transport: donor session completed"
    );
    Ok(())
}

// =============================================================================
// Implementacja strumienia iroh
// =============================================================================

/// Bidirectional stream iroh opakowany jako `FrameStream`. Joiner dostaje go z
/// `connection.open_bi()`, donor z `connection.accept_bi()`.
pub struct IrohFrameStream {
    send: iroh::endpoint::SendStream,
    recv: iroh::endpoint::RecvStream,
}

impl IrohFrameStream {
    pub fn new(send: iroh::endpoint::SendStream, recv: iroh::endpoint::RecvStream) -> Self {
        Self { send, recv }
    }
}

#[async_trait]
impl FrameStream for IrohFrameStream {
    async fn read_raw(&mut self, label: &str) -> LedgerResult<Vec<u8>> {
        let read = async {
            let mut len_buf = [0u8; 4];
            self.recv
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
            self.recv
                .read_exact(&mut body)
                .await
                .map_err(|e| transport_err(label, format!("read body: {e}")))?;
            Ok(body)
        };
        match tokio::time::timeout(FRAME_IO_TIMEOUT, read).await {
            Ok(res) => res,
            Err(_) => Err(transport_err(label, "read timed out")),
        }
    }

    async fn write_raw(&mut self, body: &[u8], label: &str) -> LedgerResult<()> {
        let write = async {
            self.send
                .write_all(&(body.len() as u32).to_be_bytes())
                .await
                .map_err(|e| transport_err(label, format!("write len: {e}")))?;
            self.send
                .write_all(body)
                .await
                .map_err(|e| transport_err(label, format!("write body: {e}")))?;
            Ok(())
        };
        match tokio::time::timeout(FRAME_IO_TIMEOUT, write).await {
            Ok(res) => res,
            Err(_) => Err(transport_err(label, "write timed out")),
        }
    }

    async fn finish(&mut self) -> LedgerResult<()> {
        self.send
            .finish()
            .map_err(|e| transport_err("finish", format!("{e}")))?;
        // Czekamy na odebranie koncowki przez peera (jak pairing).
        let _ = tokio::time::timeout(Duration::from_secs(5), self.send.stopped()).await;
        Ok(())
    }
}

// =============================================================================
// Crash-recovery: wznowienie przy starcie
// =============================================================================

/// Czy przy starcie istnieje trwaly stan adopcji, ktory joiner powinien wznowic
/// otwierajac strumien do dawcy. `Elected`/`Receiving` -> tak (transfer nie
/// dobiegl konca); `Importing` w praktyce nie wystepuje jako trwaly stan joinera
/// (import jest atomowy: faza Importing zyje tylko w pamieci miedzy
/// begin_adopt_atomic a commitem, ktory od razu zapisuje Imported); `Imported`/
/// `Completed` -> nie (post-commit jest wznawiany przez `run_baseline_adopt`/
/// `import_baseline` przez `BeginOutcome::Resume`, bez sieci). Donor nigdy nie
/// wznawia z wlasnej inicjatywy — czeka na ponowny Elect od joinera.
pub fn joiner_should_resume(state: &BaselineAdoptState) -> bool {
    state.role == BaselineRole::Joiner
        && matches!(
            state.phase,
            BaselinePhase::Elected | BaselinePhase::Receiving
        )
}

/// Zwraca peer (donor) + epoch_seen do wznowienia joinera, gdy trwaly stan tego
/// wymaga. `None` gdy nic do wznowienia (brak stanu, donor, albo juz po imporcie).
/// Wywolujacy (mesh startup) otwiera strumien do peera i wola `run_joiner_session`.
pub fn pending_joiner_resume(db: &DbPool) -> LedgerResult<Option<(String, u64)>> {
    let Some(state) = load_adopt_state(db)? else {
        return Ok(None);
    };
    if !joiner_should_resume(&state) {
        return Ok(None);
    }
    warn!(
        donor = %state.peer,
        phase = ?state.phase,
        "baseline transport: durable joiner state found at startup — resuming baseline pull"
    );
    Ok(Some((state.peer, state.epoch.counter)))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod e2e_tests;
