// =============================================================================
// File: occupancy.rs — the persistent occupancy model of a shared scene.
// Purpose: the geometry of a map that OUTLIVES the session that built it
// (docs/SHARED_MAP_PLAN.md §3). Replaces `voxel_map.rs`, whose semantics are
// "the set of cells last seen", i.e. a live view: it cannot say that a wall is
// still there when nobody is looking at it, nor that a box someone carried out
// is gone. Both are the questions a stored map exists to answer.
//
// Cells hold log-odds (OctoMap's model), so an observation is evidence rather
// than a fact, and a cell only becomes VISIBLE after repeated, time-spread
// evidence — that alone is what keeps a person walking through a room out of
// the map, with no detector involved (§4 layer 1).
//
// Removal is deliberately harder than addition: one bad depth ray must never
// delete a wall, so a cell is removed only after several free-space observations
// from viewpoints that are actually apart. The evidence for that lives in a side
// table keyed by the few cells currently losing confidence, not as a per-cell
// field — the whole grid is millions of cells and only a handful are ever in
// that state.
// =============================================================================

use std::collections::HashMap;
use std::collections::VecDeque;

/// Cells along one edge of a block (16³ = 4096 cells per block).
pub const BLOCK_EDGE: i32 = 16;
/// Blocks along one edge of a chunk (8³ blocks = 128³ cells per chunk).
pub const CHUNK_EDGE_BLOCKS: i32 = 8;
/// Cells along one edge of a chunk.
pub const CHUNK_EDGE: i32 = BLOCK_EDGE * CHUNK_EDGE_BLOCKS;
const BLOCK_CELLS: usize = (BLOCK_EDGE * BLOCK_EDGE * BLOCK_EDGE) as usize;
const BLOCKS_PER_CHUNK: usize =
    (CHUNK_EDGE_BLOCKS * CHUNK_EDGE_BLOCKS * CHUNK_EDGE_BLOCKS) as usize;

/// Log-odds are stored as `i8` at 1/32 of a unit, which covers ±3.97 — wider
/// than the clamp below, so the clamp is what bounds confidence, not the type.
const LOG_ODDS_SCALE: f32 = 32.0;

/// OctoMap's parameters. A hit is worth more than a miss because a surface that
/// answers is stronger evidence than a ray that did not stop.
const L_HIT: i8 = 27; // +0.85
const L_MISS: i8 = -13; // -0.40
const L_MIN: i8 = -64; // -2.00
const L_MAX: i8 = 112; // +3.50
/// A cell is occupied from `+0.85` up, free from `-0.40` down.
const OCCUPIED_THRESHOLD: i8 = L_HIT;
const FREE_THRESHOLD: i8 = L_MISS;

/// Hits closer together than this are one observation: a 30 Hz sensor standing
/// still would otherwise make anything "persistent" in a tenth of a second.
pub const HIT_SPACING_US: i64 = 200_000;
/// How long a cell must have been known before it may be shown.
pub const FIRST_SEEN_MIN_US: i64 = 1_000_000;
/// Hits required before a cell is visible.
pub const STABLE_HITS: u8 = 3;
/// Free-space observations required to remove a stable cell.
pub const REMOVAL_MISSES: u8 = 3;
/// How far two viewpoints must be apart to count as independent.
pub const REMOVAL_BASELINE_M: f32 = 0.3;

/// Cell flag bits, mirrored by the on-disk `.tfmc` payload.
pub mod flags {
    /// The cell passed the persistence gate and is part of the visible map.
    pub const STABLE: u8 = 0b0000_0001;
    /// Inside a dynamic-object volume when observed (§4) — fades faster.
    pub const DYNAMIC_SUSPECT: u8 = 0b0000_0010;
    /// Confidence fell below the free threshold; it leaves the map at the next
    /// compaction. Kept as a flag so the delta stream can report the removal
    /// before the bytes are rewritten.
    pub const REMOVED_PENDING: u8 = 0b0000_0100;
}

/// Integer cell coordinate in the scene frame (`floor(world / res)`).
pub type Cell = [i32; 3];

/// Chunk coordinate: the unit of persistence, replication and streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkKey(pub i32, pub i32, pub i32);

impl ChunkKey {
    /// `cx_cy_cz`, the spelling used for `map_chunks.chunk_key` and file names.
    pub fn to_key_string(self) -> String {
        format!("{}_{}_{}", self.0, self.1, self.2)
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('_');
        let x = parts.next()?.parse().ok()?;
        let y = parts.next()?.parse().ok()?;
        let z = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(ChunkKey(x, y, z))
    }
}

/// Where a cell sits: which chunk, and its index inside that chunk's 128³ grid.
/// The index is what the `.tfmc` payload and the delta stream carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId {
    pub chunk: ChunkKey,
    pub index: u32,
}

