// =============================================================================
// File: services/camera_ingest/gesture.rs — gestures on robot cameras
// =============================================================================
//
// For every LOCAL robot whose operator turned "Gesty" on, a single worker takes
// every new camera frame, finds the person (MoveNet, 17 body points, tracked
// from frame to frame), follows the hands (MediaPipe hand landmarks, tracked from
// the previous frame; the palm detector searches only near wrists still missing
// a hand) and recognizes two gestures over time:
//
//   * wave  — a raised hand swinging side to side → the robot says hello;
//   * heart — a finger heart, two hands joined into a heart, or both arms making
//             a heart over the head → the robot answers with its heart.
//
// The reaction goes through the robot addon as a system action, so the addon's
// own gates (e-stop, link online) still decide whether the robot moves. Enabling
// gestures is the operator's authorization for these reactions. Each analyzed
// frame is also published to the camera overlay (skeleton, hands, gesture).
//
// Nothing here runs unless a robot has gestures on; the worker idles otherwise.

#![cfg(feature = "camera")]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tracing::{info, warn};

use crate::mesh::robot_control::RobotAction;
use crate::services::detection_bus::{self, PoseItem, PoseMessage};
use crate::vision::hands::{Hand, HandTracker, Pt, Region};
use crate::vision::{PoseDetection, PoseEstimator};

/// A body point must reach this score to be used.
const MIN_KEYPOINT_SCORE: f32 = 0.3;
/// Single-pose MoveNet always returns its best guess, even with nobody in view
/// (a coat on a hanger). A body counts only when this many points are found…
const MIN_BODY_KEYPOINTS: usize = 8;
/// …and their mean score reaches this.
const MIN_BODY_SCORE: f32 = 0.4;
/// Wrists drive the wave, so they need a firmer detection than other points.
const MIN_WRIST_SCORE: f32 = 0.4;
/// Window over which swings of a raised hand are counted.
const WAVE_WINDOW_MS: u64 = 2000;
/// Direction changes of the hand needed for a wave.
const WAVE_REVERSALS: usize = 3;
/// Smallest swing (hand vs elbow, in shoulder widths) counted as a direction
/// change. A real wave swings ±0.5 or more; keypoint jitter of a still arm stays
/// around 0.1, so this sits well clear of it.
const WAVE_MIN_SWING: f32 = 0.3;
/// A hand lowered or lost for this long ends the raise. Shorter gaps are a fast
/// swing blurring the hand, or a keypoint misread for a few frames — at the
/// camera's full frame rate those come every few frames during a real wave and
/// must not restart the count.
const WAVE_GAP_MS: u64 = 400;
/// How long a heart shape must be held.
const HEART_HOLD_MS: u64 = 600;
/// Minimum gap between two reactions of the same gesture on one robot.
const GESTURE_COOLDOWN_MS: u64 = 8000;
/// Minimum gap between any two reactions on one robot (a move takes a few seconds).
const ANY_REACTION_GAP_MS: u64 = 4000;
/// How long the recognized gesture's name stays on the overlay.
const LABEL_MS: u64 = 2500;
/// No reaction while an operator drives the robot by hand — a gesture answer
/// must not cut into manual control.
const MANUAL_CONTROL_QUIET: Duration = Duration::from_secs(5);

// COCO body points used here.
const NOSE: usize = 0;
const L_SHOULDER: usize = 5;
const R_SHOULDER: usize = 6;
const L_ELBOW: usize = 7;
const R_ELBOW: usize = 8;
const L_WRIST: usize = 9;
const R_WRIST: usize = 10;

// MediaPipe hand points used here.
const H_WRIST: usize = 0;
const H_THUMB_MCP: usize = 2;
const H_THUMB_TIP: usize = 4;
const H_INDEX_MCP: usize = 5;
const H_INDEX_PIP: usize = 6;
const H_INDEX_DIP: usize = 7;
const H_INDEX_TIP: usize = 8;
const H_MIDDLE_MCP: usize = 9;
const H_MIDDLE_TIP: usize = 12;
const H_RING_MCP: usize = 13;
const H_RING_TIP: usize = 16;
const H_PINKY_MCP: usize = 17;
const H_PINKY_TIP: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gesture {
    Wave,
    Heart,
}

impl Gesture {
    fn action(self) -> RobotAction {
        match self {
            Gesture::Wave => RobotAction::Hello,
            Gesture::Heart => RobotAction::Heart,
        }
    }

    /// Overlay id of the gesture; the dashboard translates it.
    fn id(self) -> &'static str {
        match self {
            Gesture::Wave => "wave",
            Gesture::Heart => "heart",
        }
    }
}

/// Body points of one frame, indexed by COCO id; `None` when not found.
pub type Body = [Option<Pt>; 17];

/// The person in the frame, or `None` when the pose model only guessed.
fn body_from(det: &PoseDetection) -> Option<Body> {
    let found: Vec<f32> = det
        .keypoints
        .iter()
        .filter(|k| k.score >= MIN_KEYPOINT_SCORE)
        .map(|k| k.score)
        .collect();
    if found.len() < MIN_BODY_KEYPOINTS
        || found.iter().sum::<f32>() / (found.len() as f32) < MIN_BODY_SCORE
    {
        return None;
    }
    let mut body = [None; 17];
    for k in &det.keypoints {
        let min = if matches!(k.id as usize, L_WRIST | R_WRIST) {
            MIN_WRIST_SCORE
        } else {
            MIN_KEYPOINT_SCORE
        };
        if (k.id as usize) < 17 && k.score >= min {
            body[k.id as usize] = Some(Pt { x: k.x, y: k.y });
        }
    }
    Some(body)
}

fn dist(a: Pt, b: Pt) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

fn shoulder_width(body: &Body) -> Option<f32> {
    match (body[L_SHOULDER], body[R_SHOULDER]) {
        (Some(l), Some(r)) => Some(dist(l, r)).filter(|w| *w > 1.0),
        _ => None,
    }
}

/// Hand size: wrist → middle-finger base.
fn hand_size(h: &Hand) -> f32 {
    dist(h.landmarks[H_WRIST], h.landmarks[H_MIDDLE_MCP]).max(1.0)
}

/// Middle, ring and pinky curled into the palm.
fn three_fingers_folded(h: &Hand) -> bool {
    let w = h.landmarks[H_WRIST];
    [(H_MIDDLE_TIP, H_MIDDLE_MCP), (H_RING_TIP, H_RING_MCP), (H_PINKY_TIP, H_PINKY_MCP)]
        .iter()
        .all(|&(tip, mcp)| dist(h.landmarks[tip], w) < dist(h.landmarks[mcp], w) * 1.2)
}

/// Side of the line `a → b` that `p` lies on (sign of the cross product).
fn side(a: Pt, b: Pt, p: Pt) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

/// Korean finger heart: the bent index finger and the thumb cross near their
/// tips, the other fingers folded. "Cross" is literal — the thumb's tip and its
/// base lie on opposite sides of the index finger — so a plain pinch (holding a
/// pen or a card corner), where the thumb only meets the index tip, is not one.
fn finger_heart(h: &Hand) -> bool {
    let size = hand_size(h);
    let l = &h.landmarks;
    let index_len = dist(l[H_INDEX_MCP], l[H_INDEX_PIP])
        + dist(l[H_INDEX_PIP], l[H_INDEX_DIP])
        + dist(l[H_INDEX_DIP], l[H_INDEX_TIP]);
    let index_bent = dist(l[H_INDEX_MCP], l[H_INDEX_TIP]) < 0.85 * index_len;
    let crossed = side(l[H_INDEX_MCP], l[H_INDEX_TIP], l[H_THUMB_TIP])
        * side(l[H_INDEX_MCP], l[H_INDEX_TIP], l[H_THUMB_MCP])
        < 0.0;
    dist(l[H_THUMB_TIP], l[H_INDEX_TIP]) < 0.45 * size
        && index_bent
        && crossed
        && three_fingers_folded(h)
}

