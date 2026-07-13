// =============================================================================
// File: services/event_recorder.rs — per-vehicle event recording
// =============================================================================
//
// Operator requirement: "Potrzebujemy nagrania zawsze jak pojawia się nowy
// samochód i aż odjedzie" — whenever a vehicle appears at a camera, record
// video until the scene empties.
//
// One background task per LOCAL camera, spawned lazily from the
// `detection_bus::publish_detections` hook the first time a camera publishes
// an analysis frame (empty or not). The task drives a small state machine:
//
//   EMPTY      --any detection-->                 RECORDING (start a file)
//   RECORDING  --no detections for hysteresis-->  EMPTY     (finalize + DB row)
//
// Video comes from the SAME fMP4 passthrough source the Live view uses:
// `StreamHub::subscribe("camera:<id>")` attaches Branch B (rtph264depay →
// h264parse → mp4mux → appsink; NO transcode) and yields an init segment plus
// self-contained moof+mdat media segments. Recording is therefore a plain
// append of those bytes into `~/.tentaflow/recordings/<camera_id>/segments/`,
// and the file is served/played through the existing `GET /recordings/<ref>`
// signed-URL path with a normal `recordings` catalog row (kind = "segment").
//
// Pre-roll: while EMPTY the task keeps the hub subscription alive and holds
// the last `event_preroll_secs` of media segments in a ring buffer, so the
// vehicle is on tape from BEFORE its first detection. Fragments are 200 ms of
// passthrough H.264 — the buffer costs ~`preroll × bitrate` RAM per camera and
// zero transcode CPU. `event_preroll_secs = 0` subscribes only for the
// duration of a recording (Branch B detaches between events).
//
// Camera scope: the recorder handles every camera whose VIDEO is reachable on
// this node — core-owned sessions and Stage-B vision-worker cameras alike
// (worker detections are republished on the core detection bus and worker
// video is relayed through the same `camera:<id>` hub factories). Cameras that
// live on ANOTHER mesh node are excluded by the node-local `cameras` table
// gate; their owning node runs its own recorder.
//
// All knobs live in the `[vision]` config section (`event_recording`,
// `event_stop_hysteresis_secs`, `event_preroll_secs`,
// `event_max_duration_secs`) — deliberately NO environment variables.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, warn};

use crate::services::detection_bus::{self, Detection, DetectionsMessage};
use crate::services::recording::{
    camera_subdir, recording_base_dir, validate_camera_id, RecordingKind,
};
use crate::services::stream_hub::{StreamHub, SubscriptionHandle};

/// State-machine tick cadence. Bounds how late a hysteresis stop can fire.
const TICK_INTERVAL: Duration = Duration::from_millis(500);

/// A recorder whose camera published NOTHING on the detection bus for this
/// long (analysis stopped, camera removed) exits and frees its resources; the
/// publish hook respawns it if the camera ever publishes again.
const IDLE_EXIT: Duration = Duration::from_secs(15 * 60);

/// Back-off between failed `camera:<id>` hub subscribe attempts, and the
/// cooldown before re-trying to START a recording after a start failed because
/// video was unavailable (prevents hammering attach on every detection frame).
const VIDEO_RETRY: Duration = Duration::from_secs(30);

/// Hard RAM ceiling for the pre-roll ring buffer, guarding against a camera
/// with an absurd bitrate. Oldest fragments are dropped first.
const PREROLL_MAX_BYTES: usize = 96 * 1024 * 1024;

// -----------------------------------------------------------------------------
// Publish hook + per-camera task registry
// -----------------------------------------------------------------------------

/// Cameras that already have a recorder task (or had one spawned this tick).
/// The task removes itself on exit so a later publish can respawn it.
fn handled() -> &'static Mutex<HashSet<String>> {
    static SET: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Hook called by `detection_bus::publish_detections` for EVERY analysis
/// frame (empty ones included, so the pre-roll buffer is warm before the
/// first vehicle). Must stay cheap — it runs per frame per camera: one config
/// read plus one hash-set probe on the hot path.
pub fn on_detections_published(camera_id: &str) {
    if !crate::vision::settings::get().event_recording {
        return;
    }
    // Vision WORKER processes run this same publish path for their local bus
    // but never initialize the core DB pool — recording (catalog rows, camera
    // identity) is the core's job, which sees the same detections republished
    // by the fleet link. The pool gate keeps workers (and early core boot,
    // which self-heals on the next frame) out without per-frame task churn.
    if crate::db::global_pool().is_none() {
        return;
    }
    {
        let mut set = handled().lock().unwrap_or_else(|p| p.into_inner());
        if !set.insert(camera_id.to_string()) {
            return;
        }
    }
    // Publishers may run on non-tokio threads (GStreamer callbacks); only a
    // live runtime can host the recorder. Without one, un-mark the camera so
    // a later publish from a runtime thread retries the spawn.
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        handled()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(camera_id);
        return;
    };
    let camera_id = camera_id.to_string();
    rt.spawn(async move {
        let respawnable = run_recorder(&camera_id).await;
        // A camera that is not node-local can never become recordable under
        // this id — keep it marked so remote/mesh detections stop probing the
        // DB per frame. Idle exits DO unmark so a later frame respawns.
        if respawnable {
            handled()
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&camera_id);
        }
        debug!(camera_id = %camera_id, respawnable, "[event_recorder] task exited");
    });
}

// -----------------------------------------------------------------------------
// Pure state machine (unit-tested)
// -----------------------------------------------------------------------------

/// EMPTY ⇄ RECORDING transitions driven by detection presence and time.
/// Deliberately free of I/O so the hysteresis semantics are testable.
#[derive(Debug)]
struct EventStateMachine {
    hysteresis: Duration,
    /// `Some(last presence)` while RECORDING, `None` while EMPTY.
    last_presence: Option<Instant>,
}

impl EventStateMachine {
    fn new(hysteresis: Duration) -> Self {
        Self {
            hysteresis,
            last_presence: None,
        }
    }

    fn recording(&self) -> bool {
        self.last_presence.is_some()
    }

    /// A non-empty detection frame arrived. Returns `true` when this is the
    /// EMPTY → RECORDING edge (caller starts a file); re-triggers inside the
    /// hysteresis window just refresh the presence timestamp.
    fn on_presence(&mut self, now: Instant) -> bool {
        let start = self.last_presence.is_none();
        self.last_presence = Some(now);
        start
    }

    /// Time-driven check: the scene has stayed empty past the hysteresis.
    fn should_stop(&self, now: Instant) -> bool {
        match self.last_presence {
            Some(last) => now.duration_since(last) >= self.hysteresis,
            None => false,
        }
    }

    /// RECORDING → EMPTY (caller finalized the file).
    fn on_stopped(&mut self) {
        self.last_presence = None;
    }
}

// -----------------------------------------------------------------------------
// Event metadata accumulator
// -----------------------------------------------------------------------------

/// Per-OCR-text accumulator inside one event: how many frames read this exact
/// string and the SUM of their confidences. `weight` (confidence-weighted count)
/// picks the winner; `conf_sum / count` recovers its mean confidence for the
/// gate. A text with no confidence (executor-path read) contributes a full unit.
#[derive(Debug, Default, Clone)]
struct TextVote {
    count: u64,
    weight: f64,
    conf_sum: f64,
}

/// Confidence/agreement floors for an event's reported plate/ADR.
///
/// Confidence is DISABLED (0.0): the plate-OCR model's per-char softmax is
/// near-uniform, so its mean confidence sits at ~0.05 even for a plate read
/// identically thousands of times at 99%+ agreement — gating on it marked every
/// real plate `unreadable`. The trustworthy signal is AGREEMENT (a consistent
/// read across the event), so we report the agreement-majority winner and never
/// suppress a confidently-agreed plate. A tiny agreement floor still drops pure
/// scatter (every frame a different string) to `unreadable`.
const EVENT_TEXT_MIN_CONFIDENCE: f64 = 0.0;
const EVENT_TEXT_MIN_AGREEMENT: f64 = 0.34;

/// Gated winner for one class's OCR votes: the reported string (or unreadable),
/// its mean confidence and the agreement ratio, serialized into `event_meta`.
#[derive(Debug, serde::Serialize)]
struct TextWinner {
    /// Winning string, or `null` when gated out (see `unreadable`).
    text: Option<String>,
    confidence: f64,
    agreement: f64,
    unreadable: bool,
    /// Per-variant frame counts, kept for downstream inspection/debugging.
    votes: BTreeMap<String, u64>,
}