/// One frame's worth of change, per chunk: what became visible and what left.
/// This is exactly what the WAL appends and the `map:` stream publishes, so a
/// viewer and the disk see the same events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrameDelta {
    pub chunks: HashMap<ChunkKey, ChunkDelta>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChunkDelta {
    pub added_stable: Vec<u32>,
    pub removed: Vec<u32>,
}

impl FrameDelta {
    pub fn is_empty(&self) -> bool {
        self.chunks.values().all(|c| c.added_stable.is_empty() && c.removed.is_empty())
    }

    pub(crate) fn add_stable(&mut self, id: CellId) {
        self.chunks.entry(id.chunk).or_default().added_stable.push(id.index);
    }

    pub(crate) fn add_removed(&mut self, id: CellId) {
        self.chunks.entry(id.chunk).or_default().removed.push(id.index);
    }
}

/// The COMPLETE state of one cell — every per-cell field the grid holds.
///
/// Both on-disk artifacts carry exactly this: a `.tfmc` snapshot stores one per
/// materialized cell, and a WAL record stores one per cell the frame changed.
/// Neither reconstructs a field by inference, so a reload is the state that was
/// written rather than an approximation of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistedCell {
    /// Chunk-local index, the same number the delta stream carries.
    pub index: u32,
    pub log_odds: i8,
    pub hits: u8,
    pub last_seen_rev: u32,
    pub first_seen_delta: u16,
    pub flags: u8,
}

/// 4096 cells, structure-of-arrays so a whole field can be written to disk and
/// scanned without touching the others.
#[derive(Debug, Clone)]
struct Block {
    log_odds: Box<[i8; BLOCK_CELLS]>,
    hits: Box<[u8; BLOCK_CELLS]>,
    /// Revision of the most recent hit. Also the "have I already been hit this
    /// frame" marker, which is what keeps one frame from counting twice.
    last_seen_rev: Box<[u32; BLOCK_CELLS]>,
    /// Revisions since the cell was first observed, saturating. The persistence
    /// gate needs "about a second", not a history.
    first_seen_delta: Box<[u16; BLOCK_CELLS]>,
    flags: Box<[u8; BLOCK_CELLS]>,
}

impl Block {
    fn new() -> Self {
        Self {
            log_odds: Box::new([0; BLOCK_CELLS]),
            hits: Box::new([0; BLOCK_CELLS]),
            last_seen_rev: Box::new([0; BLOCK_CELLS]),
            first_seen_delta: Box::new([0; BLOCK_CELLS]),
            flags: Box::new([0; BLOCK_CELLS]),
        }
    }
}

/// A chunk of the map: 8³ blocks, allocated lazily. Free and unknown space is
/// never materialized — a room is mostly nothing, and storing that nothing is
/// what makes naive voxel maps unaffordable.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    blocks: HashMap<u16, Block>,
    /// Bumped on every change; the chunk index, the WAL and the stream all
    /// compare this number to decide what a peer is missing.
    pub revision: u64,
    pub dirty: bool,
}

impl Chunk {
    /// Number of cells that ever held a hit — the materialized count, which is
    /// what the on-disk size follows. A cell whose log-odds happen to have
    /// drifted back through zero still counts: it has a hit history, so it has
    /// a row, and the grid's own cap accounting uses the same test.
    pub fn materialized_cells(&self) -> usize {
        self.blocks
            .values()
            .map(|b| {
                (0..BLOCK_CELLS)
                    .filter(|&i| b.log_odds[i] != 0 || b.hits[i] != 0)
                    .count()
            })
            .sum()
    }

    /// Every materialized cell, ordered by index — exactly the rows a `.tfmc`
    /// snapshot holds. "Materialized" means "ever held a hit": free and unknown
    /// space has no row on disk (§3.1), which is what keeps a mostly-empty room
    /// from costing 128³ cells.
    pub fn persisted_cells(&self) -> Vec<PersistedCell> {
        let mut out = Vec::new();
        for (block_idx, block) in &self.blocks {
            for i in 0..BLOCK_CELLS {
                if block.log_odds[i] == 0 && block.hits[i] == 0 {
                    continue;
                }
                out.push(PersistedCell {
                    index: chunk_index(*block_idx, i as u16),
                    log_odds: block.log_odds[i],
                    hits: block.hits[i],
                    last_seen_rev: block.last_seen_rev[i],
                    first_seen_delta: block.first_seen_delta[i],
                    flags: block.flags[i],
                });
            }
        }
        out.sort_unstable_by_key(|c| c.index);
        out
    }

    /// The full state of the named cells, for one WAL record. An index with
    /// nothing materialized behind it is skipped rather than written as a zero
    /// row: the WAL describes cells that exist, and a stale index from a caller
    /// must not conjure one on replay.
    pub fn delta_cells(&self, indices: &[u32]) -> Vec<PersistedCell> {
        let mut out = Vec::with_capacity(indices.len());
        for &index in indices {
            let (block_idx, cell_idx) = split_index(index);
            let Some(block) = self.blocks.get(&block_idx) else {
                continue;
            };
            let i = cell_idx as usize;
            if block.log_odds[i] == 0 && block.hits[i] == 0 {
                continue;
            }
            out.push(PersistedCell {
                index,
                log_odds: block.log_odds[i],
                hits: block.hits[i],
                last_seen_rev: block.last_seen_rev[i],
                first_seen_delta: block.first_seen_delta[i],
                flags: block.flags[i],
            });
        }
        out
    }