/// Two hands joined into a heart: index tips touch at the top, thumb tips touch
/// at the bottom point, with the heart's opening between them — steepled or
/// praying hands (fingers pressed together) have no opening.
fn two_hand_heart(a: &Hand, b: &Hand) -> bool {
    let size = (hand_size(a) + hand_size(b)) / 2.0;
    let (la, lb) = (&a.landmarks, &b.landmarks);
    let index_gap = dist(la[H_INDEX_TIP], lb[H_INDEX_TIP]);
    let thumb_gap = dist(la[H_THUMB_TIP], lb[H_THUMB_TIP]);
    let index_y = (la[H_INDEX_TIP].y + lb[H_INDEX_TIP].y) / 2.0;
    let thumb_y = (la[H_THUMB_TIP].y + lb[H_THUMB_TIP].y) / 2.0;
    let opening = dist(la[H_INDEX_MCP], lb[H_INDEX_MCP]);
    index_gap < 0.6 * size
        && thumb_gap < 0.6 * size
        && thumb_y - index_y > 0.4 * size
        && opening > 0.8 * size
}

/// Both arms making a heart over the head: wrists joined above the top of the
/// head, elbows raised above the shoulders and spread wider than them — hands
/// clasped behind the head keep the wrists lower and the elbows down.
fn arms_heart(body: &Body) -> bool {
    let (Some(nose), Some(sw)) = (body[NOSE], shoulder_width(body)) else {
        return false;
    };
    match (
        body[L_WRIST],
        body[R_WRIST],
        body[L_ELBOW],
        body[R_ELBOW],
        body[L_SHOULDER],
        body[R_SHOULDER],
    ) {
        (Some(lw), Some(rw), Some(le), Some(re), Some(ls), Some(rs)) => {
            let head_top = nose.y - 0.5 * sw;
            lw.y < head_top
                && rw.y < head_top
                && dist(lw, rw) < 0.8 * sw
                && le.y < ls.y
                && re.y < rs.y
                && (le.x - re.x).abs() > 1.2 * sw
        }
        _ => false,
    }
}

fn heart_shape(body: Option<&Body>, hands: &[Hand]) -> bool {
    let pair = hands.iter().enumerate().any(|(i, a)| {
        hands[i + 1..].iter().any(|b| two_hand_heart(a, b))
    });
    hands.iter().any(finger_heart) || pair || body.is_some_and(arms_heart)
}

/// What one arm does on this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Arm {
    /// Hand raised: its sideways position `x` and the length `unit` that one
    /// shoulder width spans, both in pixels. Only the movement counts, so `x`
    /// may be relative to anything that stays put during a wave.
    Raised { x: f32, unit: f32 },
    /// Hand seen, not raised.
    Lowered,
    /// The wrist (or the arm) was not found on this frame.
    Lost,
}

/// One arm's pose. The hand position is the palm center of this arm's tracked
/// hand: the hand tracker follows a swinging hand on every frame, while the pose
/// model's wrist lags and blurs away on a fast swing. Without a tracked hand the
/// pose model's wrist is used.
fn arm(body: &Body, hands: &[Hand], wrist: usize, elbow: usize, shoulder: usize) -> Arm {
    let (Some(e), Some(s), Some(sw)) = (body[elbow], body[shoulder], shoulder_width(body)) else {
        return Arm::Lost;
    };
    // This arm's hand: its wrist next to the pose model's (lagging) wrist, or —
    // with that wrist lost — within a forearm of the elbow.
    let (anchor, reach) = match body[wrist] {
        Some(w) => (w, sw),
        None => (e, 1.2 * sw),
    };
    let hand = hands
        .iter()
        .filter(|h| dist(h.landmarks[H_WRIST], anchor) < reach)
        .min_by(|a, b| {
            dist(a.landmarks[H_WRIST], anchor).total_cmp(&dist(b.landmarks[H_WRIST], anchor))
        });
    let Some(w) = hand.map(|h| h.landmarks[H_MIDDLE_MCP]).or(body[wrist]) else {
        return Arm::Lost;
    };
    // Raised: the hand above the elbow AND the shoulder — a hand at the ear or the
    // chin (a phone call, a thinking pose) stays below the shoulder line.
    if w.y < e.y && w.y < s.y {
        Arm::Raised { x: w.x - e.x, unit: sw }
    } else {
        Arm::Lowered
    }
}

/// Hand widths per shoulder width: scales a hand-only swing to the same unit as
/// an arm's.
const HAND_SIZES_PER_SHOULDER: f32 = 4.0;

/// An open hand with the fingers up — the waving hand.
fn open_upright(h: &Hand) -> bool {
    let (w, m) = (h.landmarks[H_WRIST], h.landmarks[H_MIDDLE_MCP]);
    let size = hand_size(h);
    let extended = [(H_INDEX_TIP, H_INDEX_MCP), (H_MIDDLE_TIP, H_MIDDLE_MCP), (H_RING_TIP, H_RING_MCP), (H_PINKY_TIP, H_PINKY_MCP)]
        .iter()
        .filter(|&&(tip, mcp)| dist(h.landmarks[tip], w) > dist(h.landmarks[mcp], w) * 1.4)
        .count();
    w.y - m.y > 0.5 * size && extended >= 3
}

/// The wave read from the hand alone — an open hand, fingers up, moving side to
/// side — whether or not the pose model sees the person. Of several such hands
/// the one nearest the hand followed so far is taken, so two hands are never
/// mixed into one swing; otherwise the highest one.
fn waving_hand(hands: &[Hand], previous_x: Option<f32>) -> Arm {
    if hands.is_empty() {
        return Arm::Lost;
    }
    let palm = |h: &Hand| h.landmarks[H_MIDDLE_MCP];
    let open = hands.iter().filter(|h| open_upright(h));
    let chosen = match previous_x {
        Some(px) => open.min_by(|a, b| (palm(a).x - px).abs().total_cmp(&(palm(b).x - px).abs())),
        None => open.min_by(|a, b| palm(a).y.total_cmp(&palm(b).y)),
    };
    match chosen {
        Some(h) => Arm::Raised {
            x: palm(h).x,
            unit: HAND_SIZES_PER_SHOULDER * hand_size(h),
        },
        None => Arm::Lowered,
    }
}

fn contains(r: &Region, p: Pt) -> bool {
    p.x >= r.x && p.x < r.x + r.size && p.y >= r.y && p.y < r.y + r.size
}

/// Hands found twice (one hand inside both wrist regions) are one hand: keep the
/// more confident copy.
fn dedupe_hands(mut hands: Vec<Hand>) -> Vec<Hand> {
    hands.sort_by(|a, b| b.presence.total_cmp(&a.presence));
    let mut kept: Vec<Hand> = Vec::new();
    for h in hands {
        let same = kept.iter().any(|k| {
            dist(k.landmarks[H_WRIST], h.landmarks[H_WRIST]) < 0.5 * hand_size(k).min(hand_size(&h))
        });
        if !same {
            kept.push(h);
        }
    }
    kept
}