/// Aggregates every detection observed during one event into the JSON summary
/// persisted in `recordings.event_meta`. OCR reads are voted per class with a
/// CONFIDENCE-WEIGHTED tally so a repeated low-confidence misread on an occluded
/// plate is reported `unreadable`, not as a confident plate. Sticker-condition
/// labels (`stan`) are voted per label so the event reports the MAJORITY state
/// of each sticker across all frames, not one noisy frame.
/// Per-VEHICLE aggregation: every detection routed to one truck (by its
/// `vehicle_id`) tallies its plates/ADR/stickers/thumbnails HERE. This is
/// exactly the flat bag the recorder used to keep for the whole scene; the
/// per-truck separation just keeps one of these per vehicle so two trucks in one
/// event no longer mix their plates/placards.
#[derive(Debug, Default)]
struct VehicleMeta {
    /// class → frames it appeared in.
    classes: BTreeMap<String, u64>,
    /// class → OCR text → confidence-weighted vote (plates, ADR codes, …).
    texts: BTreeMap<String, BTreeMap<String, TextVote>>,
    /// sticker label → observed state → frames seen with that state.
    stany: BTreeMap<String, BTreeMap<String, u64>>,
    /// distinct non-zero tracker ids seen.
    tracks: BTreeSet<u32>,
    /// detection frames this vehicle appeared in.
    frames: u64,
    /// class → best (highest-confidence) full-frame thumbnail seen for that
    /// class: the snapshot ref plus the confidence that backs it.
    best_thumb: BTreeMap<String, (String, f32)>,
    /// Running sum of detection box CENTER-X (0..1) and its count — the mean is
    /// the bucket's horizontal position, used to cluster track fragments into
    /// physical lanes (see [`EventMeta::consolidated`]).
    cx_sum: f64,
    cx_count: u64,
}

/// Aggregates every detection observed during one event, GROUPED BY vehicle.
/// `vehicles[id]` is one truck's plates/ADR/stickers/thumbnails; `vehicle_id = 0`
/// is the "unassigned" bucket (signs outside any vehicle box, kept for the JSON
/// but never the primary). With exactly one vehicle this collapses to today's
/// single bag, so the scalar DB columns stay byte-identical for one-truck events.
#[derive(Debug, Default)]
struct EventMeta {
    vehicles: BTreeMap<u32, VehicleMeta>,
}

/// Detection class carrying vehicle plate reads.
const CLASS_PLATE: &str = "tablica_rejestracyjna";
/// Detection class carrying ADR placard reads.
const CLASS_ADR: &str = "tablica_adr";
/// Detection class of a whole-vehicle box (YOLO vehicle detector). Mirrors
/// `vision::detector_vehicle::VEHICLE_CLASS`, kept local so this always-compiled
/// module needs no dependency on the feature-gated detector.
const VEHICLE_CLASS: &str = "vehicle";

/// Minimum horizontal gap (fraction of frame width) between two clusters of
/// track fragments for them to count as TWO separate lanes/vehicles. Below this,
/// all fragments are treated as ONE physical vehicle that moved/was re-acquired.
/// Two adjacent entry lanes sit well apart; one truck's jitter stays under it.
const LANE_SPLIT_GAP: f64 = 0.20;

/// Splits a raw `stan` string into `(label, state)`. Sticker labels are emitted
/// as `"<label> <state>"` (e.g. `"nalepka_3 czysta"`, `"znak_srodowiskowy
/// uszkodzona"`), so the LAST whitespace token is the condition and everything
/// before it is the label. A bare single-token flag (`"uszkodzona"`, `"ok"`)
/// has no separate label — it is voted under itself as both.
fn split_stan(raw: &str) -> (String, String) {
    let raw = raw.trim();
    match raw.rsplit_once(char::is_whitespace) {
        Some((label, state)) if !label.trim().is_empty() && !state.trim().is_empty() => {
            (label.trim().to_string(), state.trim().to_string())
        }
        _ => (raw.to_string(), raw.to_string()),
    }
}

impl VehicleMeta {
    /// Folds ONE detection (already routed to this vehicle) into the tally. This
    /// is the exact per-detection logic the old scene-wide `absorb` ran — the
    /// per-truck split only changed WHICH `VehicleMeta` a detection lands in.
    fn absorb_one(&mut self, d: &Detection) {
        *self.classes.entry(d.klasa.clone()).or_insert(0) += 1;
        // Horizontal position of this detection (box center-x, 0..1) feeds the
        // per-lane clustering that collapses one truck's track fragments.
        self.cx_sum += (d.bbox[0] + d.bbox[2] * 0.5) as f64;
        self.cx_count += 1;
        if let Some(t) = d.tekst.as_deref() {
            if !t.is_empty() {
                // An executor-path read has no numeric score; treat it as a
                // full-confidence unit so it still clears the confidence floor and
                // is gated on agreement (mirrors the live vote).
                let conf = d.tekst_conf.unwrap_or(1.0).clamp(0.0, 1.0) as f64;
                let vote = self
                    .texts
                    .entry(d.klasa.clone())
                    .or_default()
                    .entry(t.to_string())
                    .or_default();
                vote.count += 1;
                vote.weight += conf.max(0.01);
                vote.conf_sum += conf;
            }
        }
        // A thumbnail rides only the read that just set a new best confidence for
        // its track; keep the highest-confidence one per class across the event.
        if let Some(thumb) = d.tekst_thumb_ref.as_deref() {
            if !thumb.is_empty() {
                let conf = d.tekst_conf.unwrap_or(0.0).clamp(0.0, 1.0);
                let better = self
                    .best_thumb
                    .get(&d.klasa)
                    .map(|(_, c)| conf > *c)
                    .unwrap_or(true);
                if better {
                    self.best_thumb
                        .insert(d.klasa.clone(), (thumb.to_string(), conf));
                }
            }
        }
        // The classifier emits the STATE word ("czysta"/"brudna"/…) in `d.stan`;
        // the sticker it belongs to is the detection CLASS (`d.klasa`). Aggregate
        // the majority state PER sticker class. (`split_stan` still handles a
        // legacy "<label> <state>" string defensively.)
        let sticker = d.klasa.trim();
        if !sticker.is_empty() {
            for s in &d.stan {
                let s = s.trim();
                if s.is_empty() {
                    continue;
                }
                let (label, state) = if s.contains(' ') {
                    split_stan(s)
                } else {
                    (sticker.to_string(), s.to_string())
                };
                // The plate and ADR placard are NOT stickers — they surface as
                // their own Rejestracja/ADR columns, so their condition never
                // pollutes the "Nalepki" list.
                if label == CLASS_PLATE || label == CLASS_ADR {
                    continue;
                }
                *self
                    .stany
                    .entry(label)
                    .or_default()
                    .entry(state)
                    .or_insert(0) += 1;
            }
        }
        if d.track_id != 0 {
            self.tracks.insert(d.track_id);
        }
    }

    /// Gated plate/ADR winner strings for this vehicle, mirroring the JSON
    /// `texts.<class>` winners but as plain columns for indexed search. `None`
    /// when a class was unreadable or absent.
    fn winner_texts(&self) -> (Option<String>, Option<String>) {
        let plate = self
            .texts
            .get(CLASS_PLATE)
            .map(Self::winner)
            .and_then(|w| w.text);
        let adr = self
            .texts
            .get(CLASS_ADR)
            .map(Self::winner)
            .and_then(|w| w.text);
        (plate, adr)
    }

    /// The single representative full-frame thumbnail for this vehicle: prefer
    /// the best PLATE-read frame, then the best ADR-read frame, then the best of
    /// any remaining class.
    fn event_thumb(&self) -> Option<String> {
        if let Some((r, _)) = self.best_thumb.get(CLASS_PLATE) {
            return Some(r.clone());
        }
        if let Some((r, _)) = self.best_thumb.get(CLASS_ADR) {
            return Some(r.clone());
        }
        self.best_thumb
            .values()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(r, _)| r.clone())
    }

    /// Majority sticker state per label for this vehicle: `{"nalepka_3":
    /// "czysta", …}`. The winner is the state with the most frames; ties break on
    /// the lexically-smallest state. Labels with no votes are absent.
    fn stany_winners(&self) -> BTreeMap<String, String> {
        self.stany
            .iter()
            .filter_map(|(label, states)| {
                states
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map(|(state, _)| (label.clone(), state.clone()))
            })
            .collect()
    }

    /// Confidence+agreement gate over one class's votes: winner = heaviest
    /// variant, agreement = winner_weight / total_weight, reported only when the
    /// winner's mean confidence and agreement clear the floors — otherwise the
    /// class is `unreadable`.
    fn winner(votes: &BTreeMap<String, TextVote>) -> TextWinner {
        let counts: BTreeMap<String, u64> =
            votes.iter().map(|(t, v)| (t.clone(), v.count)).collect();
        // Winner by RAW COUNT (most-read across the event), never by confidence
        // weight: the plate-OCR softmax confidence is near-uniform (~0.05) with
        // per-frame noise, so a weighted winner handed the plate to a 21-read blur
        // over a 2018-read correct plate. Raw majority is robust; agreement =
        // winner_count / total_count.
        let total_count: u64 = votes.values().map(|v| v.count).sum();
        let best = votes.iter().max_by(|a, b| a.1.count.cmp(&b.1.count));
        let Some((text, v)) = best else {
            return TextWinner {
                text: None,
                confidence: 0.0,
                agreement: 0.0,
                unreadable: true,
                votes: counts,
            };
        };
        let agreement = if total_count > 0 {
            v.count as f64 / total_count as f64
        } else {
            0.0
        };
        let mean_conf = if v.count > 0 {
            v.conf_sum / v.count as f64
        } else {
            0.0
        };
        let ok = mean_conf >= EVENT_TEXT_MIN_CONFIDENCE && agreement >= EVENT_TEXT_MIN_AGREEMENT;
        TextWinner {
            text: ok.then(|| text.clone()),
            confidence: mean_conf,
            agreement,
            unreadable: !ok,
            votes: counts,
        }
    }

    /// Per-class gated winners (plate/ADR or unreadable) + confidence, agreement
    /// and raw per-variant counts, for the JSON `texts` map.
    fn texts_winners(&self) -> BTreeMap<String, TextWinner> {
        self.texts
            .iter()
            .map(|(klasa, votes)| (klasa.clone(), Self::winner(votes)))
            .collect()
    }

    /// Mean horizontal position (box center-x, 0..1) of this bucket's detections.
    /// `0.5` (frame center) when no positioned detection was seen.
    fn mean_cx(&self) -> f64 {
        if self.cx_count == 0 {
            0.5
        } else {
            self.cx_sum / self.cx_count as f64
        }
    }

    /// Folds another bucket's tallies into this one — consolidating track-fragment
    /// buckets that resolve to the SAME physical truck (see [`EventMeta::consolidated`]).
    fn merge_from(&mut self, other: &VehicleMeta) {
        for (k, c) in &other.classes {
            *self.classes.entry(k.clone()).or_insert(0) += c;
        }
        for (k, votes) in &other.texts {
            let dst = self.texts.entry(k.clone()).or_default();
            for (t, v) in votes {
                let e = dst.entry(t.clone()).or_default();
                e.count += v.count;
                e.weight += v.weight;
                e.conf_sum += v.conf_sum;
            }
        }
        for (label, states) in &other.stany {
            let dst = self.stany.entry(label.clone()).or_default();
            for (st, c) in states {
                *dst.entry(st.clone()).or_insert(0) += c;
            }
        }
        self.tracks.extend(other.tracks.iter().copied());
        self.frames += other.frames;
        self.cx_sum += other.cx_sum;
        self.cx_count += other.cx_count;
        for (k, (r, c)) in &other.best_thumb {
            let better = self.best_thumb.get(k).map(|(_, cc)| c > cc).unwrap_or(true);
            if better {
                self.best_thumb.insert(k.clone(), (r.clone(), *c));
            }
        }
    }
}