    /// Cells currently part of the visible map.
    pub fn stable_cells(&self) -> Vec<u32> {
        let mut out = Vec::new();
        for (block_idx, block) in &self.blocks {
            for (cell_idx, flag) in block.flags.iter().enumerate() {
                if flag & flags::STABLE != 0 {
                    out.push(chunk_index(*block_idx, cell_idx as u16));
                }
            }
        }
        out.sort_unstable();
        out
    }
}

/// Evidence that a visible cell is disappearing. Held only for cells under
/// attack, so the cost is proportional to what is changing, not to the map.
#[derive(Debug, Clone, Copy)]
struct RemovalEvidence {
    misses: u8,
    /// Where the first disagreeing observation was made. A second observation
    /// from the same spot proves nothing a sensor glitch could not.
    first_origin: [f32; 3],
    baseline_met: bool,
}

/// The occupancy map of one scene.
#[derive(Debug, Clone)]
pub struct OccupancyGrid {
    res: f32,
    inv_res: f32,
    revision: u64,
    chunks: HashMap<ChunkKey, Chunk>,
    removal_evidence: HashMap<CellId, RemovalEvidence>,
    /// `(revision, time_us)` for recent frames, so the persistence gate can ask
    /// "how long ago" without a timestamp per cell. One second of frames is all
    /// it ever needs; older revisions are certainly older than the gate.
    stamps: VecDeque<(u64, i64)>,
    /// Cells hit in the frame being folded, so a frame counts once per cell.
    max_voxels: usize,
    materialized: usize,
    /// Cells refused because the scene is at its cap. Reported, never silent.
    pub dropped_at_cap: u64,
}

/// A frame's identity in the grid: which revision it is and when it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStamp {
    pub revision: u64,
    pub time_us: i64,
}

impl OccupancyGrid {
    pub fn new(res: f32, max_voxels: usize) -> Self {
        assert!(res > 0.0, "voxel resolution must be positive");
        Self {
            res,
            inv_res: 1.0 / res,
            revision: 0,
            chunks: HashMap::new(),
            removal_evidence: HashMap::new(),
            stamps: VecDeque::new(),
            max_voxels,
            materialized: 0,
            dropped_at_cap: 0,
        }
    }