/// Direction changes in a series of offsets, ignoring wiggles smaller than
/// `WAVE_MIN_SWING` (hysteresis on the running extreme).
fn count_reversals(offsets: impl Iterator<Item = f32>) -> usize {
    let mut reversals = 0;
    let mut extreme: Option<f32> = None;
    let mut rising: Option<bool> = None;
    for x in offsets {
        let Some(e) = extreme else {
            extreme = Some(x);
            continue;
        };
        match rising {
            None => {
                if (x - e).abs() >= WAVE_MIN_SWING {
                    rising = Some(x > e);
                    extreme = Some(x);
                }
            }
            Some(up) => {
                if (up && x > e) || (!up && x < e) {
                    extreme = Some(x);
                } else if (x - e).abs() >= WAVE_MIN_SWING {
                    reversals += 1;
                    rising = Some(!up);
                    extreme = Some(x);
                }
            }
        }
    }
    reversals
}

/// One arm's recent swing: the hand's offsets during one raise. A hand lowered or lost
/// for longer than `WAVE_GAP_MS` clears it, so two separate raises never join
/// into one wave.
#[derive(Default)]
struct ArmSeries {
    samples: VecDeque<(u64, f32)>,
    /// Direction changes in the current series.
    reversals: usize,
    /// Position and unit of the raise's first sample: later samples are
    /// measured from it in that unit, so a unit that jitters frame to frame (a
    /// hand's size) never turns into a swing.
    origin: Option<(f32, f32)>,
    /// Last raw position, pixels.
    last_x: Option<f32>,
}

impl ArmSeries {
    /// Add this frame's arm pose; true when the series now holds a wave.
    fn push(&mut self, ts_ms: u64, arm: Arm) -> bool {
        let x = match arm {
            Arm::Raised { x, unit } => {
                let (x0, u0) = *self.origin.get_or_insert((x, unit.max(1.0)));
                self.last_x = Some(x);
                (x - x0) / u0
            }
            Arm::Lowered | Arm::Lost => {
                if self
                    .samples
                    .back()
                    .is_some_and(|(t, _)| ts_ms.saturating_sub(*t) > WAVE_GAP_MS)
                {
                    self.clear();
                }
                return false;
            }
        };
        // No averaging: a normal wave swings in ~2 frames per direction, and an
        // average of those flattens the very swing being counted. Keypoint
        // jitter is already ignored by WAVE_MIN_SWING's hysteresis.
        self.samples.push_back((ts_ms, x));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| ts_ms.saturating_sub(*t) > WAVE_WINDOW_MS)
        {
            self.samples.pop_front();
        }
        self.reversals = count_reversals(self.samples.iter().map(|(_, x)| *x));
        self.reversals >= WAVE_REVERSALS
    }

    fn clear(&mut self) {
        self.samples.clear();
        self.reversals = 0;
        self.origin = None;
        self.last_x = None;
    }
}

/// Gesture recognition over time for one camera.
#[derive(Default)]
pub struct Recognizer {
    /// The waving hand itself, read from the hand tracker.
    hand: ArmSeries,
    /// The arms of the pose model — for a hand too far or too blurred to track.
    left: ArmSeries,
    right: ArmSeries,
    heart_since: Option<u64>,
    /// Frames with a raised hand and the most swings one raise reached, since
    /// the last [`Recognizer::take_wave_stats`] — how close people come to a wave.
    raised_frames: u32,
    peak_reversals: usize,
}

impl Recognizer {
    /// `(raised frames, most swings)` since the previous call; resets both.
    fn take_wave_stats(&mut self) -> (u32, usize) {
        let stats = (self.raised_frames, self.peak_reversals);
        self.raised_frames = 0;
        self.peak_reversals = 0;
        stats
    }

    /// Feed one analyzed frame; returns a gesture when one completes. The state
    /// that produced it is cleared, so one gesture fires once.
    pub fn update(&mut self, ts_ms: u64, body: Option<&Body>, hands: &[Hand]) -> Option<Gesture> {
        if heart_shape(body, hands) {
            let since = *self.heart_since.get_or_insert(ts_ms);
            if ts_ms.saturating_sub(since) >= HEART_HOLD_MS {
                self.heart_since = None;
                self.hand.clear();
                self.left.clear();
                self.right.clear();
                return Some(Gesture::Heart);
            }
        } else {
            self.heart_since = None;
        }

        let (left, right) = match body {
            Some(b) => (
                arm(b, hands, L_WRIST, L_ELBOW, L_SHOULDER),
                arm(b, hands, R_WRIST, R_ELBOW, R_SHOULDER),
            ),
            None => (Arm::Lost, Arm::Lost),
        };
        let hand = waving_hand(hands, self.hand.last_x);
        tracing::debug!(
            ?hand,
            ?left,
            ?right,
            hand_swings = self.hand.reversals,
            left_swings = self.left.reversals,
            right_swings = self.right.reversals,
            hands = hands.len(),
            "[gesture] wave"
        );
        let hand_waved = self.hand.push(ts_ms, hand);
        let left_waved = self.left.push(ts_ms, left);
        let right_waved = self.right.push(ts_ms, right);
        if [hand, left, right].iter().any(|a| matches!(a, Arm::Raised { .. })) {
            self.raised_frames += 1;
        }
        self.peak_reversals = self
            .peak_reversals
            .max(self.hand.reversals)
            .max(self.left.reversals)
            .max(self.right.reversals);
        if hand_waved || left_waved || right_waved {
            self.hand.clear();
            self.left.clear();
            self.right.clear();
            self.heart_since = None;
            return Some(Gesture::Wave);
        }
        None
    }
}

/// Square regions to search for hands, each with the number of hands it should
/// hold: around each found wrist, reaching past it along the forearm. Wrists
/// close together share one region (a two-hand heart).
fn hand_regions(body: &Body) -> Vec<(Region, usize)> {
    let mut centers: Vec<(Pt, f32, usize)> = Vec::new();
    for (wrist, elbow) in [(L_WRIST, L_ELBOW), (R_WRIST, R_ELBOW)] {
        let (Some(w), Some(e)) = (body[wrist], body[elbow]) else {
            continue;
        };
        let forearm = dist(w, e).max(20.0);
        let c = Pt {
            x: w.x + 0.4 * (w.x - e.x),
            y: w.y + 0.4 * (w.y - e.y),
        };
        centers.push((c, forearm * 2.2, 1));
    }
    if centers.len() == 2 && dist(centers[0].0, centers[1].0) < centers[0].1.max(centers[1].1) * 0.6 {
        let (a, b) = (centers[0], centers[1]);
        let mid = Pt {
            x: (a.0.x + b.0.x) / 2.0,
            y: (a.0.y + b.0.y) / 2.0,
        };
        centers = vec![(mid, a.1.max(b.1) + dist(a.0, b.0), 2)];
    }
    centers
        .into_iter()
        .map(|(c, size, n)| {
            let region = Region {
                x: c.x - size / 2.0,
                y: c.y - size / 2.0,
                size,
            };
            (region, n)
        })
        .collect()
}

// -----------------------------------------------------------------------------
// Worker
// -----------------------------------------------------------------------------

struct Models {
    pose: crate::vision::movenet::MovenetEngine,
    hands: HandTracker,
}

/// Camera-CV engine whose bundle holds the gesture models.
const ENGINE_ID: &str = "gesture-cv";

/// Download the gesture models when they are missing, so turning gestures on is
/// all a user does on a new machine. Pinned upstream files are sha256-checked.
/// When an admin points `vision_bundle_base_url` at another node's
/// `/models/manifest/`, the files come from that node instead — the way a node
/// without internet access gets them. Idempotent.
async fn install_models() -> anyhow::Result<()> {
    let engine = crate::services::manifest::registry()
        .by_id(ENGINE_ID)
        .ok_or_else(|| anyhow::anyhow!("'{ENGINE_ID}' is not in this build's catalog"))?;
    let base = crate::vision::camera_cv_models::resolve_bundle_base_url(None, engine);
    crate::vision::camera_cv_models::ensure_bundle(ENGINE_ID, &base, None, None).await
}