impl EventMeta {
    /// Routes every detection of a frame to its vehicle bucket by `vehicle_id`
    /// (0 = unassigned bucket) and folds it in. Empty frames are ignored so the
    /// event's `detection_frames`/`frames` count only frames that carried at
    /// least one detection (matching the old scene-wide semantics per vehicle).
    fn absorb(&mut self, items: &[Detection]) {
        if items.is_empty() {
            return;
        }
        // A vehicle is present in THIS frame iff at least one detection routed to
        // it; bump each present vehicle's frame counter once (not once per det).
        let mut present: BTreeSet<u32> = BTreeSet::new();
        for d in items {
            let vm = self.vehicles.entry(d.vehicle_id).or_default();
            vm.absorb_one(d);
            present.insert(d.vehicle_id);
        }
        for id in present {
            self.vehicles.entry(id).or_default().frames += 1;
        }
    }

    /// Consolidates track-fragment buckets into PHYSICAL vehicles by LANE. One
    /// truck driving in, stopping, and leaving is re-acquired by the IOU tracker
    /// under dozens of `vehicle_id`s — but it stays in ITS lane, so all its
    /// fragments share a horizontal position. The scene has at most two vehicles
    /// (two adjacent entry lanes side by side), so we cluster buckets by mean
    /// center-x into AT MOST TWO lanes: sort by position, split at the single
    /// widest horizontal gap only when that gap exceeds [`LANE_SPLIT_GAP`] (two
    /// distinct lanes), otherwise merge everything into one vehicle. This turns
    /// "50 vehicles" back into the one (or two) real trucks. Each lane is keyed by
    /// the smallest source `vehicle_id` it contains (stable, deterministic).
    fn consolidated(&self) -> BTreeMap<u32, VehicleMeta> {
        // (mean_cx, id) sorted left→right. Positionless buckets (no box seen) sit
        // at 0.5 so they fold into whichever lane wins the center.
        let mut ordered: Vec<(f64, u32)> = self
            .vehicles
            .iter()
            .map(|(id, vm)| (vm.mean_cx(), *id))
            .collect();
        ordered.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

        // Widest gap between adjacent positions = the boundary between two lanes.
        let mut split_at = 0usize; // number of buckets left of the split
        let mut widest = 0.0f64;
        for i in 1..ordered.len() {
            let gap = ordered[i].0 - ordered[i - 1].0;
            if gap > widest {
                widest = gap;
                split_at = i;
            }
        }
        let two_lanes = widest >= LANE_SPLIT_GAP;

        let mut out: BTreeMap<u32, VehicleMeta> = BTreeMap::new();
        // Representative id per lane = the smallest id in that lane.
        let lane_rep = |slice: &[(f64, u32)]| slice.iter().map(|(_, id)| *id).min().unwrap_or(0);
        let (left, right) = if two_lanes {
            ordered.split_at(split_at)
        } else {
            (&ordered[..], &[][..])
        };
        for lane in [left, right] {
            if lane.is_empty() {
                continue;
            }
            let rep = lane_rep(lane);
            let dst = out.entry(rep).or_default();
            for (_, id) in lane {
                if let Some(vm) = self.vehicles.get(id) {
                    dst.merge_from(vm);
                }
            }
        }
        out
    }

    /// The PRIMARY vehicle (post-consolidation): the physical truck with the most
    /// detection frames (ties → smallest id). Its plate/ADR/thumb feed the scalar
    /// DB columns so the panel row + search stay working with NO migration.
    /// `vehicle_id = 0` (unassigned) only wins when it is the ONLY bucket — a real
    /// (non-zero) vehicle always outranks it at equal frames. `None` for an empty
    /// event.
    fn primary(&self) -> Option<(u32, VehicleMeta)> {
        self.consolidated()
            .into_iter()
            .max_by(|(ida, a), (idb, b)| {
                a.frames
                    .cmp(&b.frames)
                    // A non-zero vehicle beats the unassigned (0) bucket at a tie.
                    .then_with(|| (*ida != 0).cmp(&(*idb != 0)))
                    // Then the smaller id (stable).
                    .then_with(|| idb.cmp(ida))
            })
    }

    /// Scalar plate/ADR winner columns = the PRIMARY vehicle's (one-truck events
    /// stay byte-identical to the old single-bag output).
    fn winner_texts(&self) -> (Option<String>, Option<String>) {
        self.primary()
            .map(|(_, vm)| vm.winner_texts())
            .unwrap_or((None, None))
    }

    /// Whether this event captured anything worth keeping: at least one
    /// consolidated vehicle with a plate/ADR/sticker read OR an actual vehicle
    /// box. Events that triggered on transient noise and never enriched anything
    /// (no vehicle, no sign) are DISCARDED instead of cataloged as empty clips.
    fn has_content(&self) -> bool {
        self.consolidated().values().any(|vm| {
            let (plate, adr) = vm.winner_texts();
            plate.is_some()
                || adr.is_some()
                || !vm.stany_winners().is_empty()
                || vm.classes.contains_key(VEHICLE_CLASS)
        })
    }

    /// Scalar event thumbnail = the PRIMARY vehicle's photo, falling back to ANY
    /// vehicle that captured one so a recording ALWAYS has a list thumbnail (the
    /// scene-throttled capture may land on a non-primary bucket).
    fn event_thumb(&self) -> Option<String> {
        self.primary()
            .and_then(|(_, vm)| vm.event_thumb())
            .or_else(|| self.consolidated().values().find_map(|vm| vm.event_thumb()))
    }

