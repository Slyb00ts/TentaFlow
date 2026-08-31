// =============================================================================
// File: services/camera_ingest/stream_publisher.rs — fMP4 publisher per camera
// =============================================================================
//
// Bridges the RTSP session pipeline (Branch B: rtph264depay → h264parse →
// mp4mux → appsink) with the generic `stream_hub::BinaryStreamSource` contract
// so any consumer (WS handler, addon) can subscribe to a live fragmented MP4
// feed. The publisher is built lazily by the `StreamHub` factory the first
// time a consumer subscribes to `camera:<id>`. It then asks the camera
// session to attach an on-demand mux branch to the running pipeline; the
// branch produces fMP4 fragments (one `ftyp+moov` init segment followed by
// rolling `moof+mdat` media chunks).
//
// Init segment delivery uses a `Notify` gate: subscribers waiting on
// `init_segment()` block until the appsink callback observes the first
// buffer (which mp4mux emits as the ftyp+moov "init" segment per its
// `streamable=true` contract). A 3 s timeout protects against dormant
// publishers (e.g. H.265 camera where attach refuses and the branch never
// produces bytes) — `init_segment()` returns `None` so the hub surfaces a
// clean `FactoryFailed` instead of hanging the WS consumer indefinitely.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc, Notify};

use super::session::SessionCommand;
use crate::services::stream_hub::{BinaryStreamSource, BROADCAST_CAPACITY};

/// MIME type advertised to the browser MediaSource layer. AVC level/profile
/// are intentionally not pinned here — the browser tolerates a generic
/// `video/mp4` MIME for fMP4 as long as the init segment carries the avcC
/// box. Browser MSE will refuse if the actual codec inside the init segment
/// is not H.264 (which is enforced upstream by `attach_mp4_branch_supported`).
const FMP4_H264_MIME: &str = "video/mp4; codecs=\"avc1.42E01E\"";

/// Maximum time `init_segment()` waits for the appsink to observe the first
/// fMP4 chunk after `AttachMp4Branch`. Beyond this we assume the publisher
/// will never produce (e.g. H.265 camera, branch refused) and return `None`.
/// Must comfortably exceed the camera's IDR interval: a mid-stream Branch-B
/// attach can land just after a keyframe, and h264parse cannot emit the avcC
/// init segment until the NEXT SPS/PPS+IDR arrives. The robot's IDR interval is
/// ~2 s, so 10 s leaves room for the attach + state-sync plus several keyframe
/// chances before we give up.
const INIT_SEGMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Publishes fragmented MP4 chunks produced by a camera session's Branch B
/// mux to any number of WS subscribers.
///
/// Lifecycle:
///   1. `StreamHub` factory constructs the publisher and posts
///      `SessionCommand::AttachMp4Branch(Arc<Self>)` to the session task,
///      then drops its strong reference so only the hub keeps a strong Arc.
///   2. The session attaches the on-demand mux branch and installs an
///      appsink callback that calls `push_chunk` through a `Weak<Self>`.
///   3. The first chunk seeds the init segment and unblocks
///      `init_segment()`. Subsequent chunks fan out via `broadcast`.
///   4. When the hub's strong reference drops (last subscriber unsubscribed),
///      `Drop` posts `SessionCommand::DetachMp4Branch` to the session so the
///      mux branch is torn down and pipeline CPU returns to baseline.
pub struct Mp4StreamPublisher {
    stream_id: String,
    init_segment: Mutex<Option<Bytes>>,
    init_ready: Notify,
    /// Budzone, gdy gałąź B tego publishera REALNIE wpięła się do pipeline'u.
    /// Wpięcie może być odroczone (tee negocjuje caps dopiero po pierwszym
    /// buforze), a budżet na init segment ma mierzyć „mux wpięty i milczy", nie
    /// „kamera jeszcze wstaje" — dlatego zegar w `init_segment()` startuje od
    /// nowa na ten sygnał.
    branch_attached: Notify,
    /// `None` once the publisher has terminally failed — `chunk_broadcaster`
    /// reports that to the hub so the subscribe collapses to a clean failure
    /// (no init segment, no hung empty stream) instead of being cached as an
    /// active source. Mirrors `RemoteCameraStreamSource`.
    chunks_tx: Mutex<Option<broadcast::Sender<Bytes>>>,
    /// Set true on any terminal outcome (attach refused / init timeout); gates
    /// duplicate teardown and makes `chunk_broadcaster()` return `None`.
    terminal: AtomicBool,
    cmd_tx: mpsc::Sender<SessionCommand>,
    // mp4mux emits raw fMP4 boxes split across arbitrary `GstBuffer`s. We
    // accumulate them here, parse 8-byte box headers, and forward to MSE
    // subscribers as complete segments. See `push_chunk` for the rules.
    parser_buf: Mutex<Vec<u8>>,
    pending_init: Mutex<Vec<u8>>,
    media_buf: Mutex<Vec<u8>>,
    first_chunk_seen: AtomicBool,
    /// PTS (media-timeline, ns) PIERWSZEGO buforu wchodzacego do mp4mux w Branch
    /// B. Bo Branch A i B dziela ten sam `tee` przed dekodem/muxem, jest to ta
    /// sama oś czasu co PTS detekcji — klient dodaje ja do init-segmentu MSE, by
    /// odjac offset osi mediów i zakotwiczyc overlay na wlasciwej klatce.
    base_pts_ns: Mutex<Option<u64>>,
    /// `true` = publisher wariantu PODGLĄDU (transkod 720p/~1,5 Mbit/s pod klucz
    /// hubu `camera:<id>#preview`), `false` = pełna jakość (passthrough). Sesja
    /// wybiera po tym budowniczego gałęzi B, a `Drop` kieruje detach do
    /// właściwego slotu — full i preview to dwie niezależne gałęzie na tym
    /// samym tee.
    preview: bool,
}