/// Loaded models; `None` (with a warning) when they cannot be loaded — the worker
/// keeps idling and retries after a while.
fn load_models() -> Option<Arc<Models>> {
    use crate::vision::camera_cv_models::{HAND_LANDMARK_FILE, MOVENET_FILE, PALM_DETECTION_FILE};
    let dir = crate::paths::vision_models_dir();
    let loaded = crate::vision::movenet::load(&dir.join(MOVENET_FILE)).and_then(|pose| {
        HandTracker::load(&dir.join(PALM_DETECTION_FILE), &dir.join(HAND_LANDMARK_FILE))
            .map(|hands| Models { pose, hands })
    });
    match loaded {
        Ok(m) => Some(Arc::new(m)),
        Err(e) => {
            warn!("[gesture] models unavailable: {e:#}");
            None
        }
    }
}

struct Analysis {
    body: Option<Body>,
    pose_score: f32,
    hands: Vec<Hand>,
    /// Where to look for the person in the next frame.
    next_pose_region: Option<Region>,
}

/// Side of the square image handed to MoveNet (it resizes to its own input).
const POSE_CROP: u32 = 256;

/// The whole frame as a square, letterboxed — the first look, before the person
/// is found. Squeezing a 16:9 frame into MoveNet's square input instead would
/// halve the horizontal resolution and lose a raised, moving arm first.
fn full_frame_region(w: u32, h: u32) -> Region {
    let side = w.max(h) as f32;
    Region {
        x: (w as f32 - side) / 2.0,
        y: (h as f32 - side) / 2.0,
        size: side,
    }
}

/// Next frame's pose crop: the person's keypoint box, squared and enlarged so a
/// raised or swinging arm stays inside (the MoveNet reference tracking crop).
fn track_region(body: &Body, w: u32, h: u32) -> Option<Region> {
    let pts: Vec<Pt> = body.iter().flatten().copied().collect();
    if pts.is_empty() {
        return None;
    }
    let (mut x1, mut y1, mut x2, mut y2) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &pts {
        x1 = x1.min(p.x);
        y1 = y1.min(p.y);
        x2 = x2.max(p.x);
        y2 = y2.max(p.y);
    }
    let side = ((x2 - x1).max(y2 - y1) * 1.7).clamp(0.25 * w.min(h) as f32, w.max(h) as f32);
    Some(Region {
        x: (x1 + x2) / 2.0 - side / 2.0,
        y: (y1 + y2) / 2.0 - side / 2.0,
        size: side,
    })
}

/// `region` of an RGB24 frame resampled to an `n`×`n` RGB24 image; black outside
/// the frame.
fn square_crop_rgb(rgb: &[u8], w: u32, h: u32, region: Region, n: u32) -> Vec<u8> {
    let mut out = vec![0u8; (n * n * 3) as usize];
    let scale = region.size / n as f32;
    for oy in 0..n {
        let sy = (region.y + (oy as f32 + 0.5) * scale) as i64;
        if sy < 0 || sy >= h as i64 {
            continue;
        }
        for ox in 0..n {
            let sx = (region.x + (ox as f32 + 0.5) * scale) as i64;
            if sx < 0 || sx >= w as i64 {
                continue;
            }
            let src = ((sy as u32 * w + sx as u32) * 3) as usize;
            let dst = ((oy * n + ox) * 3) as usize;
            out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
        }
    }
    out
}

/// The person in `region` of the frame (the previous frame's tracking crop),
/// keypoints in frame pixels.
fn estimate_pose(models: &Models, rgb: &[u8], w: u32, h: u32, region: Region) -> anyhow::Result<Option<PoseDetection>> {
    let crop = square_crop_rgb(rgb, w, h, region, POSE_CROP);
    let scale = region.size / POSE_CROP as f32;
    Ok(models
        .pose
        .estimate(&crop, POSE_CROP, POSE_CROP)?
        .into_iter()
        .next()
        .map(|mut d| {
            for k in &mut d.keypoints {
                k.x = region.x + k.x * scale;
                k.y = region.y + k.y * scale;
            }
            d
        }))
}

/// Runs `jobs` at once, one thread each — the models sit in separate session
/// pools, so a pose, two hand trackings or two palm searches overlap on the GPU
/// instead of queuing. Results keep the order of `jobs`.
fn run_parallel<T, F>(jobs: Vec<F>) -> Vec<T>
where
    T: Send,
    F: FnOnce() -> T + Send,
{
    std::thread::scope(|scope| {
        let handles: Vec<_> = jobs.into_iter().map(|job| scope.spawn(job)).collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
            .collect()
    })
}

/// One frame. The person (in the tracking crop from the previous frame) and the
/// hands of the previous frame, re-found from their landmarks, are read at the
/// same time; then the palm detector searches, in parallel, only the wrist
/// regions still missing a hand.
fn analyze(
    models: &Models,
    rgb: &[u8],
    w: u32,
    h: u32,
    pose_region: Option<Region>,
    previous_hands: &[Hand],
) -> anyhow::Result<Analysis> {
    let region = pose_region.unwrap_or_else(|| full_frame_region(w, h));
    let (det, tracked) = std::thread::scope(|scope| {
        let pose = scope.spawn(|| estimate_pose(models, rgb, w, h, region));
        let tracked = run_parallel(
            previous_hands
                .iter()
                .map(|previous| move || models.hands.track(rgb, w, h, previous))
                .collect(),
        );
        let pose = pose.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (pose, tracked)
    });
    let det = det?;
    let body = det.as_ref().and_then(body_from);
    let mut hands = Vec::with_capacity(tracked.len());
    for hand in tracked {
        hands.extend(hand?);
    }
    let mut hands = dedupe_hands(hands);
    let regions = body.as_ref().map(hand_regions).unwrap_or_default();
    let mut missing: Vec<Region> = regions
        .iter()
        .filter(|(region, expected)| {
            hands.iter().filter(|h| contains(region, h.landmarks[H_WRIST])).count() < *expected
        })
        .map(|(region, _)| *region)
        .collect();
    // No person and no hand: someone close to the camera shows hands without a
    // body the pose model can read — search the whole frame, where such hands
    // are large enough for the palm detector.
    if body.is_none() && hands.is_empty() {
        missing.push(full_frame_region(w, h));
    }
    let searched = missing.len();
    for found in run_parallel(
        missing
            .into_iter()
            .map(|region| move || models.hands.hands_in(rgb, w, h, region))
            .collect(),
    ) {
        hands.extend(found?);
    }
    if searched > 0 {
        hands = dedupe_hands(hands);
    }
    // One person has two hands; extra ones (most confident first) are strays that
    // would otherwise be tracked forever.
    hands.truncate(2);
    if tracing::enabled!(tracing::Level::DEBUG) {
        let score_of = |id: usize| {
            det.as_ref()
                .and_then(|d| d.keypoints.iter().find(|k| k.id as usize == id))
                .map_or(0.0, |k| k.score)
        };
        let found = det
            .as_ref()
            .map_or(0, |d| d.keypoints.iter().filter(|k| k.score >= MIN_KEYPOINT_SCORE).count());
        tracing::debug!(
            person = body.is_some(),
            keypoints = found,
            l_wrist = score_of(L_WRIST),
            r_wrist = score_of(R_WRIST),
            l_arm = ?body.as_ref().map(|b| arm(b, &hands, L_WRIST, L_ELBOW, L_SHOULDER)),
            r_arm = ?body.as_ref().map(|b| arm(b, &hands, R_WRIST, R_ELBOW, R_SHOULDER)),
            regions = regions.len(),
            palm_searches = searched,
            hands = hands.len(),
            "[gesture] frame"
        );
    }
    let next_pose_region = body.as_ref().and_then(|b| track_region(b, w, h));
    Ok(Analysis {
        body,
        pose_score: det.map(|d| d.score).unwrap_or(0.0),
        hands,
        next_pose_region,
    })
}