    pub fn resolution(&self) -> f32 {
        self.res
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn materialized_cells(&self) -> usize {
        self.materialized
    }

    pub fn chunk(&self, key: ChunkKey) -> Option<&Chunk> {
        self.chunks.get(&key)
    }

    pub fn chunk_keys(&self) -> Vec<ChunkKey> {
        let mut keys: Vec<ChunkKey> = self.chunks.keys().copied().collect();
        keys.sort_unstable();
        keys
    }

    /// Chunks with changes not yet in a snapshot — the set a compaction must
    /// write before the WAL may be truncated.
    pub fn dirty_chunk_keys(&self) -> Vec<ChunkKey> {
        let mut keys: Vec<ChunkKey> =
            self.chunks.iter().filter(|(_, c)| c.dirty).map(|(k, _)| *k).collect();
        keys.sort_unstable();
        keys
    }

    /// Installs a chunk exactly as it was persisted, replacing whatever was
    /// there. Only `chunk_io` calls this, when opening a scene: it performs no
    /// evidence accounting because the stored bytes ARE the accounting, and the
    /// chunk is clean — its state is already on disk.
    pub fn restore_chunk(&mut self, key: ChunkKey, revision: u64, cells: &[PersistedCell]) {
        if let Some(old) = self.chunks.remove(&key) {
            self.materialized = self.materialized.saturating_sub(old.materialized_cells());
        }
        let mut chunk = Chunk {
            blocks: HashMap::new(),
            revision,
            dirty: false,
        };
        for cell in cells {
            let (block_idx, cell_idx) = split_index(cell.index);
            let block = chunk.blocks.entry(block_idx).or_insert_with(Block::new);
            let i = cell_idx as usize;
            block.log_odds[i] = cell.log_odds;
            block.hits[i] = cell.hits;
            block.last_seen_rev[i] = cell.last_seen_rev;
            block.first_seen_delta[i] = cell.first_seen_delta;
            block.flags[i] = cell.flags;
        }
        self.materialized += chunk.materialized_cells();
        self.revision = self.revision.max(revision);
        self.chunks.insert(key, chunk);
    }

    /// Replays one WAL record on top of the restored snapshots.
    ///
    /// The record holds each cell's complete state, so this assigns it and
    /// derives nothing. That is deliberate: inferring `last_seen_rev` from a
    /// rising `hits`/`log_odds` works until both saturate, and a cell at
    /// `hits == 255` with clamped log-odds is exactly the long-lived wall a map
    /// must reload correctly.
    pub fn apply_delta_cells(&mut self, key: ChunkKey, revision: u64, cells: &[PersistedCell]) {
        let chunk = self.chunks.entry(key).or_default();
        let mut gained = 0usize;
        let mut lost = 0usize;
        for cell in cells {
            let (block_idx, cell_idx) = split_index(cell.index);
            let block = chunk.blocks.entry(block_idx).or_insert_with(Block::new);
            let i = cell_idx as usize;
            let existed = block.log_odds[i] != 0 || block.hits[i] != 0;
            block.log_odds[i] = cell.log_odds;
            block.hits[i] = cell.hits;
            block.last_seen_rev[i] = cell.last_seen_rev;
            block.first_seen_delta[i] = cell.first_seen_delta;
            block.flags[i] = cell.flags;
            let exists = cell.log_odds != 0 || cell.hits != 0;
            match (existed, exists) {
                (false, true) => gained += 1,
                (true, false) => lost += 1,
                _ => {}
            }
        }
        chunk.revision = chunk.revision.max(revision);
        chunk.dirty = true;
        self.materialized = self.materialized + gained - lost;
        self.revision = self.revision.max(revision);
    }

    /// Opens a frame: the grid takes the revision from here, so hits and misses
    /// of one frame share it and a cell cannot be counted twice.
    pub fn begin_frame(&mut self, time_us: i64) -> FrameStamp {
        self.revision += 1;
        self.stamps.push_back((self.revision, time_us));
        // Keep a little more than the persistence window; anything older is
        // resolved by the saturating delta alone.
        while self.stamps.len() > 256 {
            self.stamps.pop_front();
        }
        FrameStamp {
            revision: self.revision,
            time_us,
        }
    }

    /// Cell that contains a scene-frame point.
    pub fn cell_of(&self, p: [f32; 3]) -> Cell {
        [
            (p[0] * self.inv_res).floor() as i32,
            (p[1] * self.inv_res).floor() as i32,
            (p[2] * self.inv_res).floor() as i32,
        ]
    }

    /// Centre of a cell in the scene frame.
    pub fn cell_center(&self, cell: Cell) -> [f32; 3] {
        [
            (cell[0] as f32 + 0.5) * self.res,
            (cell[1] as f32 + 0.5) * self.res,
            (cell[2] as f32 + 0.5) * self.res,
        ]
    }

    pub fn cell_id(&self, cell: Cell) -> CellId {
        let chunk = ChunkKey(
            div_floor(cell[0], CHUNK_EDGE),
            div_floor(cell[1], CHUNK_EDGE),
            div_floor(cell[2], CHUNK_EDGE),
        );
        let local = [
            cell[0] - chunk.0 * CHUNK_EDGE,
            cell[1] - chunk.1 * CHUNK_EDGE,
            cell[2] - chunk.2 * CHUNK_EDGE,
        ];
        let index = (local[2] * CHUNK_EDGE * CHUNK_EDGE + local[1] * CHUNK_EDGE + local[0]) as u32;
        CellId { chunk, index }
    }

    /// True when the cell is part of the visible map.
    pub fn is_stable(&self, cell: Cell) -> bool {
        let id = self.cell_id(cell);
        self.flag_of(id).is_some_and(|f| f & flags::STABLE != 0)
    }

    /// Log-odds of a cell, in units (not the stored i8), for tests and tools.
    pub fn log_odds(&self, cell: Cell) -> f32 {
        let id = self.cell_id(cell);
        let (block_idx, cell_idx) = split_index(id.index);
        self.chunks
            .get(&id.chunk)
            .and_then(|c| c.blocks.get(&block_idx))
            .map(|b| b.log_odds[cell_idx as usize] as f32 / LOG_ODDS_SCALE)
            .unwrap_or(0.0)
    }

    /// Folds one observed point in. Returns the cell id when it BECAME visible
    /// with this hit, which is what the delta reports.
    pub fn hit(&mut self, stamp: FrameStamp, cell: Cell, dynamic_suspect: bool) -> Option<CellId> {
        let id = self.cell_id(cell);
        let (block_idx, cell_idx) = split_index(id.index);
        let at_cap = self.materialized >= self.max_voxels;
        let entry_is_new = !self.cell_exists(id);
        if entry_is_new && at_cap {
            // Refusing beats evicting: a scene at its cap that silently dropped
            // the oldest geometry would quietly forget the room it started in.
            self.dropped_at_cap += 1;
            return None;
        }
        let res_time = self.time_of(stamp.revision);
        // Disjoint borrows: the cell arrays and the revision→time window are
        // different fields, and the persistence gate needs both at once.
        let Self { chunks, stamps, .. } = self;
        let chunk = chunks.entry(id.chunk).or_default();
        let block = chunk.blocks.entry(block_idx).or_insert_with(Block::new);
        let i = cell_idx as usize;

        let was_new = block.log_odds[i] == 0 && block.hits[i] == 0;
        if was_new {
            block.first_seen_delta[i] = 0;
        } else {
            let seen = stamp
                .revision
                .saturating_sub(u64::from(block.last_seen_rev[i]));
            let grown = u64::from(block.first_seen_delta[i]).saturating_add(seen);
            block.first_seen_delta[i] = grown.min(u64::from(u16::MAX)) as u16;
        }

        let same_frame = block.last_seen_rev[i] as u64 == stamp.revision;
        if !same_frame {
            block.log_odds[i] = clamp_log_odds(block.log_odds[i].saturating_add(L_HIT));
            let last_time = stamps_time_of(stamps, block.last_seen_rev[i]);
            let spaced = match last_time {
                Some(prev) => res_time.saturating_sub(prev) >= HIT_SPACING_US,
                // Older than the stamp window: certainly more than 200 ms ago.
                None => block.hits[i] > 0 || was_new,
            };
            if spaced {
                block.hits[i] = block.hits[i].saturating_add(1);
            }
            block.last_seen_rev[i] = stamp.revision as u32;
        }
        if dynamic_suspect {
            block.flags[i] |= flags::DYNAMIC_SUSPECT;
        }
        // A cell that is being seen again is no longer leaving.
        block.flags[i] &= !flags::REMOVED_PENDING;

        if entry_is_new {
            self.materialized += 1;
        }
        chunk.dirty = true;
        chunk.revision = stamp.revision;

        let became_stable = block.flags[i] & flags::STABLE == 0
            && block.log_odds[i] >= OCCUPIED_THRESHOLD
            && block.hits[i] >= STABLE_HITS
            && first_seen_old_enough(stamps, block.first_seen_delta[i], stamp, res_time);
        if became_stable {
            block.flags[i] |= flags::STABLE;
        }
        self.removal_evidence.remove(&id);
        became_stable.then_some(id)
    }

    /// Folds one free-space observation in. `origin` is where the sensor was;
    /// `weight` scales the miss (a monocular depth ray is worth half a lidar
    /// return, a dynamic-suspect cell twice as much). Returns the cell id when
    /// the cell LEFT the visible map.
    pub fn miss(
        &mut self,
        stamp: FrameStamp,
        cell: Cell,
        origin: [f32; 3],
        weight: f32,
    ) -> Option<CellId> {
        let id = self.cell_id(cell);
        if !self.cell_exists(id) {
            // Free space is not materialized: a miss on a cell nobody ever saw
            // occupied has nothing to say.
            return None;
        }
        let (block_idx, cell_idx) = split_index(id.index);
        let i = cell_idx as usize;
        let chunk = self.chunks.get_mut(&id.chunk)?;
        let block = chunk.blocks.get_mut(&block_idx)?;
        if block.last_seen_rev[i] as u64 == stamp.revision {
            // The same frame that saw a surface here cannot also report the
            // space as free — that is the ray passing its own endpoint.
            return None;
        }
        let was_stable = block.flags[i] & flags::STABLE != 0;
        let dynamic = block.flags[i] & flags::DYNAMIC_SUSPECT != 0;
        let scaled = (f32::from(L_MISS) * weight * if dynamic { 2.0 } else { 1.0 })
            .clamp(f32::from(i8::MIN), f32::from(i8::MAX)) as i8;
        block.log_odds[i] = clamp_log_odds(block.log_odds[i].saturating_add(scaled));
        chunk.dirty = true;
        chunk.revision = stamp.revision;

        if !was_stable {
            // Nothing visible to take away; the confidence drop is enough.
            return None;
        }
        let evidence = self
            .removal_evidence
            .entry(id)
            .or_insert(RemovalEvidence {
                misses: 0,
                first_origin: origin,
                baseline_met: false,
            });
        evidence.misses = evidence.misses.saturating_add(1);
        if !evidence.baseline_met && distance(evidence.first_origin, origin) >= REMOVAL_BASELINE_M {
            evidence.baseline_met = true;
        }
        let enough = evidence.misses >= REMOVAL_MISSES && evidence.baseline_met;
        if !enough || block.log_odds[i] > FREE_THRESHOLD {
            return None;
        }
        block.flags[i] &= !flags::STABLE;
        block.flags[i] |= flags::REMOVED_PENDING;
        self.removal_evidence.remove(&id);
        Some(id)
    }

    /// Drops every `removed-pending` cell for good and clears the dirty marks.
    /// Called by the compaction that writes a fresh chunk snapshot: until then
    /// the cell stays so the delta stream can still name it.
    pub fn compact(&mut self) -> usize {
        let mut dropped = 0;
        for chunk in self.chunks.values_mut() {
            for block in chunk.blocks.values_mut() {
                for i in 0..BLOCK_CELLS {
                    if block.flags[i] & flags::REMOVED_PENDING != 0 {
                        block.log_odds[i] = 0;
                        block.hits[i] = 0;
                        block.last_seen_rev[i] = 0;
                        block.first_seen_delta[i] = 0;
                        block.flags[i] = 0;
                        dropped += 1;
                    }
                }
            }
            // Same "ever held a hit" test as `cell_exists`: dropping a block
            // that still has hit counts would leak the cap accounting.
            chunk
                .blocks
                .retain(|_, b| (0..BLOCK_CELLS).any(|i| b.log_odds[i] != 0 || b.hits[i] != 0));
            chunk.dirty = false;
        }
        self.chunks.retain(|_, c| !c.blocks.is_empty());
        self.materialized = self.materialized.saturating_sub(dropped);
        dropped
    }

    fn cell_exists(&self, id: CellId) -> bool {
        let (block_idx, cell_idx) = split_index(id.index);
        self.chunks
            .get(&id.chunk)
            .and_then(|c| c.blocks.get(&block_idx))
            .is_some_and(|b| {
                b.log_odds[cell_idx as usize] != 0 || b.hits[cell_idx as usize] != 0
            })
    }

    fn flag_of(&self, id: CellId) -> Option<u8> {
        let (block_idx, cell_idx) = split_index(id.index);
        self.chunks
            .get(&id.chunk)
            .and_then(|c| c.blocks.get(&block_idx))
            .map(|b| b.flags[cell_idx as usize])
    }

    fn time_of(&self, revision: u64) -> i64 {
        self.stamps
            .iter()
            .rev()
            .find(|(r, _)| *r == revision)
            .map(|(_, t)| *t)
            .unwrap_or(0)
    }

}

fn stamps_time_of(stamps: &VecDeque<(u64, i64)>, revision: u32) -> Option<i64> {
    if revision == 0 {
        return None;
    }
    stamps
        .iter()
        .find(|(r, _)| *r == u64::from(revision))
        .map(|(_, t)| *t)
}

/// "Known for at least a second". Inside the stamp window the answer is
/// measured; beyond it the cell is older than the whole window, which is longer
/// than the gate.
fn first_seen_old_enough(
    stamps: &VecDeque<(u64, i64)>,
    delta: u16,
    stamp: FrameStamp,
    now_us: i64,
) -> bool {
    let first_rev = stamp.revision.saturating_sub(u64::from(delta));
    match stamps.iter().find(|(r, _)| *r == first_rev) {
        Some((_, first_time)) => now_us.saturating_sub(*first_time) >= FIRST_SEEN_MIN_US,
        None => true,
    }
}

fn clamp_log_odds(v: i8) -> i8 {
    v.clamp(L_MIN, L_MAX)
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let dx = a[0] - b[0];
    let dy = a[1] - b[1];
    let dz = a[2] - b[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Euclidean division that rounds toward negative infinity, so a scene spanning
/// the origin does not get two chunk 0s.
fn div_floor(a: i32, b: i32) -> i32 {
    let q = a / b;
    if a % b != 0 && (a < 0) != (b < 0) {
        q - 1
    } else {
        q
    }
}

/// Split a chunk-local cell index into (block index, cell-in-block index).
fn split_index(index: u32) -> (u16, u16) {
    let x = index % CHUNK_EDGE as u32;
    let y = (index / CHUNK_EDGE as u32) % CHUNK_EDGE as u32;
    let z = index / (CHUNK_EDGE as u32 * CHUNK_EDGE as u32);
    let (bx, by, bz) = (
        x / BLOCK_EDGE as u32,
        y / BLOCK_EDGE as u32,
        z / BLOCK_EDGE as u32,
    );
    let block =
        bz * (CHUNK_EDGE_BLOCKS * CHUNK_EDGE_BLOCKS) as u32 + by * CHUNK_EDGE_BLOCKS as u32 + bx;
    let (lx, ly, lz) = (
        x % BLOCK_EDGE as u32,
        y % BLOCK_EDGE as u32,
        z % BLOCK_EDGE as u32,
    );
    let cell = lz * (BLOCK_EDGE * BLOCK_EDGE) as u32 + ly * BLOCK_EDGE as u32 + lx;
    debug_assert!((block as usize) < BLOCKS_PER_CHUNK);
    (block as u16, cell as u16)
}

/// Inverse of `split_index`.
fn chunk_index(block: u16, cell: u16) -> u32 {
    let (bx, by, bz) = (
        u32::from(block) % CHUNK_EDGE_BLOCKS as u32,
        (u32::from(block) / CHUNK_EDGE_BLOCKS as u32) % CHUNK_EDGE_BLOCKS as u32,
        u32::from(block) / (CHUNK_EDGE_BLOCKS * CHUNK_EDGE_BLOCKS) as u32,
    );
    let (lx, ly, lz) = (
        u32::from(cell) % BLOCK_EDGE as u32,
        (u32::from(cell) / BLOCK_EDGE as u32) % BLOCK_EDGE as u32,
        u32::from(cell) / (BLOCK_EDGE * BLOCK_EDGE) as u32,
    );
    let (x, y, z) = (
        bx * BLOCK_EDGE as u32 + lx,
        by * BLOCK_EDGE as u32 + ly,
        bz * BLOCK_EDGE as u32 + lz,
    );
    z * (CHUNK_EDGE * CHUNK_EDGE) as u32 + y * CHUNK_EDGE as u32 + x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> OccupancyGrid {
        OccupancyGrid::new(0.05, 1_000_000)
    }

    /// Three hits a second apart make a surface; the same three inside one
    /// frame, or inside a tenth of a second, do not. This is the whole reason a
    /// person walking through a room leaves nothing behind.
    #[test]
    fn a_cell_needs_three_spaced_hits_over_a_second() {
        let mut g = grid();
        let cell = [10, 4, 2];

        let s0 = g.begin_frame(0);
        assert!(g.hit(s0, cell, false).is_none());
        assert!(!g.is_stable(cell), "one hit is never a surface");

        // Same frame again: not a second observation.
        assert!(g.hit(s0, cell, false).is_none());

        let s1 = g.begin_frame(50_000); // 50 ms later — too close to count
        assert!(g.hit(s1, cell, false).is_none());
        assert!(!g.is_stable(cell));

        let s2 = g.begin_frame(600_000);
        assert!(g.hit(s2, cell, false).is_none());
        let s3 = g.begin_frame(1_200_000);
        let became = g.hit(s3, cell, false);
        assert_eq!(became, Some(g.cell_id(cell)));
        assert!(g.is_stable(cell));
    }

    /// A person crossing the view at walking pace: two hits, seconds of nothing.
    #[test]
    fn a_passer_by_never_becomes_part_of_the_map() {
        let mut g = grid();
        let mut time = 0;
        for step in 0..8 {
            let stamp = g.begin_frame(time);
            // One cell per step — the body is somewhere else each frame.
            g.hit(stamp, [step, 0, 0], false);
            time += 250_000;
        }
        for step in 0..8 {
            assert!(!g.is_stable([step, 0, 0]), "cell {step} must stay invisible");
        }
    }

    /// One stray ray must never delete a wall: three misses from ONE spot leave
    /// it standing, and the third from a different spot takes it down.
    #[test]
    fn removing_a_wall_needs_misses_from_two_places() {
        let mut g = grid();
        let cell = [3, 3, 3];
        let mut time = 0;
        for _ in 0..4 {
            let s = g.begin_frame(time);
            g.hit(s, cell, false);
            time += 500_000;
        }
        assert!(g.is_stable(cell));

        // Enough misses to drive the confidence below the free threshold — but
        // all from ONE spot, which is one witness however often it repeats.
        let origin = [0.0, 0.0, 0.0];
        for _ in 0..12 {
            let s = g.begin_frame(time);
            assert_eq!(g.miss(s, cell, origin, 1.0), None);
            time += 100_000;
        }
        assert!(
            g.log_odds(cell) < 0.0,
            "the fixture must really have pushed the cell below the free threshold"
        );
        assert!(
            g.is_stable(cell),
            "one viewpoint is one witness, however often it repeats"
        );

        let elsewhere = [1.0, 0.0, 0.0];
        let s = g.begin_frame(time);
        let removed = g.miss(s, cell, elsewhere, 1.0);
        assert_eq!(removed, Some(g.cell_id(cell)));
        assert!(!g.is_stable(cell));
    }

    /// The frame that saw the surface cannot also carve it.
    #[test]
    fn a_hit_and_a_miss_in_one_frame_do_not_fight() {
        let mut g = grid();
        let cell = [1, 1, 1];
        let mut time = 0;
        for _ in 0..4 {
            let s = g.begin_frame(time);
            g.hit(s, cell, false);
            time += 500_000;
        }
        let s = g.begin_frame(time);
        g.hit(s, cell, false);
        let before = g.log_odds(cell);
        assert_eq!(g.miss(s, cell, [5.0, 5.0, 5.0], 1.0), None);
        assert_eq!(g.log_odds(cell), before, "the ray stopped at this cell");
    }

    #[test]
    fn log_odds_stay_inside_the_clamp() {
        let mut g = grid();
        let cell = [0, 0, 0];
        let mut time = 0;
        for _ in 0..200 {
            let s = g.begin_frame(time);
            g.hit(s, cell, false);
            time += 300_000;
        }
        assert!((g.log_odds(cell) - f32::from(L_MAX) / LOG_ODDS_SCALE).abs() < 1e-6);
        for _ in 0..500 {
            let s = g.begin_frame(time);
            g.miss(s, cell, [0.0, 0.0, 0.0], 1.0);
            time += 300_000;
        }
        assert!((g.log_odds(cell) - f32::from(L_MIN) / LOG_ODDS_SCALE).abs() < 1e-6);
    }

    /// Hits saturate instead of wrapping: a cell hit 300 times must not read as
    /// hit 44 times.
    #[test]
    fn hits_and_first_seen_saturate() {
        let mut g = grid();
        let cell = [7, 7, 7];
        let mut time = 0;
        for _ in 0..300 {
            let s = g.begin_frame(time);
            g.hit(s, cell, false);
            time += 250_000;
        }
        let id = g.cell_id(cell);
        let (b, c) = split_index(id.index);
        let block = g.chunks[&id.chunk].blocks.get(&b).unwrap();
        assert_eq!(block.hits[c as usize], u8::MAX);
        assert!(block.first_seen_delta[c as usize] > 0);
    }

    /// Chunk and cell indexing must round-trip, including across the origin
    /// where a naive integer division would fold -1 and 0 into one chunk.
    #[test]
    fn cell_ids_round_trip_across_the_origin() {
        let g = grid();
        for cell in [
            [0, 0, 0],
            [127, 127, 127],
            [128, 0, 0],
            [-1, -1, -1],
            [-128, -129, 5],
            [1_000, -2_000, 3_000],
        ] {
            let id = g.cell_id(cell);
            let (b, c) = split_index(id.index);
            assert_eq!(chunk_index(b, c), id.index, "index round-trip for {cell:?}");
            let local = [
                cell[0] - id.chunk.0 * CHUNK_EDGE,
                cell[1] - id.chunk.1 * CHUNK_EDGE,
                cell[2] - id.chunk.2 * CHUNK_EDGE,
            ];
            assert!(
                local.iter().all(|v| (0..CHUNK_EDGE).contains(v)),
                "{cell:?} landed outside its chunk: {local:?}"
            );
        }
        assert_ne!(g.cell_id([-1, 0, 0]).chunk, g.cell_id([0, 0, 0]).chunk);
    }

    #[test]
    fn chunk_keys_round_trip_through_their_string() {
        for key in [ChunkKey(0, 0, 0), ChunkKey(-3, 12, -400)] {
            assert_eq!(ChunkKey::parse(&key.to_key_string()), Some(key));
        }
        assert_eq!(ChunkKey::parse("1_2"), None);
        assert_eq!(ChunkKey::parse("1_2_3_4"), None);
    }

    /// Compaction is what actually frees a removed cell, and until it runs the
    /// cell stays addressable so the stream can report its removal.
    #[test]
    fn compaction_frees_removed_cells_and_keeps_the_rest() {
        let mut g = grid();
        let gone = [2, 0, 0];
        let kept = [3, 0, 0];
        let mut time = 0;
        for _ in 0..4 {
            let s = g.begin_frame(time);
            g.hit(s, gone, false);
            g.hit(s, kept, false);
            time += 500_000;
        }
        assert_eq!(g.materialized_cells(), 2);

        // Twelve free-space observations, walking sideways between them: both
        // the confidence and the two-viewpoint gate have to be satisfied.
        let mut origin = [0.0, 0.0, 0.0];
        for _ in 0..12 {
            let s = g.begin_frame(time);
            g.miss(s, gone, origin, 1.0);
            origin[0] += 0.5;
            time += 200_000;
        }
        assert!(!g.is_stable(gone));
        assert_eq!(g.materialized_cells(), 2, "still addressable before compaction");

        assert_eq!(g.compact(), 1);
        assert_eq!(g.materialized_cells(), 1);
        assert!(g.is_stable(kept));
        assert!(!g.is_stable(gone));
    }

    /// A scene at its cap refuses new geometry and says how much it refused —
    /// it never evicts, because forgetting the room you started in is worse
    /// than not growing.
    #[test]
    fn a_full_scene_refuses_instead_of_forgetting() {
        let mut g = OccupancyGrid::new(0.05, 2);
        let s = g.begin_frame(0);
        g.hit(s, [0, 0, 0], false);
        g.hit(s, [1, 0, 0], false);
        assert_eq!(g.hit(s, [2, 0, 0], false), None);
        assert_eq!(g.dropped_at_cap, 1);
        assert_eq!(g.materialized_cells(), 2);
        // An existing cell is still updatable at the cap.
        let s2 = g.begin_frame(500_000);
        g.hit(s2, [0, 0, 0], false);
        assert_eq!(g.dropped_at_cap, 1);
    }

    /// A cell inside a detected person's volume fades twice as fast, so the
    /// map recovers quickly from what a detector already told us is not scenery.
    #[test]
    fn a_dynamic_suspect_cell_fades_faster() {
        let mut g = grid();
        let plain = [20, 0, 0];
        let suspect = [21, 0, 0];
        let mut time = 0;
        for _ in 0..4 {
            let s = g.begin_frame(time);
            g.hit(s, plain, false);
            g.hit(s, suspect, true);
            time += 500_000;
        }
        let s = g.begin_frame(time);
        g.miss(s, plain, [0.0, 0.0, 0.0], 1.0);
        g.miss(s, suspect, [0.0, 0.0, 0.0], 1.0);
        assert!(
            g.log_odds(suspect) < g.log_odds(plain),
            "the suspect cell must lose confidence faster"
        );
    }
}