impl std::fmt::Debug for Mp4StreamPublisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mp4StreamPublisher")
            .field("stream_id", &self.stream_id)
            .field(
                "init_segment_len",
                &self.init_segment.lock().as_ref().map(|b| b.len()),
            )
            .field(
                "subscribers",
                &self.chunks_tx.lock().as_ref().map(|tx| tx.receiver_count()),
            )
            .field("terminal", &self.terminal.load(Ordering::Acquire))
            .finish()
    }
}

impl Mp4StreamPublisher {
    /// Construct a fresh publisher. The hub-facing `Arc` is created by the
    /// caller (`Arc::new(Mp4StreamPublisher::new(...))`) so the strong ref
    /// count is well-defined from the start.
    pub fn new(camera_id: String, cmd_tx: mpsc::Sender<SessionCommand>, preview: bool) -> Self {
        let (chunks_tx, _rx) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            parser_buf: Mutex::new(Vec::with_capacity(64 * 1024)),
            pending_init: Mutex::new(Vec::with_capacity(2048)),
            media_buf: Mutex::new(Vec::with_capacity(64 * 1024)),
            stream_id: if preview {
                format!("camera:{}#preview", camera_id)
            } else {
                format!("camera:{}", camera_id)
            },
            init_segment: Mutex::new(None),
            init_ready: Notify::new(),
            branch_attached: Notify::new(),
            chunks_tx: Mutex::new(Some(chunks_tx)),
            first_chunk_seen: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            base_pts_ns: Mutex::new(None),
            preview,
            cmd_tx,
        }
    }

    /// Czy to publisher wariantu podglądu (720p). Sesja wybiera po tym slot
    /// gałęzi B i budowniczego pipeline'u.
    pub fn is_preview(&self) -> bool {
        self.preview
    }

    /// Zapisuje bazowy PTS (media-timeline, ns) osi mediów Branch B. Wolane raz,
    /// z pad-probe na pierwszym buforze mux/h264parse. Kolejne wywolania sa
    /// ignorowane — baza jest ustalana tylko przez pierwszy bufor.
    pub fn set_base_pts_ns(&self, pts_ns: u64) {
        let mut guard = self.base_pts_ns.lock();
        if guard.is_none() {
            *guard = Some(pts_ns);
        }
    }

    /// Kasuje bazowy PTS. Wolane przy odpieciu/rebuildzie Branch B: po reconnect
    /// oś PTS mediów resetuje sie wraz z nowym init-segmentem, wiec stara baza
    /// jest juz nieaktualna. Bez tego overlay rozjezdza sie po reconnectcie, bo
    /// `set_base_pts_ns` ustawia baze tylko gdy jest pusta. Po skasowaniu pierwszy
    /// bufor NOWEJ Branch B ustali baze spojna z nowym init-segmentem.
    pub fn reset_base_pts_ns(&self) {
        *self.base_pts_ns.lock() = None;
    }

    /// Push bytes from the `mp4mux` appsink. The muxer flushes raw fMP4
    /// boxes in arbitrary chunks — a single `GstBuffer` may contain a partial
    /// box, a full box, or several boxes concatenated. Browser MSE needs the
    /// init segment (`ftyp` + `moov`) delivered as ONE blob, then each media
    /// segment (`moof` + `mdat`) delivered as its own blob — splitting in the
    /// wrong place crashes the `SourceBuffer` (`InvalidStateError` and
    /// `SourceBuffer has been removed`).
    ///
    /// We accumulate bytes in `parser_buf`, parse 8-byte box headers, and:
    ///   - while `init_segment` is None: keep appending boxes until we see the
    ///     first `moof` — everything before it is the init segment;
    ///   - once init is sealed: each `moof+mdat` pair becomes one broadcast
    ///     chunk (browser appends them as one media segment).
    pub fn push_chunk(&self, bytes: Vec<u8>) {
        if !self.first_chunk_seen.swap(true, Ordering::AcqRel) {
            tracing::info!(
                len = bytes.len(),
                "fMP4 publisher: first mp4mux chunk received from Branch B"
            );
        }
        let mut buf = self.parser_buf.lock();
        buf.extend_from_slice(&bytes);

        // Drain as many complete top-level boxes as possible.
        loop {
            if buf.len() < 8 {
                break;
            }
            let size = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            let kind = [buf[4], buf[5], buf[6], buf[7]];
            // `size == 0` would mean "to end of file" — mp4mux never emits
            // that for streamable output. `size == 1` would mean 64-bit
            // largesize follows; again not produced by mp4mux for our caps.
            // Treat either as a parser desync and refuse to advance.
            if size < 8 || buf.len() < size {
                break;
            }
            // Detach the box bytes from the front of the buffer.
            let box_bytes: Vec<u8> = buf.drain(..size).collect();

            let mut init_guard = self.init_segment.lock();
            if init_guard.is_none() {
                // Building the init segment: ftyp / moov / styp / sidx /
                // anything BEFORE the first `moof` belongs here.
                if &kind == b"moof" {
                    // Init phase complete — seal what we have and start the
                    // first media segment with this `moof`.
                    let pending = std::mem::take(&mut *self.pending_init.lock());
                    if !pending.is_empty() {
                        tracing::info!(
                            init_len = pending.len(),
                            "fMP4 publisher: init segment (ftyp+moov) sealed"
                        );
                        *init_guard = Some(Bytes::from(pending));
                        drop(init_guard);
                        self.init_ready.notify_waiters();
                    } else {
                        // mp4mux emitted `moof` before any init box — shouldn't
                        // happen with default dash-or-mss fragment mode. Treat
                        // the moof as start of media regardless; subscribers
                        // attaching later will time out waiting for init.
                        drop(init_guard);
                    }
                    // Stash this moof; we need its mdat counterpart before
                    // emitting a complete media segment.
                    self.media_buf.lock().extend_from_slice(&box_bytes);
                } else {
                    self.pending_init.lock().extend_from_slice(&box_bytes);
                }
                continue;
            }
            drop(init_guard);

            // Media phase — pair every `moof` with the following `mdat` and
            // broadcast the pair as a single MSE media segment. Stray boxes
            // (`free`, `mfra`, …) are forwarded immediately so the receiver
            // SourceBuffer never sees a half-finished append.
            match &kind {
                b"moof" => {
                    self.media_buf.lock().extend_from_slice(&box_bytes);
                }
                b"mdat" => {
                    let mut media = self.media_buf.lock();
                    media.extend_from_slice(&box_bytes);
                    let segment = std::mem::take(&mut *media);
                    drop(media);
                    if let Some(tx) = self.chunks_tx.lock().as_ref() {
                        let _ = tx.send(Bytes::from(segment));
                    }
                }
                _ => {
                    if let Some(tx) = self.chunks_tx.lock().as_ref() {
                        let _ = tx.send(Bytes::from(box_bytes));
                    }
                }
            }
        }
    }

    /// Sesja zgłasza, że gałąź B tego publishera została wpięta do pipeline'u.
    /// Resetuje budżet oczekiwania na init segment: wpięcie bywa odroczone o
    /// sekundy (zimna kamera, tee bez caps), a bez tego resetu publisher bywał
    /// uznawany za martwy CHWILĘ PO tym, jak gałąź wreszcie ruszyła — mux nie
    /// miał kiedy wypluć ftyp+moov.
    pub fn mark_branch_attached(&self) {
        self.branch_attached.notify_waiters();
    }

    /// Mark the publisher as permanently undeliverable. Called by the session
    /// when the attach refuses (non-H.264 codec, mux build failure, File/Local
    /// source with no tee) or on an init-segment timeout. Drops the broadcast
    /// sender so live receivers observe `Closed`, makes `chunk_broadcaster()`
    /// return `None` so the hub rejects/cache-busts the failed source, and wakes
    /// init waiters so `init_segment()` returns `None` immediately instead of
    /// blocking for the full timeout. Idempotent.
    pub fn mark_unsupported(&self) {
        self.terminal.store(true, Ordering::Release);
        // Dropping the only Sender closes every outstanding Receiver and makes
        // `chunk_broadcaster()` report the source as terminal to the hub.
        *self.chunks_tx.lock() = None;
        self.init_ready.notify_waiters();
    }
}