/// Per-camera recognition state, kept while the robot has gestures on.
#[derive(Default)]
struct CameraState {
    recognizer: Recognizer,
    last_reaction: HashMap<Gesture, u64>,
    last_any_reaction: u64,
    label: Option<(Gesture, u64)>,
    timed_frames: u32,
    /// Start of the current timing window: frames per second actually analyzed.
    timed_since: Option<std::time::Instant>,
    /// Time spent getting frames (snapshot, GPU download, NV12 → RGB).
    timed_fetch: Duration,
    timed_total: Duration,
    timed_max: Duration,
    /// Tracking crop for the pose model; `None` = search the whole frame.
    pose_region: Option<Region>,
    /// Hands of the previous frame, re-found from their landmarks on the next.
    hands: Vec<Hand>,
}

/// Frames per analysis-cost log line.
const TIMING_LOG_EVERY: u32 = 100;

impl CameraState {
    /// Log the cost and the rate now and then: what this camera's gestures cost
    /// the node, how many frames per second they really see, and how close the
    /// people in view come to a wave.
    fn note_timing(&mut self, camera_id: &str, fetch: Duration, took: Duration, hands: usize) {
        let since = *self.timed_since.get_or_insert_with(std::time::Instant::now);
        self.timed_frames += 1;
        self.timed_fetch += fetch;
        self.timed_total += took;
        self.timed_max = self.timed_max.max(took);
        if self.timed_frames == TIMING_LOG_EVERY {
            let n = f64::from(self.timed_frames);
            let (raised, swings) = self.recognizer.take_wave_stats();
            info!(
                "[gesture] {camera_id}: {:.1} fps, frame fetch mean={:.0}ms, analysis mean={:.0}ms max={:.0}ms over {} frames (hands now: {hands}; raised-hand frames: {raised}, most swings: {swings}/{WAVE_REVERSALS})",
                n / since.elapsed().as_secs_f64().max(1e-3),
                self.timed_fetch.as_secs_f64() * 1000.0 / n,
                self.timed_total.as_secs_f64() * 1000.0 / n,
                self.timed_max.as_secs_f64() * 1000.0,
                self.timed_frames
            );
            self.timed_frames = 0;
            self.timed_since = Some(std::time::Instant::now());
            self.timed_fetch = Duration::ZERO;
            self.timed_total = Duration::ZERO;
            self.timed_max = Duration::ZERO;
        }
    }

    fn may_react(&self, g: Gesture, now: u64) -> bool {
        now.saturating_sub(self.last_any_reaction) >= ANY_REACTION_GAP_MS
            && self
                .last_reaction
                .get(&g)
                .is_none_or(|t| now.saturating_sub(*t) >= GESTURE_COOLDOWN_MS)
    }
}

/// Start the gesture worker once per process. Cheap while no robot has gestures on.
pub fn spawn_worker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        tokio::spawn(worker());
    });
}

/// The fetcher waits this long before asking the camera again for a new frame.
const FRAME_POLL: Duration = Duration::from_millis(4);
/// Check for robots with gestures on (or retry the models) this often.
const IDLE_POLL: Duration = Duration::from_millis(500);

/// Starts and stops one analysis task per camera of a local robot with gestures
/// on, so cameras never wait for each other.
async fn worker() {
    let mut models: Option<Arc<Models>> = None;
    let mut models_retry_at = 0u64;
    let mut running: HashMap<(String, String), tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        let robots = crate::mesh::robot_dispatch::local_gesture_cameras();
        running.retain(|key, task| {
            let keep = robots.contains(key) && !task.is_finished();
            if !keep {
                task.abort();
            }
            keep
        });
        if !robots.is_empty() && models.is_none() && now_ms() >= models_retry_at {
            if let Err(e) = install_models().await {
                warn!("[gesture] model download failed: {e:#}");
            }
            models = tokio::task::spawn_blocking(load_models).await.ok().flatten();
            if models.is_none() {
                models_retry_at = now_ms() + 60_000;
            }
        }
        if let Some(m) = &models {
            for key in robots {
                if !running.contains_key(&key) {
                    let (robot_id, camera_id) = key.clone();
                    running.insert(key, tokio::spawn(camera_task(m.clone(), robot_id, camera_id)));
                }
            }
        }
        tokio::time::sleep(IDLE_POLL).await;
    }
}

/// A frame and the time it took to get it.
type Fetched = Option<(Arc<crate::addon::host_functions::camera::RgbFrame>, Duration)>;

/// One camera: getting the next frame (snapshot, GPU download, NV12 → RGB)
/// overlaps with analyzing the current one, so neither waits for the other.
async fn camera_task(models: Arc<Models>, robot_id: String, camera_id: String) {
    let (tx, rx) = tokio::sync::watch::channel::<Fetched>(None);
    tokio::join!(fetch_frames(&camera_id, tx), analyze_frames(models, &robot_id, &camera_id, rx));
}

/// Publishes every new frame of the camera; the analysis takes the newest one
/// and skips any it had no time for.
async fn fetch_frames(camera_id: &str, tx: tokio::sync::watch::Sender<Fetched>) {
    let mut last_ms = 0u64;
    loop {
        let started = std::time::Instant::now();
        match crate::addon::host_functions::camera::latest_rgb_frame_global(camera_id, last_ms).await {
            Some(frame) => {
                last_ms = frame.captured_ms;
                if tx.send(Some((Arc::new(frame), started.elapsed()))).is_err() {
                    return;
                }
            }
            None => tokio::time::sleep(FRAME_POLL).await,
        }
    }
}

async fn analyze_frames(
    models: Arc<Models>,
    robot_id: &str,
    camera_id: &str,
    mut rx: tokio::sync::watch::Receiver<Fetched>,
) {
    let mut s = CameraState::default();
    while rx.changed().await.is_ok() {
        let Some((frame, fetch)) = rx.borrow_and_update().clone() else {
            continue;
        };
        let (m, f, pose_region, previous_hands) =
            (models.clone(), frame.clone(), s.pose_region, s.hands.clone());
        let started = std::time::Instant::now();
        let analysis = match tokio::task::spawn_blocking(move || {
            analyze(&m, &f.rgb, f.width, f.height, pose_region, &previous_hands)
        })
        .await
        {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => {
                warn!("[gesture] analysis failed for {camera_id}: {e:#}");
                s.pose_region = None;
                s.hands.clear();
                continue;
            }
            Err(e) => {
                warn!("[gesture] analysis task dropped for {camera_id}: {e}");
                s.pose_region = None;
                s.hands.clear();
                continue;
            }
        };
        let took = started.elapsed();
        s.pose_region = analysis.next_pose_region;
        s.hands = analysis.hands.clone();
        s.note_timing(camera_id, fetch, took, analysis.hands.len());
        let now = now_ms();
        let gesture = s
            .recognizer
            .update(frame.captured_ms, analysis.body.as_ref(), &analysis.hands);
        let operator_busy =
            crate::mesh::robot_dispatch::manual_control_within(robot_id, MANUAL_CONTROL_QUIET);
        let reaction = gesture.filter(|g| !operator_busy && s.may_react(*g, now));
        if let Some(g) = gesture {
            s.label = Some((g, now));
        }
        if let Some(g) = reaction {
            s.last_reaction.insert(g, now);
            s.last_any_reaction = now;
        }
        let label = s
            .label
            .filter(|(_, at)| now.saturating_sub(*at) < LABEL_MS)
            .map(|(g, _)| g);
        publish_overlay(camera_id, &frame, took, &analysis, label);
        if let Some(g) = reaction {
            react(robot_id.to_string(), g);
        }
    }
}