    /// Serialize the summary for one finalized file. Keeps the historical
    /// top-level `classes`/`texts`/`stany`/`tracks`/`detection_frames` — for a
    /// single vehicle those equal that vehicle, so ONE-truck JSON stays
    /// byte-identical there; multi-truck top-level fields are the UNION across
    /// vehicles. The new `vehicles[]` array carries the per-truck breakdown the
    /// panel renders. `start/stop` are wall clock (unix ms) of the FILE, `part`
    /// numbers files within one long event.
    fn to_json(&self, start_ts_ms: u64, stop_ts_ms: u64, preroll_ms: u64, part: u32) -> String {
        // Consolidate track fragments into physical trucks BEFORE serializing, so
        // the panel shows one entry per real truck (not one per re-acquisition).
        let vehicles_map = self.consolidated();
        // Top-level aggregate (union across vehicles) — back-compat shape.
        let mut classes: BTreeMap<String, u64> = BTreeMap::new();
        let mut texts_merged: BTreeMap<String, BTreeMap<String, TextVote>> = BTreeMap::new();
        let mut stany_merged: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        let mut tracks: BTreeSet<u32> = BTreeSet::new();
        let mut frames = 0u64;
        for vm in vehicles_map.values() {
            for (k, c) in &vm.classes {
                *classes.entry(k.clone()).or_insert(0) += c;
            }
            for (k, votes) in &vm.texts {
                let dst = texts_merged.entry(k.clone()).or_default();
                for (t, v) in votes {
                    let e = dst.entry(t.clone()).or_default();
                    e.count += v.count;
                    e.weight += v.weight;
                    e.conf_sum += v.conf_sum;
                }
            }
            for (label, states) in &vm.stany {
                let dst = stany_merged.entry(label.clone()).or_default();
                for (st, c) in states {
                    *dst.entry(st.clone()).or_insert(0) += c;
                }
            }
            tracks.extend(vm.tracks.iter().copied());
            frames = frames.max(vm.frames);
        }
        let texts: BTreeMap<String, TextWinner> = texts_merged
            .iter()
            .map(|(k, votes)| (k.clone(), VehicleMeta::winner(votes)))
            .collect();
        let stany_winners: BTreeMap<String, String> = stany_merged
            .iter()
            .filter_map(|(label, states)| {
                states
                    .iter()
                    .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                    .map(|(state, _)| (label.clone(), state.clone()))
            })
            .collect();

        // Per-vehicle breakdown — one object per CONSOLIDATED truck (unassigned
        // bucket 0 included for completeness; the panel prefers non-zero vehicles).
        let vehicles: Vec<serde_json::Value> = vehicles_map
            .iter()
            .map(|(id, vm)| {
                let (plate, adr) = vm.winner_texts();
                serde_json::json!({
                    "vehicle_id": id,
                    "plate": plate,
                    "adr": adr,
                    "stany": vm.stany_winners(),
                    "thumb": vm.event_thumb(),
                    "detection_frames": vm.frames,
                    "classes": vm.classes,
                    "texts": vm.texts_winners(),
                })
            })
            .collect();

        serde_json::json!({
            "event": "vehicle_presence",
            "start_ts_ms": start_ts_ms,
            "stop_ts_ms": stop_ts_ms,
            "preroll_ms": preroll_ms,
            "part": part,
            "detection_frames": frames,
            "tracks": tracks.len(),
            "classes": classes,
            "texts": texts,
            "stany": stany_winners,
            "vehicles": vehicles,
        })
        .to_string()
    }
}

// -----------------------------------------------------------------------------
// fMP4 helpers + pre-roll ring buffer
// -----------------------------------------------------------------------------

/// Four-CC of the first top-level box in a publisher chunk. Media chunks start
/// with `moof`; a mid-stream `ftyp` means the camera pipeline rebuilt and a
/// NEW init segment (new PTS axis) follows — appending across that boundary
/// would corrupt the file, so the recorder rotates on it.
fn fmp4_box_kind(chunk: &[u8]) -> Option<[u8; 4]> {
    if chunk.len() < 8 {
        return None;
    }
    Some([chunk[4], chunk[5], chunk[6], chunk[7]])
}

/// Rolling window of recent media segments kept while EMPTY.
#[derive(Debug, Default)]
struct PrerollBuffer {
    segments: VecDeque<(Instant, Bytes)>,
    bytes: usize,
}

impl PrerollBuffer {
    fn push(&mut self, now: Instant, window: Duration, chunk: Bytes) {
        self.bytes += chunk.len();
        self.segments.push_back((now, chunk));
        while let Some((t, b)) = self.segments.front() {
            if now.duration_since(*t) > window && self.segments.len() > 1
                || self.bytes > PREROLL_MAX_BYTES
            {
                self.bytes -= b.len();
                self.segments.pop_front();
            } else {
                break;
            }
        }
    }

    fn clear(&mut self) {
        self.segments.clear();
        self.bytes = 0;
    }

    /// Age of the oldest buffered fragment — the pre-roll actually achieved.
    fn span(&self, now: Instant) -> Duration {
        self.segments
            .front()
            .map(|(t, _)| now.duration_since(*t))
            .unwrap_or(Duration::ZERO)
    }

    fn drain(&mut self) -> Vec<Bytes> {
        self.bytes = 0;
        self.segments.drain(..).map(|(_, b)| b).collect()
    }
}

// -----------------------------------------------------------------------------
// File writer
// -----------------------------------------------------------------------------

/// One in-progress event file. Bytes stream into `<final>.mp4.tmp`; only a
/// clean finalize renames to the final path (same invariant as the ad-hoc
/// segment recorder: an observable `.mp4` is always complete). fMP4 needs no
/// trailer, so a crash merely leaves a stale-but-harmless `.tmp`.
struct ActiveFile {
    file: tokio::fs::File,
    tmp_path: PathBuf,
    final_path: PathBuf,
    recording_ref: String,
    hasher: Sha256,
    bytes: u64,
    opened_at: Instant,
    /// Wall-clock start of the FILE's content (event trigger minus pre-roll).
    started_wall_ms: u64,
    preroll_ms: u64,
    part: u32,
}

/// Everything the DB row needs once the bytes are on disk.
struct FinalizedFile {
    recording_ref: String,
    final_path: PathBuf,
    bytes: u64,
    duration_ms: u64,
    started_wall_ms: u64,
    stopped_wall_ms: u64,
    preroll_ms: u64,
    hash_sha256: String,
    part: u32,
}

impl ActiveFile {
    async fn create(camera_id: &str, preroll_ms: u64, part: u32) -> anyhow::Result<Self> {
        let recording_ref = format!("clip_{}", uuid::Uuid::new_v4());
        let base = recording_base_dir().map_err(|e| anyhow::anyhow!("{e}"))?;
        let dir = camera_subdir(&base, camera_id, RecordingKind::Segment);
        tokio::fs::create_dir_all(&dir).await?;
        let final_path = dir.join(format!("{recording_ref}.mp4"));
        let tmp_path = dir.join(format!("{recording_ref}.mp4.tmp"));
        let file = tokio::fs::File::create(&tmp_path).await?;
        Ok(Self {
            file,
            tmp_path,
            final_path,
            recording_ref,
            hasher: Sha256::new(),
            bytes: 0,
            opened_at: Instant::now(),
            started_wall_ms: now_unix_ms().saturating_sub(preroll_ms),
            preroll_ms,
            part,
        })
    }