#[async_trait]
impl BinaryStreamSource for Mp4StreamPublisher {
    fn id(&self) -> &str {
        &self.stream_id
    }

    fn mime_type(&self) -> &str {
        FMP4_H264_MIME
    }

    async fn init_segment(&self) -> Option<Bytes> {
        // Fast path: appsink has already produced the first fragment.
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        // Slow path: subscribe BEFORE re-checking so we cannot race a notify
        // that fires between the lock release and `notified()`.
        let notified = self.init_ready.notified();
        if let Some(b) = self.init_segment.lock().clone() {
            return Some(b);
        }
        // A publisher that already failed before we started waiting: bail now.
        if self.terminal.load(Ordering::Acquire) {
            return None;
        }
        // Zegar liczy od WPIĘCIA gałęzi, nie od subskrypcji. Wpięcie jest
        // odraczane, dopóki tee nie wynegocjuje caps (zimna kamera, świeżo
        // przebudowany pipeline), a stały budżet od subskrypcji wypalał się na
        // tym czekaniu: w logu produkcyjnym publisher był uznawany za martwy
        // 0,25 s PO `branch B attached`. Każde wpięcie restartuje budżet, więc
        // 10 s mierzy to, co miało mierzyć — wpięty mux, który nic nie produkuje.
        let mut notified = notified;
        let mut deadline = tokio::time::Instant::now() + INIT_SEGMENT_TIMEOUT;
        loop {
            let attached = self.branch_attached.notified();
            tokio::select! {
                _ = notified => return self.init_segment.lock().clone(),
                _ = attached => {
                    deadline = tokio::time::Instant::now() + INIT_SEGMENT_TIMEOUT;
                    // `notified` został skonsumowany przez `select!`, więc trzeba
                    // uzbroić nowe oczekiwanie PRZED ponownym sprawdzeniem stanu.
                    notified = self.init_ready.notified();
                    if let Some(b) = self.init_segment.lock().clone() {
                        return Some(b);
                    }
                    if self.terminal.load(Ordering::Acquire) {
                        return None;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    tracing::warn!(
                        timeout_s = INIT_SEGMENT_TIMEOUT.as_secs(),
                        "fMP4 publisher: init segment timed out, marking unsupported"
                    );
                    // Init never arrived in time: make the publisher terminal so
                    // `chunk_broadcaster()` returns None and subscribe fails cleanly
                    // (live receivers get Closed) instead of caching a hung empty
                    // stream that never delivers an init segment.
                    self.mark_unsupported();
                    return None;
                }
            }
        }
    }

    fn chunk_broadcaster(&self) -> Option<broadcast::Sender<Bytes>> {
        self.chunks_tx.lock().clone()
    }

    /// Bazowy PTS osi mediów Branch B (ns) albo `None`, gdy pierwszy bufor jeszcze
    /// nie przeszedl przez pad-probe. Klient MSE dolacza to do init-segmentu.
    fn base_pts_ns(&self) -> Option<u64> {
        *self.base_pts_ns.lock()
    }
}

impl Drop for Mp4StreamPublisher {
    fn drop(&mut self) {
        // Best-effort detach. `try_send` so the drop never blocks; if the
        // session's command queue is saturated the branch will be cleaned
        // up when the session itself shuts down (which always tears the
        // whole pipeline down anyway). The session is the only entity that
        // can untangle the mux elements from the running pipeline, so this
        // is the canonical teardown path.
        tracing::debug!(
            stream_id = %self.stream_id,
            "fMP4 publisher dropped, posting DetachMp4Branch"
        );
        let _ = self.cmd_tx.try_send(SessionCommand::DetachMp4Branch {
            preview: self.preview,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_publisher() -> (Arc<Mp4StreamPublisher>, mpsc::Receiver<SessionCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(8);
        let pub_ = Arc::new(Mp4StreamPublisher::new("cam_test".into(), cmd_tx, false));
        (pub_, cmd_rx)
    }

    /// Frames a top-level MP4 box as `push_chunk` expects: 4-byte big-endian
    /// size (header + payload) followed by the 4-byte kind and the payload.
    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut out = size.to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    /// Pushes an init box (`ftyp`) then a `moof`+`mdat` pair so the publisher
    /// seals the init segment AND flushes the sealing media segment, leaving the
    /// media buffer empty for the test's own segments. Returns the exact init
    /// bytes the publisher should now expose.
    fn seed_init(pub_: &Mp4StreamPublisher) -> Vec<u8> {
        let init = mp4_box(b"ftyp", &[0xDE, 0xAD, 0xBE, 0xEF]);
        pub_.push_chunk(init.clone());
        pub_.push_chunk(mp4_box(b"moof", &[0x00]));
        pub_.push_chunk(mp4_box(b"mdat", &[0x00]));
        init
    }

    #[tokio::test]
    async fn init_segment_cached_on_first_chunk() {
        let (pub_, _cmd_rx) = make_publisher();
        let expected = seed_init(&pub_);
        let init = pub_.init_segment().await.expect("init present");
        assert_eq!(&init[..], &expected[..]);
        // Re-calling returns the same cached buffer (no re-allocation).
        let init2 = pub_.init_segment().await.expect("init still present");
        assert_eq!(&init2[..], &expected[..]);
    }

    #[tokio::test]
    async fn init_segment_notify_unblocks_waiters() {
        let (pub_, _cmd_rx) = make_publisher();
        let pub_for_push = Arc::clone(&pub_);
        let expected = mp4_box(b"ftyp", &[1, 2, 3]);
        let expected_for_push = expected.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pub_for_push.push_chunk(expected_for_push);
            pub_for_push.push_chunk(mp4_box(b"moof", &[0x00]));
        });
        let init = pub_.init_segment().await.expect("init via notify");
        assert_eq!(&init[..], &expected[..]);
    }

    #[tokio::test(start_paused = true)]
    async fn init_segment_timeout_returns_none_for_dormant() {
        let (pub_, _cmd_rx) = make_publisher();
        // Paused clock: tokio auto-advances when the only pending work is the
        // timeout sleep, so the full window elapses instantly in test time.
        let start = tokio::time::Instant::now();
        let result = pub_.init_segment().await;
        let elapsed = start.elapsed();
        assert!(result.is_none(), "dormant publisher must yield None");
        assert!(
            elapsed >= INIT_SEGMENT_TIMEOUT,
            "must wait at least the timeout window, got {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn subsequent_chunks_broadcast_to_subscribers() {
        let (pub_, _cmd_rx) = make_publisher();
        // Seed the init segment first — only media segments pushed AFTER the init
        // segment travel through the broadcast channel.
        seed_init(&pub_);
        let _ = pub_.init_segment().await.expect("init seeded");
        let mut rx = pub_.chunk_broadcaster().expect("broadcaster").subscribe();
        // A media segment is the `moof`+`mdat` pair, broadcast concatenated.
        let moof1 = mp4_box(b"moof", &[1]);
        let mdat1 = mp4_box(b"mdat", &[2]);
        pub_.push_chunk(moof1.clone());
        pub_.push_chunk(mdat1.clone());
        let moof2 = mp4_box(b"moof", &[3]);
        let mdat2 = mp4_box(b"mdat", &[4]);
        pub_.push_chunk(moof2.clone());
        pub_.push_chunk(mdat2.clone());
        let first = rx.recv().await.expect("first chunk");
        let second = rx.recv().await.expect("second chunk");
        let mut expected1 = moof1.clone();
        expected1.extend_from_slice(&mdat1);
        let mut expected2 = moof2.clone();
        expected2.extend_from_slice(&mdat2);
        assert_eq!(&first[..], &expected1[..]);
        assert_eq!(&second[..], &expected2[..]);
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_same_chunks() {
        let (pub_, _cmd_rx) = make_publisher();
        seed_init(&pub_);
        let _ = pub_.init_segment().await.expect("init seeded");
        let mut rx1 = pub_.chunk_broadcaster().expect("broadcaster").subscribe();
        let mut rx2 = pub_.chunk_broadcaster().expect("broadcaster").subscribe();
        let moof = mp4_box(b"moof", &[9]);
        let mdat = mp4_box(b"mdat", &[9]);
        pub_.push_chunk(moof.clone());
        pub_.push_chunk(mdat.clone());
        let mut expected = moof.clone();
        expected.extend_from_slice(&mdat);
        let a = rx1.recv().await.expect("rx1");
        let b = rx2.recv().await.expect("rx2");
        assert_eq!(&a[..], &expected[..]);
        assert_eq!(&b[..], &expected[..]);
    }

    #[tokio::test]
    async fn drop_posts_detach_command() {
        let (pub_, mut cmd_rx) = make_publisher();
        // While the strong Arc lives the channel must be empty.
        assert!(cmd_rx.try_recv().is_err());
        drop(pub_);
        let cmd = cmd_rx.recv().await.expect("detach command on drop");
        assert!(matches!(
            cmd,
            SessionCommand::DetachMp4Branch { preview: false }
        ));
    }

    #[tokio::test]
    async fn mark_unsupported_unblocks_waiters_with_none() {
        let (pub_, _cmd_rx) = make_publisher();
        let pub_for_mark = Arc::clone(&pub_);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            pub_for_mark.mark_unsupported();
        });
        let init = pub_.init_segment().await;
        assert!(init.is_none(), "unsupported publisher must yield None");
    }

    #[tokio::test]
    async fn mark_unsupported_makes_broadcaster_terminal() {
        let (pub_, _cmd_rx) = make_publisher();
        // A live receiver before the terminal transition observes Closed.
        let mut rx = pub_
            .chunk_broadcaster()
            .expect("broadcaster live")
            .subscribe();
        pub_.mark_unsupported();
        assert!(
            pub_.chunk_broadcaster().is_none(),
            "terminal publisher must report no broadcaster so the hub cache-busts it"
        );
        assert!(
            rx.recv().await.is_err(),
            "dropping the sender closes outstanding receivers"
        );
    }

    /// Regresja z produkcji: gałąź B bywa wpinana z opóźnieniem (tee negocjuje
    /// caps dopiero po pierwszym buforze), a budżet liczony od SUBSKRYPCJI
    /// wypalał się na tym czekaniu — w logu publisher był uznawany za martwy
    /// 0,25 s PO `branch B attached`, więc mux nie miał kiedy wypluć ftyp+moov.
    /// Wpięcie musi restartować zegar.
    #[tokio::test(start_paused = true)]
    async fn branch_attach_restarts_the_init_budget() {
        let (pub_, _cmd_rx) = make_publisher();
        let waiter = {
            let pub_ = Arc::clone(&pub_);
            tokio::spawn(async move { pub_.init_segment().await })
        };

        // Prawie cały pierwotny budżet mija bez wpięcia gałęzi.
        tokio::time::sleep(INIT_SEGMENT_TIMEOUT - Duration::from_millis(250)).await;
        assert!(
            !waiter.is_finished(),
            "budżet nie powinien jeszcze wygasnąć"
        );

        // Gałąź wreszcie się wpina — od tego momentu liczymy 10 s od nowa.
        pub_.mark_branch_attached();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(
            !waiter.is_finished(),
            "po wpięciu gałęzi publisher nie może umrzeć na starym deadline"
        );

        // Init segment przychodzi normalnie po wpięciu. Pieczętuje go dopiero
        // pierwszy `moof` (patrz `push_chunk`), stąd pełna trójka jak w `seed_init`.
        pub_.push_chunk(mp4_box(b"ftyp", &[0xDE, 0xAD, 0xBE, 0xEF]));
        pub_.push_chunk(mp4_box(b"moof", &[0x00]));
        pub_.push_chunk(mp4_box(b"mdat", &[0x00]));
        let got = waiter.await.expect("waiter task");
        assert!(got.is_some(), "init segment po wpięciu musi dotrzeć");
        assert!(
            pub_.chunk_broadcaster().is_some(),
            "publisher, który dostarczył init, nie może być terminalny"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn init_timeout_makes_broadcaster_terminal() {
        let (pub_, _cmd_rx) = make_publisher();
        // No chunk ever pushed: init_segment waits out the timeout, then must
        // mark the publisher terminal so the hub does not cache it active.
        let result = pub_.init_segment().await;
        assert!(result.is_none(), "dormant publisher must yield None");
        assert!(
            pub_.chunk_broadcaster().is_none(),
            "init timeout must make the broadcaster terminal"
        );
    }
}