/// Run the robot's answer to a gesture as a system action through its addon.
fn react(robot_id: String, gesture: Gesture) {
    let Some(ctx) = crate::mesh::robot_dispatch::dispatch_context() else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        let resp = crate::mesh::robot_control::execute_system_action(
            &ctx.addon_manager,
            &robot_id,
            &gesture.action(),
        );
        if resp.ok {
            info!("[gesture] {robot_id}: {:?} recognized, robot answered", gesture);
        } else {
            warn!(
                "[gesture] {robot_id}: {:?} recognized, robot refused: {}",
                gesture,
                resp.error.as_deref().unwrap_or("rejected")
            );
        }
    });
}

fn publish_overlay(
    camera_id: &str,
    frame: &crate::addon::host_functions::camera::RgbFrame,
    took: Duration,
    a: &Analysis,
    label: Option<Gesture>,
) {
    let (fw, fh) = (frame.width.max(1) as f32, frame.height.max(1) as f32);
    let norm = |p: Pt, score: f32| [p.x / fw, p.y / fh, score];
    let bbox = |pts: &mut dyn Iterator<Item = Pt>| {
        let (mut x1, mut y1, mut x2, mut y2) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in pts {
            x1 = x1.min(p.x);
            y1 = y1.min(p.y);
            x2 = x2.max(p.x);
            y2 = y2.max(p.y);
        }
        if x1 > x2 {
            return [0.0; 4];
        }
        [x1 / fw, y1 / fh, (x2 - x1) / fw, (y2 - y1) / fh]
    };
    let mut items = Vec::new();
    let mut person_box = [0.0f32; 4];
    if let Some(body) = &a.body {
        person_box = bbox(&mut body.iter().flatten().copied());
        items.push(PoseItem {
            klasa: "pose",
            bbox: person_box,
            score: a.pose_score,
            keypoints: body
                .iter()
                .map(|p| p.map_or([0.0, 0.0, 0.0], |p| norm(p, 1.0)))
                .collect(),
            label: None,
        });
    }
    for hand in &a.hands {
        items.push(PoseItem {
            klasa: "hand",
            bbox: bbox(&mut hand.landmarks.iter().copied()),
            score: hand.presence,
            keypoints: hand.landmarks.iter().map(|p| norm(*p, 1.0)).collect(),
            label: None,
        });
    }
    if let Some(g) = label {
        items.push(PoseItem {
            klasa: "gesture",
            bbox: person_box,
            score: 1.0,
            keypoints: Vec::new(),
            label: Some(g.id().to_string()),
        });
    }
    detection_bus::publish_pose(PoseMessage {
        camera_id: camera_id.to_string(),
        ts_ms: frame.captured_ms,
        pts_ns: frame.pts_ns,
        proc_ms: took.as_millis().min(u128::from(u32::MAX)) as u32,
        items,
    });
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(points: &[(usize, f32, f32)]) -> Body {
        let mut b = [None; 17];
        for &(i, x, y) in points {
            b[i] = Some(Pt { x, y });
        }
        b
    }

    /// Standing person, shoulders 100 px apart, right arm raised with the hand at
    /// `hand_dx` px from the elbow.
    fn raised_right(hand_dx: f32) -> Body {
        body(&[
            (NOSE, 200.0, 100.0),
            (L_SHOULDER, 250.0, 150.0),
            (R_SHOULDER, 150.0, 150.0),
            (R_ELBOW, 120.0, 140.0),
            (R_WRIST, 120.0 + hand_dx, 80.0),
            (L_ELBOW, 260.0, 220.0),
            (L_WRIST, 260.0, 290.0),
        ])
    }

    /// Offset (px) of a waving hand at time `t_ms`: 2.5 swings/s, ±60 px.
    fn wave_dx(t_ms: u64) -> f32 {
        60.0 * (2.0 * std::f32::consts::PI * 2.5 * t_ms as f32 / 1000.0).sin()
    }

    #[test]
    fn a_raised_hand_swinging_side_to_side_is_a_wave() {
        let mut r = Recognizer::default();
        let fired = (0..20u64)
            .map(|i| i * 100)
            .find_map(|t| r.update(t, Some(&raised_right(wave_dx(t))), &[]));
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn keypoint_jitter_of_a_still_raised_hand_is_not_a_wave() {
        let mut r = Recognizer::default();
        for i in 0..40u64 {
            // ±10 px of noise on a 100 px shoulder width, every frame.
            let dx = if i % 2 == 0 { 10.0 } else { -10.0 };
            assert_eq!(r.update(i * 100, Some(&raised_right(dx)), &[]), None);
        }
    }

    #[test]
    fn swinging_a_hand_below_the_shoulder_is_not_a_wave() {
        let mut r = Recognizer::default();
        for i in 0..20u64 {
            let t = i * 100;
            let mut b = raised_right(wave_dx(t));
            // Hand at the ear: above the elbow but below the shoulder line.
            b[R_WRIST] = Some(Pt { x: 120.0 + wave_dx(t), y: 160.0 });
            b[R_ELBOW] = Some(Pt { x: 120.0, y: 220.0 });
            assert_eq!(r.update(t, Some(&b), &[]), None);
        }
    }

    #[test]
    fn lowering_the_hand_between_swings_starts_over() {
        // Raises of one swing each (right, left, right), every one followed by
        // the hand down for longer than the gap: no single raise holds a wave.
        let mut r = Recognizer::default();
        let mut t = 0;
        for _ in 0..6 {
            for dx in [60.0, -60.0, 60.0] {
                assert_eq!(r.update(t, Some(&raised_right(dx)), &[]), None);
                t += 100;
            }
            let mut down = raised_right(0.0);
            down[R_WRIST] = Some(Pt { x: 120.0, y: 220.0 });
            for _ in 0..5 {
                assert_eq!(r.update(t, Some(&down), &[]), None);
                t += 100;
            }
        }
    }

    #[test]
    fn a_hand_misread_as_lowered_for_a_frame_mid_wave_does_not_restart_it() {
        let mut r = Recognizer::default();
        let fired = (0..60u64).map(|i| i * 33).find_map(|t| {
            let mut b = raised_right(wave_dx(t));
            if (t / 33) % 4 == 3 {
                b[R_WRIST] = Some(Pt { x: 120.0, y: 220.0 });
            }
            r.update(t, Some(&b), &[])
        });
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn a_wrist_lost_for_a_few_frames_mid_wave_does_not_restart_it() {
        let mut r = Recognizer::default();
        let fired = (0..60u64).map(|i| i * 33).find_map(|t| {
            let mut b = raised_right(wave_dx(t));
            // Every fourth frame the swing blurs the wrist away.
            if (t / 33) % 4 == 3 {
                b[R_WRIST] = None;
            }
            r.update(t, Some(&b), &[])
        });
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn a_wave_seen_at_a_low_frame_rate_is_still_a_wave() {
        // 5 frames per second against 2.5 swings per second.
        let mut r = Recognizer::default();
        let fired = (0..15u64)
            .map(|i| i * 200 + 50)
            .find_map(|t| r.update(t, Some(&raised_right(wave_dx(t))), &[]));
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn the_tracked_palm_drives_the_wave_when_the_pose_wrist_lags() {
        // The pose model's wrist stays put; the tracked hand swings.
        let mut r = Recognizer::default();
        let fired = (0..60u64).map(|i| i * 33).find_map(|t| {
            let b = raised_right(0.0);
            r.update(t, Some(&b), &[hand_at(120.0 + wave_dx(t), 80.0)])
        });
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn an_open_hand_swinging_without_a_body_in_view_is_a_wave() {
        let mut r = Recognizer::default();
        let fired = (0..60u64)
            .map(|i| i * 33)
            .find_map(|t| r.update(t, None, &[hand_at(300.0 + 2.0 * wave_dx(t), 200.0)]));
        assert_eq!(fired, Some(Gesture::Wave));
        // A fist moving the same way is not.
        let mut r = Recognizer::default();
        let mut fist = hand_at(300.0, 200.0);
        for tip in [H_INDEX_TIP, H_MIDDLE_TIP, H_RING_TIP, H_PINKY_TIP] {
            fist.landmarks[tip] = Pt { x: 300.0, y: 175.0 };
        }
        for i in 0..60u64 {
            let t = i * 33;
            let mut h = fist.clone();
            for p in h.landmarks.iter_mut() {
                p.x += 2.0 * wave_dx(t);
            }
            assert_eq!(r.update(t, None, &[h]), None);
        }
    }

    #[test]
    fn a_waving_hand_counts_even_when_the_pose_model_sees_the_arm_lowered() {
        // Elbow and wrist of the pose model below the shoulder (a low wave the
        // pose model misreads), the tracked hand open and swinging.
        let mut r = Recognizer::default();
        let fired = (0..60u64).map(|i| i * 33).find_map(|t| {
            let mut b = raised_right(0.0);
            b[R_WRIST] = Some(Pt { x: 120.0, y: 230.0 });
            b[R_ELBOW] = Some(Pt { x: 120.0, y: 300.0 });
            r.update(t, Some(&b), &[hand_at(400.0 + 2.0 * wave_dx(t), 200.0)])
        });
        assert_eq!(fired, Some(Gesture::Wave));
    }

    #[test]
    fn a_still_open_hand_whose_size_jitters_is_not_a_wave() {
        let mut r = Recognizer::default();
        for i in 0..90u64 {
            // Far right of the frame, the detected hand size breathing ±10 %.
            let s = if i % 2 == 0 { 1.1 } else { 0.9 };
            let mut h = hand_at(1200.0, 400.0);
            let w = h.landmarks[H_WRIST];
            for p in h.landmarks.iter_mut() {
                p.x = w.x + (p.x - w.x) * s;
                p.y = w.y + (p.y - w.y) * s;
            }
            assert_eq!(r.update(i * 33, None, &[h]), None);
        }
    }

    #[test]
    fn two_still_open_hands_listed_in_changing_order_are_not_a_wave() {
        let mut r = Recognizer::default();
        let (a, b) = (hand_at(200.0, 200.0), hand_at(600.0, 200.0));
        for i in 0..90u64 {
            let hands = if i % 2 == 0 { [a.clone(), b.clone()] } else { [b.clone(), a.clone()] };
            assert_eq!(r.update(i * 33, None, &hands), None);
        }
    }

    #[test]
    fn a_wrist_lost_for_long_ends_the_raise() {
        let mut series = ArmSeries::default();
        assert!(!series.push(0, Arm::Raised { x: 60.0, unit: 100.0 }));
        assert!(!series.push(100, Arm::Raised { x: -60.0, unit: 100.0 }));
        assert!(!series.push(200 + WAVE_GAP_MS, Arm::Lost));
        assert!(series.samples.is_empty());
    }

    #[test]
    fn a_tracked_hand_stands_in_for_a_wrist_the_pose_model_lost() {
        let mut b = raised_right(40.0);
        b[R_WRIST] = None;
        let hand = hand_at(170.0, 80.0);
        assert_eq!(arm(&b, &[], R_WRIST, R_ELBOW, R_SHOULDER), Arm::Lost);
        let Arm::Raised { x, unit } = arm(&b, &[hand], R_WRIST, R_ELBOW, R_SHOULDER) else {
            panic!("the hand next to the elbow is the wrist");
        };
        assert!((x / unit - 0.5).abs() < 1e-5);
        // A hand far from this elbow (the other arm's) is not used.
        assert_eq!(arm(&b, &[hand_at(400.0, 80.0)], R_WRIST, R_ELBOW, R_SHOULDER), Arm::Lost);
    }

    #[test]
    fn swings_spread_beyond_the_window_are_not_a_wave() {
        let mut r = Recognizer::default();
        for (i, dx) in [0.0, 60.0, -60.0, 60.0, -60.0].iter().enumerate() {
            assert_eq!(r.update(i as u64 * 1200, Some(&raised_right(*dx)), &[]), None);
        }
    }

    /// A hand with the wrist at (x, y), fingers pointing up, size 40 px.
    fn hand_at(x: f32, y: f32) -> Hand {
        let mut l = [Pt { x, y }; 21];
        l[H_WRIST] = Pt { x, y };
        l[H_MIDDLE_MCP] = Pt { x, y: y - 40.0 };
        l[H_RING_MCP] = Pt { x: x + 8.0, y: y - 38.0 };
        l[H_PINKY_MCP] = Pt { x: x + 16.0, y: y - 34.0 };
        l[H_INDEX_MCP] = Pt { x: x - 8.0, y: y - 38.0 };
        for (tip, mcp) in [(H_INDEX_TIP, H_INDEX_MCP), (H_MIDDLE_TIP, H_MIDDLE_MCP), (H_RING_TIP, H_RING_MCP), (H_PINKY_TIP, H_PINKY_MCP)] {
            l[tip] = Pt { x: l[mcp].x, y: l[mcp].y - 35.0 };
        }
        l[H_INDEX_PIP] = Pt { x: x - 8.0, y: y - 50.0 };
        l[H_INDEX_DIP] = Pt { x: x - 8.0, y: y - 62.0 };
        l[H_THUMB_MCP] = Pt { x: x - 20.0, y: y - 15.0 };
        l[H_THUMB_TIP] = Pt { x: x - 30.0, y: y - 25.0 };
        Hand { landmarks: l, presence: 0.95, motion: Pt { x: 0.0, y: 0.0 } }
    }

    /// Finger heart: index bent back over itself, thumb tip poking across it.
    fn finger_heart_hand() -> Hand {
        let mut h = hand_at(100.0, 200.0);
        for (tip, mcp) in [(H_MIDDLE_TIP, H_MIDDLE_MCP), (H_RING_TIP, H_RING_MCP), (H_PINKY_TIP, H_PINKY_MCP)] {
            h.landmarks[tip] = Pt { x: h.landmarks[mcp].x, y: h.landmarks[mcp].y + 15.0 };
        }
        h.landmarks[H_INDEX_MCP] = Pt { x: 92.0, y: 162.0 };
        h.landmarks[H_INDEX_PIP] = Pt { x: 90.0, y: 140.0 };
        h.landmarks[H_INDEX_DIP] = Pt { x: 97.0, y: 128.0 };
        h.landmarks[H_INDEX_TIP] = Pt { x: 104.0, y: 132.0 };
        h.landmarks[H_THUMB_MCP] = Pt { x: 80.0, y: 180.0 };
        h.landmarks[H_THUMB_TIP] = Pt { x: 106.0, y: 136.0 };
        h
    }

    #[test]
    fn crossed_thumb_and_bent_index_with_folded_fingers_is_a_finger_heart() {
        assert!(finger_heart(&finger_heart_hand()));
        // An open hand is not a heart.
        assert!(!finger_heart(&hand_at(100.0, 200.0)));
    }

    #[test]
    fn a_pinch_without_the_thumb_crossing_is_not_a_finger_heart() {
        let mut h = finger_heart_hand();
        // Thumb tip meets the index tip from its own side, as when holding a pen.
        h.landmarks[H_THUMB_TIP] = Pt { x: 101.0, y: 131.0 };
        assert!(!finger_heart(&h));
    }

    /// Two hands whose index tips meet at the top and thumbs at the bottom.
    fn two_hand_heart_pair() -> [Hand; 2] {
        let mut a = hand_at(80.0, 200.0);
        let mut b = hand_at(120.0, 200.0);
        a.landmarks[H_INDEX_TIP] = Pt { x: 98.0, y: 140.0 };
        b.landmarks[H_INDEX_TIP] = Pt { x: 102.0, y: 140.0 };
        a.landmarks[H_THUMB_TIP] = Pt { x: 99.0, y: 190.0 };
        b.landmarks[H_THUMB_TIP] = Pt { x: 101.0, y: 190.0 };
        [a, b]
    }

    #[test]
    fn two_hands_joined_into_a_heart_are_recognized_after_the_hold_time() {
        let hands = two_hand_heart_pair();
        assert!(two_hand_heart(&hands[0], &hands[1]));
        let mut r = Recognizer::default();
        assert_eq!(r.update(0, None, &hands), None);
        assert_eq!(r.update(300, None, &hands), None);
        assert_eq!(r.update(700, None, &hands), Some(Gesture::Heart));
        // Fires once; holding on starts a new hold.
        assert_eq!(r.update(800, None, &hands), None);
    }

    #[test]
    fn steepled_hands_without_an_opening_are_not_a_heart() {
        let [mut a, mut b] = two_hand_heart_pair();
        // Palms pressed together: the index knuckles touch too.
        a.landmarks[H_INDEX_MCP] = Pt { x: 98.0, y: 165.0 };
        b.landmarks[H_INDEX_MCP] = Pt { x: 102.0, y: 165.0 };
        assert!(!two_hand_heart(&a, &b));
    }

    #[test]
    fn one_hand_found_twice_is_one_hand_and_never_a_heart_with_itself() {
        let h = hand_at(100.0, 200.0);
        let hands = dedupe_hands(vec![h.clone(), h.clone()]);
        assert_eq!(hands.len(), 1);
        assert!(!heart_shape(None, &hands));
    }

    #[test]
    fn a_heart_pair_is_found_among_more_than_two_hands() {
        let [a, b] = two_hand_heart_pair();
        let stray = hand_at(400.0, 300.0);
        assert!(heart_shape(None, &[stray, a, b]));
    }

    #[test]
    fn a_broken_hold_restarts_the_heart_timer() {
        let hands = two_hand_heart_pair();
        let mut r = Recognizer::default();
        assert_eq!(r.update(0, None, &hands), None);
        assert_eq!(r.update(400, None, &[]), None);
        assert_eq!(r.update(500, None, &hands), None);
        assert_eq!(r.update(900, None, &hands), None);
        assert_eq!(r.update(1100, None, &hands), Some(Gesture::Heart));
    }

    fn arms_over_head() -> Body {
        body(&[
            (NOSE, 200.0, 100.0),
            (L_SHOULDER, 250.0, 150.0),
            (R_SHOULDER, 150.0, 150.0),
            (L_ELBOW, 290.0, 80.0),
            (R_ELBOW, 110.0, 80.0),
            (L_WRIST, 215.0, 40.0),
            (R_WRIST, 185.0, 40.0),
        ])
    }

    #[test]
    fn arms_over_the_head_with_wrists_together_are_a_heart() {
        assert!(arms_heart(&arms_over_head()));
        // Arms straight up and apart: not a heart.
        let mut apart = arms_over_head();
        apart[L_WRIST] = Some(Pt { x: 300.0, y: 40.0 });
        apart[R_WRIST] = Some(Pt { x: 100.0, y: 40.0 });
        assert!(!arms_heart(&apart));
    }

    #[test]
    fn hands_clasped_behind_the_head_are_not_a_heart() {
        let mut b = arms_over_head();
        // Wrists at the back of the head, elbows out at shoulder height.
        b[L_WRIST] = Some(Pt { x: 210.0, y: 75.0 });
        b[R_WRIST] = Some(Pt { x: 190.0, y: 75.0 });
        b[L_ELBOW] = Some(Pt { x: 300.0, y: 150.0 });
        b[R_ELBOW] = Some(Pt { x: 100.0, y: 150.0 });
        assert!(!arms_heart(&b));
    }

    #[test]
    fn wrists_close_together_share_one_hand_region() {
        let b = body(&[
            (L_ELBOW, 230.0, 260.0),
            (R_ELBOW, 170.0, 260.0),
            (L_WRIST, 210.0, 200.0),
            (R_WRIST, 190.0, 200.0),
        ]);
        assert_eq!(hand_regions(&b).len(), 1);
        let apart = body(&[
            (L_ELBOW, 330.0, 260.0),
            (R_ELBOW, 70.0, 260.0),
            (L_WRIST, 360.0, 200.0),
            (R_WRIST, 40.0, 200.0),
        ]);
        assert_eq!(hand_regions(&apart).len(), 2);
    }

    fn detection(found: usize, score: f32) -> PoseDetection {
        PoseDetection {
            bbox: (0.0, 0.0, 1.0, 1.0),
            score,
            keypoints: (0..found)
                .map(|i| crate::vision::PoseKeypoint {
                    id: i as u8,
                    name: "",
                    x: i as f32,
                    y: i as f32,
                    score,
                })
                .collect(),
        }
    }

    #[test]
    fn the_first_pose_look_letterboxes_the_whole_frame_without_squeezing() {
        let r = full_frame_region(1280, 720);
        assert_eq!((r.x, r.y, r.size), (0.0, -280.0, 1280.0));
        // A red pixel in the frame lands at the same relative spot of the crop;
        // the bars above and below are black.
        let (w, h) = (1280u32, 720u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        // A 20×20 red block (the crop is 5× smaller, so it becomes ~4×4).
        let (px, py) = (640u32, 360u32);
        for y in py..py + 20 {
            for x in px..px + 20 {
                rgb[((y * w + x) * 3) as usize] = 255;
            }
        }
        let crop = square_crop_rgb(&rgb, w, h, r, 256);
        let (cx, cy) = ((px as f32 / 5.0) as u32 + 2, ((py as f32 + 280.0) / 5.0) as u32 + 2);
        assert_eq!(crop[((cy * 256 + cx) * 3) as usize], 255, "the block lands near ({cx}, {cy})");
        assert!(crop[..256 * 3 * 40].iter().all(|v| *v == 0), "top bar is black");
    }

    #[test]
    fn the_tracking_crop_keeps_a_raised_arm_inside() {
        let b = raised_right(60.0);
        let r = track_region(&b, 1280, 720).unwrap();
        for p in b.iter().flatten() {
            assert!(p.x > r.x && p.x < r.x + r.size && p.y > r.y && p.y < r.y + r.size, "{p:?} in {r:?}");
        }
        // Never smaller than a quarter of the frame's short side.
        assert!(r.size >= 180.0);
    }

    #[test]
    fn a_weak_pose_guess_is_not_a_person() {
        // A coat on a hanger: a few confident points, the rest guessed.
        assert!(body_from(&detection(5, 0.9)).is_none());
        // Many points, all barely above the floor.
        assert!(body_from(&detection(14, 0.32)).is_none());
        // A real person.
        assert!(body_from(&detection(14, 0.7)).is_some());
    }

    #[test]
    fn reactions_respect_the_per_gesture_cooldown_and_the_global_gap() {
        let mut s = CameraState::default();
        assert!(s.may_react(Gesture::Wave, 10_000));
        s.last_reaction.insert(Gesture::Wave, 10_000);
        s.last_any_reaction = 10_000;
        assert!(!s.may_react(Gesture::Heart, 12_000), "global gap");
        assert!(s.may_react(Gesture::Heart, 14_000));
        assert!(!s.may_react(Gesture::Wave, 14_000), "per-gesture cooldown");
        assert!(s.may_react(Gesture::Wave, 18_000));
    }
}