    async fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.file.write_all(data).await?;
        self.hasher.update(data);
        self.bytes += data.len() as u64;
        Ok(())
    }

    async fn finalize(mut self) -> anyhow::Result<FinalizedFile> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        drop(self.file);
        tokio::fs::rename(&self.tmp_path, &self.final_path).await?;
        let duration_ms =
            self.opened_at.elapsed().as_millis().min(u64::MAX as u128) as u64 + self.preroll_ms;
        Ok(FinalizedFile {
            recording_ref: self.recording_ref,
            final_path: self.final_path,
            bytes: self.bytes,
            duration_ms,
            started_wall_ms: self.started_wall_ms,
            stopped_wall_ms: now_unix_ms(),
            preroll_ms: self.preroll_ms,
            hash_sha256: hex::encode(self.hasher.finalize()),
            part: self.part,
        })
    }

    /// Drop the partial file (start failed mid-way / unplayable content).
    async fn discard(self) {
        drop(self.file);
        let _ = tokio::fs::remove_file(&self.tmp_path).await;
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// DB glue
// -----------------------------------------------------------------------------

/// `(owner_addon_id, org_id, retention_class)` of the local camera. `None` =
/// not a local camera (mesh-remote or already removed) — the recorder exits.
async fn camera_identity(camera_id: &str) -> Option<(String, String, String)> {
    let id = camera_id.to_string();
    tokio::task::spawn_blocking(move || {
        let pool = crate::db::global_pool()?;
        match crate::db::repository::camera_recording_identity(&pool, &id) {
            Ok(v) => v,
            Err(e) => {
                warn!(camera_id = %id, "[event_recorder] camera identity lookup failed: {e}");
                None
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Catalog the finalized file. Attribution goes to the camera's OWNING addon
/// so the recording shows up in the exact same dashboard surfaces (addon
/// storage stats, signed-URL playback) as addon-saved segments. On a DB
/// failure the file is purged — same compensation as the host-function path.
#[allow(clippy::too_many_arguments)]
async fn insert_event_row(
    camera_id: &str,
    identity: &(String, String, String),
    fin: FinalizedFile,
    event_meta_json: String,
    plate_text: Option<String>,
    adr_text: Option<String>,
    // Single representative event thumbnail (repurposed `plate_thumb_ref`).
    thumb_ref: Option<String>,
    // Retained NULL: the per-class `adr_thumb_ref` column is no longer written,
    // kept only so the catalog schema/back-compat rows stay valid.
    adr_thumb_ref: Option<String>,
) {
    let (owner_addon_id, org_id, retention_class) = identity.clone();
    let cam_for_db = camera_id.to_string();
    let path_for_purge = fin.final_path.clone();
    let inserted = tokio::task::spawn_blocking(move || {
        let Some(pool) = crate::db::global_pool() else {
            return Err(anyhow::anyhow!("no global DB pool"));
        };
        crate::db::repository::insert_recording(
            &pool,
            &fin.recording_ref,
            "segment",
            &owner_addon_id,
            &cam_for_db,
            &fin.final_path.to_string_lossy(),
            fin.bytes as i64,
            Some(fin.duration_ms.min(i64::MAX as u64) as i64),
            None,
            None,
            None,
            &fin.hash_sha256,
            &retention_class,
            Some(&org_id),
            Some(&event_meta_json),
            plate_text.as_deref(),
            adr_text.as_deref(),
            thumb_ref.as_deref(),
            adr_thumb_ref.as_deref(),
        )
        .map(|_| (fin.recording_ref, fin.duration_ms, fin.bytes, fin.part))
        .map_err(anyhow::Error::from)
    })
    .await;
    match inserted {
        Ok(Ok((recording_ref, duration_ms, bytes, part))) => {
            info!(
                camera_id,
                recording_ref = %recording_ref,
                duration_ms,
                bytes,
                part,
                "[event_recorder] event recording saved"
            );
        }
        Ok(Err(e)) => {
            warn!(
                camera_id,
                "[event_recorder] recordings insert failed (compensating purge): {e}"
            );
            let _ = tokio::fs::remove_file(&path_for_purge).await;
        }
        Err(e) => {
            warn!(
                camera_id,
                "[event_recorder] recordings insert task join failed: {e}"
            );
            let _ = tokio::fs::remove_file(&path_for_purge).await;
        }
    }
}

// -----------------------------------------------------------------------------
// Video stream wrapper
// -----------------------------------------------------------------------------

/// Live `camera:<id>` hub subscription plus the CURRENT init segment. The
/// publisher forwards a rebuilt pipeline's fresh `ftyp`/`moov` boxes through
/// the media channel, so the recorder mirrors the publisher's own init parse:
/// a `ftyp` opens `collecting`, non-`moof` boxes append to it, and the next
/// `moof` seals it as the new `init`.
struct VideoStream {
    handle: SubscriptionHandle,
    init: Bytes,
    collecting: Option<Vec<u8>>,
}

/// What a received chunk means for the recorder.
enum VideoEvent {
    /// A media segment (starts with `moof`, or a stray forwardable box).
    Media(Bytes),
    /// A new init segment was sealed — the PTS axis reset (pipeline rebuild);
    /// any open file must rotate before the carried media segment (the
    /// sealing `moof` chunk, first of the new axis) is written.
    InitReset(Bytes),
    /// Chunk consumed into the in-progress init collection; nothing to write.
    Absorbed,
}

impl VideoStream {
    async fn subscribe(camera_id: &str) -> Option<Self> {
        let stream_id = format!("camera:{camera_id}");
        match StreamHub::global().subscribe(&stream_id).await {
            Ok(handle) => match handle.init_segment.clone() {
                Some(init) => Some(Self {
                    handle,
                    init,
                    collecting: None,
                }),
                None => {
                    debug!(
                        camera_id,
                        "[event_recorder] subscribe yielded no init segment"
                    );
                    None
                }
            },
            Err(e) => {
                debug!(camera_id, "[event_recorder] video subscribe failed: {e}");
                None
            }
        }
    }

    fn classify(&mut self, chunk: Bytes) -> VideoEvent {
        match fmp4_box_kind(&chunk) {
            Some(kind) if &kind == b"ftyp" => {
                self.collecting = Some(chunk.to_vec());
                VideoEvent::Absorbed
            }
            Some(kind) if &kind == b"moof" => {
                if let Some(collected) = self.collecting.take() {
                    self.init = Bytes::from(collected);
                    return VideoEvent::InitReset(chunk);
                }
                VideoEvent::Media(chunk)
            }
            _ => {
                if let Some(collected) = self.collecting.as_mut() {
                    collected.extend_from_slice(&chunk);
                    return VideoEvent::Absorbed;
                }
                VideoEvent::Media(chunk)
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Recorder task
// -----------------------------------------------------------------------------

/// Returns whether the publish hook may respawn a recorder for this camera
/// later (`false` = permanently out of scope on this node).
async fn run_recorder(camera_id: &str) -> bool {
    // The id becomes a filesystem path component — same containment rule as
    // every other recording entry point. Real ids (`cam_<uuid>`) always pass;
    // this shields against synthetic bus publishers with hostile ids.
    if validate_camera_id(camera_id).is_err() {
        debug!(
            camera_id,
            "[event_recorder] camera id not path-safe, recorder not started"
        );
        return false;
    }
    // Node-local camera gate + identity for the catalog row. A mesh-remote
    // camera (its video lives on the owning node) never has a local row.
    let Some(identity) = camera_identity(camera_id).await else {
        debug!(
            camera_id,
            "[event_recorder] camera is not node-local, recorder not started"
        );
        return false;
    };

    let cfg = crate::vision::settings::get();
    let hysteresis = Duration::from_secs(cfg.event_stop_hysteresis_secs.max(1));
    let preroll = Duration::from_secs(cfg.event_preroll_secs);
    let max_duration = Duration::from_secs(cfg.event_max_duration_secs.max(60));

    info!(
        camera_id,
        hysteresis_s = hysteresis.as_secs(),
        preroll_s = preroll.as_secs(),
        "[event_recorder] recorder started"
    );

    let mut det_rx = detection_bus::subscribe(camera_id);
    let mut sm = EventStateMachine::new(hysteresis);
    let mut meta = EventMeta::default();
    let mut ring = PrerollBuffer::default();
    let mut video: Option<VideoStream> = None;
    let mut file: Option<ActiveFile> = None;
    let mut part: u32 = 0;
    let mut last_bus_msg = Instant::now();
    let mut video_retry_at = Instant::now();
    let mut start_blocked_until: Option<Instant> = None;
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // Keep the pre-roll subscription warm while idle (only when a pre-roll
        // window is configured — otherwise Branch B attaches per event).
        let want_video = !preroll.is_zero() || sm.recording();
        if want_video && video.is_none() && Instant::now() >= video_retry_at {
            video = VideoStream::subscribe(camera_id).await;
            if video.is_none() {
                video_retry_at = Instant::now() + VIDEO_RETRY;
            }
        }

        tokio::select! {
            msg = det_rx.recv() => match msg {
                Ok(m) => {
                    last_bus_msg = Instant::now();
                    handle_detections(
                        camera_id, m, &mut sm, &mut meta, &mut ring, &mut video,
                        &mut file, &mut part, preroll, &mut start_blocked_until,
                    ).await;
                }
                Err(RecvError::Lagged(_)) => { last_bus_msg = Instant::now(); }
                // The per-camera bus sender lives for the whole process, so
                // Closed is unreachable today; exit defensively if it ever is.
                Err(RecvError::Closed) => break,
            },
            chunk = video_recv(&mut video), if video.is_some() => match chunk {
                Ok(chunk) => {
                    handle_video_chunk(
                        camera_id, &identity, chunk, &mut sm, &mut meta, &mut ring,
                        &mut video, &mut file, &mut part, preroll,
                    ).await;
                }
                Err(RecvError::Lagged(n)) => {
                    // Missed fragments = a gap. While idle the ring just loses
                    // depth; mid-recording the file keeps only complete
                    // self-contained segments either side of the gap.
                    debug!(camera_id, missed = n, "[event_recorder] video broadcast lagged");
                }
                Err(RecvError::Closed) => {
                    // Publisher went terminal (camera stopped / removed /
                    // reconnecting). Finalize whatever is on disk.
                    video = None;
                    video_retry_at = Instant::now() + VIDEO_RETRY;
                    ring.clear();
                    if let Some(f) = file.take() {
                        finalize_and_catalog(camera_id, &identity, f, &meta).await;
                    }
                    if sm.recording() {
                        sm.on_stopped();
                        meta = EventMeta::default();
                        part = 0;
                    }
                }
            },
            _ = ticker.tick() => {
                let now = Instant::now();
                if sm.should_stop(now) {
                    if let Some(f) = file.take() {
                        finalize_and_catalog(camera_id, &identity, f, &meta).await;
                    }
                    sm.on_stopped();
                    meta = EventMeta::default();
                    part = 0;
                    if preroll.is_zero() {
                        // Release the hub subscription so Branch B detaches
                        // between events.
                        video = None;
                        ring.clear();
                    }
                } else if sm.recording() {
                    // Rotate an endless event (busy gate / stuck detection) so
                    // no single file grows unbounded.
                    let rotate = file
                        .as_ref()
                        .map(|f| f.opened_at.elapsed() >= max_duration)
                        .unwrap_or(false);
                    if rotate {
                        rotate_file(camera_id, &identity, &mut file, &mut part, &mut video, &meta)
                            .await;
                    }
                }
                if !sm.recording() && last_bus_msg.elapsed() >= IDLE_EXIT {
                    debug!(camera_id, "[event_recorder] idle (no analysis frames), exiting");
                    break;
                }
            }
        }
    }
    true
}

/// Awaitable video receive that borrows the optional stream. Only polled when
/// `video.is_some()` (select guard).
async fn video_recv(video: &mut Option<VideoStream>) -> Result<Bytes, RecvError> {
    match video.as_mut() {
        Some(v) => v.handle.receiver.recv().await,
        // Guarded out by `if video.is_some()`; pend forever if ever polled.
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_detections(
    camera_id: &str,
    msg: DetectionsMessage,
    sm: &mut EventStateMachine,
    meta: &mut EventMeta,
    ring: &mut PrerollBuffer,
    video: &mut Option<VideoStream>,
    file: &mut Option<ActiveFile>,
    part: &mut u32,
    preroll: Duration,
    start_blocked_until: &mut Option<Instant>,
) {
    if msg.items.is_empty() {
        return;
    }
    let now = Instant::now();
    if let Some(until) = *start_blocked_until {
        if !sm.recording() && now < until {
            return;
        }
        *start_blocked_until = None;
    }
    let started = sm.on_presence(now);
    // Presence (start/keep recording) fires on EVERY frame — a truck must trigger
    // recording immediately via the hot FAZA 1 stream. But vehicle bucketing only
    // folds ENRICHED (FAZA 2) frames: those alone carry the final `vehicle_id`
    // association + OCR/stan. Bucketing the hot stream would flood `vehicle_id = 0`
    // with every-frame, unstamped detections.
    if msg.enriched {
        meta.absorb(&msg.items);
    }
    if !started {
        return;
    }
    *part = 0;
    // EMPTY → RECORDING: make sure video is up (pre-roll = 0 subscribes here)
    // and open the file with init + buffered pre-roll.
    if video.is_none() {
        *video = VideoStream::subscribe(camera_id).await;
    }
    let Some(v) = video.as_ref() else {
        warn!(
            camera_id,
            "[event_recorder] video unavailable, event NOT recorded (retry in {}s)",
            VIDEO_RETRY.as_secs()
        );
        sm.on_stopped();
        *meta = EventMeta::default();
        *start_blocked_until = Some(now + VIDEO_RETRY);
        return;
    };
    let preroll_ms = ring.span(now).min(preroll).as_millis() as u64;
    match ActiveFile::create(camera_id, preroll_ms, *part).await {
        Ok(mut f) => {
            let mut ok = f.write(&v.init).await.is_ok();
            if ok {
                for seg in ring.drain() {
                    if f.write(&seg).await.is_err() {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                info!(
                    camera_id,
                    recording_ref = %f.recording_ref,
                    preroll_ms,
                    "[event_recorder] event recording started"
                );
                *file = Some(f);
            } else {
                warn!(
                    camera_id,
                    "[event_recorder] write failed at start, event dropped"
                );
                f.discard().await;
                sm.on_stopped();
                *meta = EventMeta::default();
                *start_blocked_until = Some(now + VIDEO_RETRY);
            }
        }
        Err(e) => {
            warn!(camera_id, "[event_recorder] cannot open event file: {e:#}");
            sm.on_stopped();
            *meta = EventMeta::default();
            *start_blocked_until = Some(now + VIDEO_RETRY);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_video_chunk(
    camera_id: &str,
    identity: &(String, String, String),
    chunk: Bytes,
    sm: &mut EventStateMachine,
    meta: &mut EventMeta,
    ring: &mut PrerollBuffer,
    video: &mut Option<VideoStream>,
    file: &mut Option<ActiveFile>,
    part: &mut u32,
    preroll: Duration,
) {
    let Some(v) = video.as_mut() else { return };
    let seg = match v.classify(chunk) {
        VideoEvent::Absorbed => return,
        VideoEvent::InitReset(seg) => {
            // New PTS axis: the buffered pre-roll belongs to the old axis and
            // an open file cannot span the boundary — rotate onto the new
            // init, then write the carried first segment of the new axis.
            ring.clear();
            if sm.recording() && file.is_some() {
                rotate_file(camera_id, identity, file, part, video, meta).await;
            }
            seg
        }
        VideoEvent::Media(seg) => seg,
    };
    if let Some(f) = file.as_mut() {
        if let Err(e) = f.write(&seg).await {
            warn!(
                camera_id,
                "[event_recorder] write failed mid-recording: {e}"
            );
            if let Some(f) = file.take() {
                finalize_and_catalog(camera_id, identity, f, meta).await;
            }
            sm.on_stopped();
            *meta = EventMeta::default();
            *part = 0;
        }
    } else if !preroll.is_zero() {
        ring.push(Instant::now(), preroll, seg);
    }
}

/// Finalize the open file and immediately continue the SAME event in a fresh
/// file (max-duration rotation or a pipeline-rebuild init reset). On any
/// failure the event simply ends — the next detection re-triggers cleanly.
async fn rotate_file(
    camera_id: &str,
    identity: &(String, String, String),
    file: &mut Option<ActiveFile>,
    part: &mut u32,
    video: &mut Option<VideoStream>,
    meta: &EventMeta,
) {
    if let Some(f) = file.take() {
        finalize_and_catalog(camera_id, identity, f, meta).await;
    }
    *part += 1;
    let Some(v) = video.as_ref() else { return };
    match ActiveFile::create(camera_id, 0, *part).await {
        Ok(mut f) => {
            if f.write(&v.init).await.is_ok() {
                *file = Some(f);
            } else {
                warn!(camera_id, "[event_recorder] rotation init write failed");
                f.discard().await;
            }
        }
        Err(e) => warn!(camera_id, "[event_recorder] rotation open failed: {e:#}"),
    }
}

async fn finalize_and_catalog(
    camera_id: &str,
    identity: &(String, String, String),
    file: ActiveFile,
    meta: &EventMeta,
) {
    // Never catalog an empty clip: an event that triggered on transient noise and
    // never enriched a vehicle/sign is dropped (file deleted), so the recordings
    // list only ever shows clips that actually contain a vehicle.
    if !meta.has_content() {
        file.discard().await;
        debug!(
            camera_id,
            "[event_recorder] event had no vehicle content — clip discarded (not cataloged)"
        );
        return;
    }
    match file.finalize().await {
        Ok(fin) => {
            let json = meta.to_json(
                fin.started_wall_ms,
                fin.stopped_wall_ms,
                fin.preroll_ms,
                fin.part,
            );
            let (plate_text, adr_text) = meta.winner_texts();
            // One representative truck thumbnail per event; carried in the
            // repurposed `plate_thumb_ref` column (the old per-class `adr_thumb`
            // is no longer populated — the list shows a single photo).
            let event_thumb = meta.event_thumb();
            insert_event_row(
                camera_id,
                identity,
                fin,
                json,
                plate_text,
                adr_text,
                event_thumb,
                None,
            )
            .await;
        }
        Err(e) => warn!(
            camera_id,
            "[event_recorder] finalize failed, file dropped: {e:#}"
        ),
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn det(klasa: &str, tekst: Option<&str>, stan: &[&str], track_id: u32) -> Detection {
        det_conf(klasa, tekst, None, stan, track_id)
    }

    fn det_conf(
        klasa: &str,
        tekst: Option<&str>,
        tekst_conf: Option<f32>,
        stan: &[&str],
        track_id: u32,
    ) -> Detection {
        Detection {
            klasa: klasa.into(),
            bbox: [0.1, 0.1, 0.2, 0.2],
            score: 0.9,
            stan: stan.iter().map(|s| s.to_string()).collect(),
            tekst: tekst.map(str::to_string),
            tekst_conf,
            tekst_thumb_ref: None,
            track_id,
            vehicle_id: 0,
            vx: 0.,
            vy: 0.,
        }
    }

    /// Like `det` but stamped with an explicit `vehicle_id` — for the per-truck
    /// routing tests.
    fn det_veh(klasa: &str, tekst: Option<&str>, track_id: u32, vehicle_id: u32) -> Detection {
        let mut d = det(klasa, tekst, &[], track_id);
        d.vehicle_id = vehicle_id;
        d
    }

    /// Like `det_veh` but placed at horizontal position `cx` (box center-x, 0..1)
    /// so the lane-clustering consolidation can tell fragments apart by position.
    fn det_veh_at(
        klasa: &str,
        tekst: Option<&str>,
        track_id: u32,
        vehicle_id: u32,
        cx: f32,
    ) -> Detection {
        let mut d = det_veh(klasa, tekst, track_id, vehicle_id);
        d.bbox = [(cx - 0.05).max(0.0), 0.1, 0.1, 0.2];
        d
    }

    /// Two trucks on the two ADJACENT LANES (well apart horizontally) must NOT
    /// mix their plates/ADR: lane clustering keeps them as two vehicles, and the
    /// scalar columns report the PRIMARY (most-frames) truck.
    #[test]
    fn event_meta_two_trucks_do_not_mix() {
        let mut meta = EventMeta::default();
        // Truck 1 (left lane, cx≈0.25) — plate + ADR, seen in 3 frames.
        for _ in 0..3 {
            meta.absorb(&[
                det_veh_at(CLASS_PLATE, Some("WGM11111"), 10, 1, 0.25),
                det_veh_at(CLASS_ADR, Some("30/1202"), 11, 1, 0.25),
            ]);
        }
        // Truck 2 (right lane, cx≈0.75) — a DIFFERENT plate, seen in 1 frame.
        meta.absorb(&[det_veh_at(CLASS_PLATE, Some("KR22222"), 20, 2, 0.75)]);

        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        let vehicles = v["vehicles"].as_array().expect("vehicles array");
        assert_eq!(vehicles.len(), 2, "two distinct lanes → two trucks");
        // Left lane owns WGM11111 + the ADR; right lane owns KR22222 and NO ADR.
        let t1 = vehicles.iter().find(|x| x["plate"] == "WGM11111").unwrap();
        assert_eq!(t1["adr"], "30/1202");
        assert_eq!(t1["detection_frames"], 3);
        let t2 = vehicles.iter().find(|x| x["plate"] == "KR22222").unwrap();
        assert!(t2["adr"].is_null(), "truck 2 has no ADR — not truck 1's");
        assert_eq!(t2["detection_frames"], 1);

        // Scalar columns = the PRIMARY vehicle (truck 1, more frames).
        let (plate, adr) = meta.winner_texts();
        assert_eq!(plate.as_deref(), Some("WGM11111"));
        assert_eq!(adr.as_deref(), Some("30/1202"));
    }

    /// Track fragmentation: ONE truck driving in / stopping / leaving is
    /// re-acquired by the IOU tracker under dozens of `vehicle_id`s — but it stays
    /// in its lane, so ALL fragments share a horizontal position and MUST collapse
    /// to a single vehicle (else the panel shows "50 vehicles" for one truck).
    #[test]
    fn event_meta_one_truck_many_fragments_collapse_to_one() {
        let mut meta = EventMeta::default();
        // Same truck (left lane, cx≈0.3) re-acquired as MANY different vehicle_ids,
        // reading its plate on only some fragments (the rest are bare boxes).
        for vid in [5u32, 9, 40, 77, 103, 150, 201] {
            meta.absorb(&[
                det_veh_at("vehicle", None, vid, vid, 0.30),
                det_veh_at(CLASS_PLATE, Some("WZ12345"), vid + 1, vid, 0.30),
            ]);
        }
        // Plus a swarm of read-less fragments at the same position (occlusion jitter).
        for vid in 300u32..320 {
            meta.absorb(&[det_veh_at("vehicle", None, vid, vid, 0.31)]);
        }

        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        let vehicles = v["vehicles"].as_array().expect("vehicles array");
        assert_eq!(vehicles.len(), 1, "one lane → exactly one vehicle");
        assert_eq!(vehicles[0]["plate"], "WZ12345");
        let (plate, _) = meta.winner_texts();
        assert_eq!(plate.as_deref(), Some("WZ12345"));
    }

    /// A single-vehicle event must keep the scalar columns byte-identical to the
    /// old single-bag output (no migration): one bucket → primary = that bucket.
    #[test]
    fn event_meta_single_truck_scalars_unchanged() {
        let mut single = EventMeta::default();
        single.absorb(&[det_veh(CLASS_PLATE, Some("WPL5HJ2"), 3, 1)]);
        single.absorb(&[det_veh(CLASS_ADR, Some("33/1088"), 4, 1)]);
        let (plate, adr) = single.winner_texts();
        assert_eq!(plate.as_deref(), Some("WPL5HJ2"));
        assert_eq!(adr.as_deref(), Some("33/1088"));

        // Top-level JSON classes/texts equal the single vehicle's (union of one).
        let v: serde_json::Value =
            serde_json::from_str(&single.to_json(0, 0, 0, 0)).expect("valid json");
        assert_eq!(v["texts"][CLASS_PLATE]["text"], "WPL5HJ2");
        assert_eq!(v["classes"][CLASS_PLATE], 1);
        assert_eq!(v["vehicles"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn state_machine_starts_on_first_presence() {
        let mut sm = EventStateMachine::new(Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(!sm.recording());
        assert!(sm.on_presence(t0), "first presence must start a recording");
        assert!(sm.recording());
        assert!(
            !sm.on_presence(t0 + Duration::from_secs(1)),
            "presence while recording must not re-start"
        );
    }

    #[test]
    fn state_machine_stops_after_hysteresis() {
        let mut sm = EventStateMachine::new(Duration::from_secs(10));
        let t0 = Instant::now();
        sm.on_presence(t0);
        assert!(!sm.should_stop(t0 + Duration::from_secs(9)));
        assert!(sm.should_stop(t0 + Duration::from_secs(10)));
        sm.on_stopped();
        assert!(!sm.recording());
        assert!(!sm.should_stop(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn state_machine_retrigger_extends_recording() {
        let mut sm = EventStateMachine::new(Duration::from_secs(10));
        let t0 = Instant::now();
        sm.on_presence(t0);
        // A new vehicle 8 s in (inside the hysteresis window) extends the
        // SAME recording: the stop deadline moves to last presence + 10 s.
        assert!(!sm.on_presence(t0 + Duration::from_secs(8)));
        assert!(!sm.should_stop(t0 + Duration::from_secs(17)));
        assert!(sm.should_stop(t0 + Duration::from_secs(18)));
    }

    #[test]
    fn event_meta_accumulates_votes() {
        let mut meta = EventMeta::default();
        meta.absorb(&[
            det("tablica_adr", Some("30/1202"), &[], 7),
            det("tablica_rejestracyjna", Some("WGM12345"), &[], 8),
        ]);
        meta.absorb(&[det(
            "tablica_adr",
            Some("30/1202"),
            &["nalepka_3 uszkodzona"],
            7,
        )]);
        meta.absorb(&[]); // empty frames are ignored
                          // All these `det`s carry vehicle_id=0 → one bucket; assert on it.
        let vm = &meta.vehicles[&0];
        assert_eq!(vm.frames, 2);
        assert_eq!(vm.classes["tablica_adr"], 2);
        // Raw per-variant frame counts still tallied.
        assert_eq!(vm.texts["tablica_adr"]["30/1202"].count, 2);
        assert_eq!(vm.texts["tablica_rejestracyjna"]["WGM12345"].count, 1);
        // Sticker states are voted per label (`<label> <state>` split).
        assert_eq!(vm.stany["nalepka_3"]["uszkodzona"], 1);
        assert_eq!(vm.tracks.len(), 2);

        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(1_000, 21_000, 5_000, 0)).expect("valid json");
        assert_eq!(v["event"], "vehicle_presence");
        assert_eq!(v["start_ts_ms"], 1_000);
        assert_eq!(v["stop_ts_ms"], 21_000);
        assert_eq!(v["preroll_ms"], 5_000);
        // Majority sticker state per label is emitted under `stany`.
        assert_eq!(v["stany"]["nalepka_3"], "uszkodzona");
        // Two consistent reads with no explicit conf (executor-path = full unit)
        // → agreement 1.0, confidence 1.0, reported.
        let adr = &v["texts"]["tablica_adr"];
        assert_eq!(adr["text"], "30/1202");
        assert_eq!(adr["unreadable"], false);
        assert_eq!(adr["votes"]["30/1202"], 2);
        assert_eq!(v["tracks"], 2);
    }

    /// Gate semantics after the confidence floor was disabled (0.0) and the
    /// AGREEMENT floor (0.34) became the sole trust signal, because the plate-OCR
    /// softmax is near-uniform. A consistent majority read (4/7 = 0.57 agreement)
    /// at NEAR-ZERO per-char confidence must now be REPORTED — never suppressed by
    /// the broken confidence — mirroring a plate read the same way thousands of
    /// times in the field.
    #[test]
    fn event_meta_reports_agreement_majority_at_low_confidence() {
        let mut meta = EventMeta::default();
        for s in [
            "M88901", "M88901", "M88901", "M88901", "N59156", "B67K71", "DRR740",
        ] {
            meta.absorb(&[det_conf(
                "tablica_rejestracyjna",
                Some(s),
                Some(0.05),
                &[],
                5,
            )]);
        }
        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        let plate = &v["texts"]["tablica_rejestracyjna"];
        assert_eq!(plate["unreadable"], false, "agreement majority is reported");
        assert_eq!(plate["text"], "M88901");
        assert_eq!(plate["votes"]["M88901"], 4);
    }

    /// Pure scatter — every frame a DIFFERENT string — stays `unreadable`: no
    /// variant clears the agreement floor, so the recorder reports nothing rather
    /// than a fabricated plate.
    #[test]
    fn event_meta_gates_pure_scatter_as_unreadable() {
        let mut meta = EventMeta::default();
        for s in ["M88901", "N59156", "B67K71", "DRR740"] {
            meta.absorb(&[det_conf(
                "tablica_rejestracyjna",
                Some(s),
                Some(0.05),
                &[],
                5,
            )]);
        }
        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        let plate = &v["texts"]["tablica_rejestracyjna"];
        // Each variant has agreement 0.25 < 0.34 → gated out.
        assert_eq!(plate["unreadable"], true, "scatter must be unreadable");
        assert!(plate["text"].is_null(), "no fabricated plate is reported");
    }

    /// A single high-confidence, valid read is reported immediately (agreement
    /// 1.0) — the gate must not require many frames.
    #[test]
    fn event_meta_single_confident_read_is_reported() {
        let mut meta = EventMeta::default();
        meta.absorb(&[det_conf(
            "tablica_rejestracyjna",
            Some("WPL5HJ2"),
            Some(0.94),
            &[],
            3,
        )]);
        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        let plate = &v["texts"]["tablica_rejestracyjna"];
        assert_eq!(plate["text"], "WPL5HJ2");
        assert_eq!(plate["unreadable"], false);
        assert!((plate["agreement"].as_f64().unwrap() - 1.0).abs() < 1e-6);
    }

    /// `winner_texts` mirrors the JSON gate: a confident consistent plate + a
    /// confident ADR are surfaced as the indexed search columns; an unreadable
    /// class collapses to `None`.
    #[test]
    fn event_meta_winner_texts_extracts_plate_and_adr() {
        let mut meta = EventMeta::default();
        meta.absorb(&[
            det_conf(CLASS_PLATE, Some("WGM12345"), Some(0.9), &[], 3),
            det_conf(CLASS_ADR, Some("30/1202"), Some(0.9), &[], 4),
        ]);
        meta.absorb(&[det_conf(CLASS_PLATE, Some("WGM12345"), Some(0.9), &[], 3)]);
        let (plate, adr) = meta.winner_texts();
        assert_eq!(plate.as_deref(), Some("WGM12345"));
        assert_eq!(adr.as_deref(), Some("30/1202"));

        // An occluded, low-confidence disagreeing plate → column stays NULL.
        let mut occluded = EventMeta::default();
        for s in ["M88901", "N59156", "B67K71"] {
            occluded.absorb(&[det_conf(CLASS_PLATE, Some(s), Some(0.3), &[], 5)]);
        }
        let (plate, adr) = occluded.winner_texts();
        assert!(
            plate.is_none(),
            "occluded plate must not populate the column"
        );
        assert!(adr.is_none(), "absent ADR class → None");
    }

    /// The single representative thumbnail: the recorder keeps the
    /// highest-confidence thumbnail per class and the event photo prefers the
    /// best PLATE-read frame (the truck facing the camera).
    #[test]
    fn event_meta_picks_best_plate_frame_as_event_thumb() {
        let with_thumb = |klasa: &str, conf: f32, thumb: &str| {
            let mut d = det_conf(klasa, Some("WGM12345"), Some(conf), &[], 3);
            d.tekst_thumb_ref = Some(thumb.to_string());
            d
        };
        let mut meta = EventMeta::default();
        // Plate thumbs arrive at rising then falling confidence; the recorder must
        // retain the 0.92 one and use it as the event photo even though the ADR
        // frame is present too.
        meta.absorb(&[with_thumb(CLASS_PLATE, 0.60, "snap_plate_a")]);
        meta.absorb(&[with_thumb(CLASS_PLATE, 0.92, "snap_plate_b")]);
        meta.absorb(&[with_thumb(CLASS_PLATE, 0.70, "snap_plate_c")]);
        meta.absorb(&[with_thumb(CLASS_ADR, 0.85, "snap_adr_a")]);
        // A read with no thumb ref must not clear the retained best.
        meta.absorb(&[det_conf(CLASS_PLATE, Some("WGM12345"), Some(0.99), &[], 3)]);

        assert_eq!(meta.event_thumb().as_deref(), Some("snap_plate_b"));
    }

    /// With no plate frame, the event photo falls back to the best ADR frame,
    /// then to any remaining class's best thumbnail.
    #[test]
    fn event_thumb_falls_back_when_no_plate() {
        let with_thumb = |klasa: &str, conf: f32, thumb: &str| {
            let mut d = det_conf(klasa, Some("30/1202"), Some(conf), &[], 3);
            d.tekst_thumb_ref = Some(thumb.to_string());
            d
        };
        let mut adr_only = EventMeta::default();
        adr_only.absorb(&[with_thumb(CLASS_ADR, 0.85, "snap_adr_a")]);
        assert_eq!(adr_only.event_thumb().as_deref(), Some("snap_adr_a"));

        let mut other = EventMeta::default();
        other.absorb(&[with_thumb("nalepka", 0.40, "snap_x")]);
        assert_eq!(other.event_thumb().as_deref(), Some("snap_x"));

        assert_eq!(EventMeta::default().event_thumb(), None);
    }

    /// A photo must ALWAYS be present: when the PRIMARY truck (most frames) never
    /// captured a thumbnail but ANOTHER vehicle did, the scalar event thumbnail
    /// falls back to that vehicle's photo instead of returning `None`.
    #[test]
    fn event_thumb_falls_back_to_non_primary_vehicle() {
        let mut meta = EventMeta::default();
        // Primary truck (left lane, cx≈0.25): 3 frames, plate WPRIM11, NO thumbnail.
        for _ in 0..3 {
            meta.absorb(&[det_veh_at(CLASS_PLATE, Some("WPRIM11"), 10, 1, 0.25)]);
        }
        // Secondary vehicle (right lane, cx≈0.75): 1 frame, no read, but a thumbnail.
        let mut d = det_veh_at("vehicle", None, 20, 2, 0.75);
        d.tekst_thumb_ref = Some("snap_scene".to_string());
        meta.absorb(&[d]);

        // Primary is the left-lane truck (more frames) and has no thumb → fall back
        // to the right-lane vehicle's scene photo rather than None.
        assert_eq!(meta.event_thumb().as_deref(), Some("snap_scene"));
    }

    /// An event that enriched NOTHING (no vehicle, no sign) must report no
    /// content, so `finalize_and_catalog` discards it instead of cataloging an
    /// empty clip. A single vehicle box (a real car) IS content.
    #[test]
    fn event_meta_empty_has_no_content() {
        assert!(
            !EventMeta::default().has_content(),
            "nothing absorbed → empty"
        );

        let mut noise = EventMeta::default();
        // A sign flash with no readable text and no vehicle box → still empty.
        noise.absorb(&[det_veh("nalepka_3", None, 0, 0)]);
        assert!(
            !noise.has_content(),
            "a read-less sign with no vehicle box is not content"
        );

        let mut car = EventMeta::default();
        car.absorb(&[det_veh("vehicle", None, 7, 7)]);
        assert!(car.has_content(), "a real vehicle box IS content");
    }

    /// Sticker-state aggregation: each `<label> <state>` frame votes for the
    /// label's state; the reported state is the frame majority per label.
    #[test]
    fn event_meta_aggregates_stan_majority_per_label() {
        let mut meta = EventMeta::default();
        // nalepka_3 reads "czysta" ×4 and "uszkodzona" ×1 → majority czysta.
        for _ in 0..4 {
            meta.absorb(&[det("nalepka", None, &["nalepka_3 czysta"], 1)]);
        }
        meta.absorb(&[det("nalepka", None, &["nalepka_3 uszkodzona"], 1)]);
        // znak_srodowiskowy is uszkodzona in the only frame it appears.
        meta.absorb(&[det("nalepka", None, &["znak_srodowiskowy uszkodzona"], 2)]);
        // A bare single-token state ("ok") has no separate label, so it is keyed
        // under the detection CLASS ("pojazd") — the sticker it belongs to (per
        // the per-class aggregation).
        meta.absorb(&[det("pojazd", None, &["ok"], 3)]);

        // All these `det`s carry vehicle_id=0 → one bucket; assert on it.
        let winners = meta.vehicles[&0].stany_winners();
        assert_eq!(winners["nalepka_3"], "czysta");
        assert_eq!(winners["znak_srodowiskowy"], "uszkodzona");
        assert_eq!(winners["pojazd"], "ok");

        let v: serde_json::Value =
            serde_json::from_str(&meta.to_json(0, 0, 0, 0)).expect("valid json");
        assert_eq!(v["stany"]["nalepka_3"], "czysta");
        assert_eq!(v["stany"]["znak_srodowiskowy"], "uszkodzona");
    }

    #[test]
    fn split_stan_separates_label_and_state() {
        assert_eq!(
            split_stan("nalepka_3 czysta"),
            ("nalepka_3".to_string(), "czysta".to_string())
        );
        assert_eq!(
            split_stan("znak_srodowiskowy uszkodzona"),
            ("znak_srodowiskowy".to_string(), "uszkodzona".to_string())
        );
        // A bare flag becomes its own label + state.
        assert_eq!(split_stan("ok"), ("ok".to_string(), "ok".to_string()));
    }

    #[test]
    fn preroll_buffer_trims_by_time() {
        let mut ring = PrerollBuffer::default();
        let window = Duration::from_secs(5);
        let t0 = Instant::now();
        ring.push(t0, window, Bytes::from_static(b"a"));
        ring.push(
            t0 + Duration::from_secs(3),
            window,
            Bytes::from_static(b"b"),
        );
        // 7 s later the first fragment is out of the window and gets trimmed.
        ring.push(
            t0 + Duration::from_secs(7),
            window,
            Bytes::from_static(b"c"),
        );
        let drained = ring.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(&drained[0][..], b"b");
        assert_eq!(&drained[1][..], b"c");
        assert_eq!(ring.bytes, 0);
    }

    #[test]
    fn preroll_span_reports_achieved_window() {
        let mut ring = PrerollBuffer::default();
        let window = Duration::from_secs(5);
        let t0 = Instant::now();
        assert_eq!(ring.span(t0), Duration::ZERO);
        ring.push(t0, window, Bytes::from_static(b"a"));
        assert_eq!(
            ring.span(t0 + Duration::from_secs(2)),
            Duration::from_secs(2)
        );
    }

    fn mp4_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = (8 + payload.len()) as u32;
        let mut out = size.to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn box_kind_parses_top_level_fourcc() {
        assert_eq!(fmp4_box_kind(&mp4_box(b"moof", &[1, 2])), Some(*b"moof"));
        assert_eq!(fmp4_box_kind(&mp4_box(b"ftyp", &[])), Some(*b"ftyp"));
        assert_eq!(fmp4_box_kind(&[0, 0]), None);
    }
}
