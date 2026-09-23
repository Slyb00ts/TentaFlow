// =============================================================================
// File: chunk_io.rs — on-disk persistence for the shared map's occupancy grid.
// Purpose: the geometry of a scene lives in plain files, not in SQLite, Fjall
// or the blob store (docs/SHARED_MAP_PLAN.md §1.2). A map is millions of cells
// rewritten every few seconds; pushing that through a transactional store or a
// content-addressed ledger turns a 30 s compaction into unbounded op growth.
//
// Two artifacts carry the state between runs, each with its own magic and
// version, because they change independently:
//   * `chunks/<cx>_<cy>_<cz>.<rev>.tfmc` — an immutable snapshot of one chunk.
//   * `wal.log` — every change since those snapshots, appended and replayed.
// Opening a scene is "read the snapshots, replay the log"; compaction is "write
// fresh snapshots, then throw the log away". Nothing else reconstructs a map.
//
// Both artifacts store each cell's COMPLETE state. Nothing here reconstructs a
// field from the shape of another one: a snapshot or record says what the cell
// was, and replay assigns it.
//
// `scene_id`, `voxel_res` and `owner_epoch` come from the scene's `map_scenes`
// row and are passed in by the caller — this store validates every file against
// them and quarantines a mismatch, but it is not their source of truth.
//
// Each reader accepts exactly one format version. Multi-version readers are
// forbidden by CLAUDE.md and by the plan: a version bump is a rewrite of every
// chunk by the scene owner, not a compatibility branch in this file.
// =============================================================================

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::occupancy::{Chunk, ChunkKey, OccupancyGrid, PersistedCell};

/// File magic. Four bytes so a truncated or unrelated file is rejected before
/// any length is trusted.
pub const TFMC_MAGIC: [u8; 4] = *b"TFMC";

/// The only `.tfmc` version this build reads or writes.
pub const TFMC_VERSION: u16 = 1;

/// magic(4) + version(2) + scene_id(8) + chunk_key(12) + voxel_res(4)
/// + revision(8) + owner_epoch(4) + cell_count(4) + payload_len(4).
const TFMC_HEADER_LEN: usize = 50;
/// Uncompressed bytes per cell in the SoA payload: idx(4) + log_odds(1)
/// + hits(1) + last_seen_rev(4) + first_seen_delta(2) + flags(1).
const TFMC_CELL_BYTES: usize = 13;
/// A chunk is 128³ cells, so no honest snapshot can name more than that. The
/// bound is what stops a corrupt `cell_count` from asking for a 50 GiB buffer.
const MAX_CELLS_PER_CHUNK: u32 = 2_097_152;
/// Fast enough to stay off the ingest path, small enough to matter on disk.
const ZSTD_LEVEL: i32 = 3;

/// `wal.log`'s own magic and version. The log is versioned separately from the
/// snapshots because the two formats move independently, and because a log left
/// over from an earlier format must be REFUSED rather than reframed: its record
/// boundaries would land mid-cell and every following record would parse into
/// plausible nonsense.
pub const WAL_MAGIC: [u8; 4] = *b"TFMW";
/// The only `wal.log` version this build reads or writes.
pub const WAL_VERSION: u16 = 1;
/// magic(4) + version(2), written once when the log is created.
const WAL_PREFIX_LEN: usize = 6;

/// Per record: revision(8) + chunk_key(12) + n(2).
const WAL_RECORD_HEADER_LEN: usize = 22;
/// Per cell: idx(4) + log_odds(1) + hits(1) + last_seen_rev(4)
/// + first_seen_delta(2) + flags(1) — the same complete state a snapshot holds.
const WAL_CELL_BYTES: usize = 13;

const SNAPSHOT_EXT: &str = "tfmc";
const CHUNKS_DIR: &str = "chunks";
const CORRUPT_DIR: &str = "corrupt";
const WAL_FILE: &str = "wal.log";

#[derive(Debug, thiserror::Error)]
pub enum ChunkIoError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("not a .tfmc file: magic {0:02x?}")]
    BadMagic([u8; 4]),
    /// The plan's version policy: the owner rewrites every chunk on a bump, so
    /// an old file here means the rewrite did not finish, not that we should
    /// try to read it.
    #[error("unsupported .tfmc version {found} (this build reads only {expected})")]
    UnsupportedVersion { found: u16, expected: u16 },
    /// Unlike a snapshot, a bad log is not quarantined: it is scene-wide, and
    /// setting it aside would silently discard every frame since the last
    /// compaction. The caller is told instead.
    #[error("wal.log is not a TentaFlow map log: magic {0:02x?}")]
    WalBadMagic([u8; 4]),
    #[error("unsupported wal.log version {found} (this build reads only {expected})")]
    WalUnsupportedVersion { found: u16, expected: u16 },
    #[error("snapshot is {actual} bytes, header declares {expected}")]
    LengthMismatch { expected: usize, actual: usize },
    #[error("checksum mismatch: stored {stored:#010x}, computed {computed:#010x}")]
    Checksum { stored: u32, computed: u32 },
    #[error("declared cell_count {0} exceeds a chunk's 128³ cells")]
    CellCountOutOfRange(u32),
    #[error("payload holds {actual} bytes, {expected} expected for {cells} cells")]
    PayloadShape {
        cells: u32,
        expected: usize,
        actual: usize,
    },
    #[error("zstd payload could not be decoded: {0}")]
    Payload(String),
    /// The file name says one chunk, the header another. Either way the file
    /// cannot be placed in the scene, so it is treated as damaged.
    #[error("snapshot header does not match its own identity: {0}")]
    IdentityMismatch(String),
}

type Result<T> = std::result::Result<T, ChunkIoError>;

fn io_err(context: impl Into<String>, source: std::io::Error) -> ChunkIoError {
    ChunkIoError::Io {
        context: context.into(),
        source,
    }
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// One `.tfmc` file: a whole chunk at one revision.
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkSnapshot {
    pub scene_id: u64,
    pub chunk_key: ChunkKey,
    /// Part of the identity, not decoration: the same cells at a different
    /// resolution are a different map, so a mismatch is a damaged file.
    pub voxel_res: f32,
    pub revision: u64,
    pub owner_epoch: u32,
    /// Ordered by index, and only cells that ever held a hit.
    pub cells: Vec<PersistedCell>,
}

impl ChunkSnapshot {
    /// Captures a live chunk. Free and unknown space is not materialized, so
    /// what is written is exactly what the grid holds.
    pub fn from_chunk(
        scene_id: u64,
        chunk_key: ChunkKey,
        voxel_res: f32,
        owner_epoch: u32,
        chunk: &Chunk,
    ) -> Self {
        Self {
            scene_id,
            chunk_key,
            voxel_res,
            revision: chunk.revision,
            owner_epoch,
            cells: chunk.persisted_cells(),
        }
    }

    /// Serializes the whole file, little-endian, with the crc32 footer over
    /// every preceding byte — header included, so a flipped `revision` is
    /// caught just like a flipped cell.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let n = self.cells.len();
        let mut raw = Vec::with_capacity(n * TFMC_CELL_BYTES);
        // Structure of arrays: each field is contiguous, which is both what
        // zstd compresses well and what a future mmap reader can slice.
        for c in &self.cells {
            raw.extend_from_slice(&c.index.to_le_bytes());
        }
        for c in &self.cells {
            raw.push(c.log_odds as u8);
        }
        for c in &self.cells {
            raw.push(c.hits);
        }
        for c in &self.cells {
            raw.extend_from_slice(&c.last_seen_rev.to_le_bytes());
        }
        for c in &self.cells {
            raw.extend_from_slice(&c.first_seen_delta.to_le_bytes());
        }
        for c in &self.cells {
            raw.push(c.flags);
        }
        let payload = zstd::bulk::compress(&raw, ZSTD_LEVEL)
            .map_err(|e| ChunkIoError::Payload(e.to_string()))?;

        let mut out = Vec::with_capacity(TFMC_HEADER_LEN + payload.len() + 4);
        out.extend_from_slice(&TFMC_MAGIC);
        out.extend_from_slice(&TFMC_VERSION.to_le_bytes());
        out.extend_from_slice(&self.scene_id.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.0.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.1.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.2.to_le_bytes());
        out.extend_from_slice(&self.voxel_res.to_le_bytes());
        out.extend_from_slice(&self.revision.to_le_bytes());
        out.extend_from_slice(&self.owner_epoch.to_le_bytes());
        out.extend_from_slice(&(n as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&payload);
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        Ok(out)
    }

    /// Parses a whole file. Every failure here means the file is damaged or
    /// foreign, and the caller quarantines it rather than guessing.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < TFMC_HEADER_LEN + 4 {
            return Err(ChunkIoError::LengthMismatch {
                expected: TFMC_HEADER_LEN + 4,
                actual: bytes.len(),
            });
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[0..4]);
        if magic != TFMC_MAGIC {
            return Err(ChunkIoError::BadMagic(magic));
        }
        let version = le_u16(&bytes[4..6]);
        if version != TFMC_VERSION {
            return Err(ChunkIoError::UnsupportedVersion {
                found: version,
                expected: TFMC_VERSION,
            });
        }
        let scene_id = le_u64(&bytes[6..14]);
        let chunk_key = ChunkKey(
            le_i32(&bytes[14..18]),
            le_i32(&bytes[18..22]),
            le_i32(&bytes[22..26]),
        );
        let voxel_res = f32::from_le_bytes([bytes[26], bytes[27], bytes[28], bytes[29]]);
        let revision = le_u64(&bytes[30..38]);
        let owner_epoch = le_u32(&bytes[38..42]);
        let cell_count = le_u32(&bytes[42..46]);
        let payload_len = le_u32(&bytes[46..50]) as usize;

        if cell_count > MAX_CELLS_PER_CHUNK {
            return Err(ChunkIoError::CellCountOutOfRange(cell_count));
        }
        let expected_len = TFMC_HEADER_LEN + payload_len + 4;
        if bytes.len() != expected_len {
            return Err(ChunkIoError::LengthMismatch {
                expected: expected_len,
                actual: bytes.len(),
            });
        }
        // Checksum before the payload is handed to the decompressor: a zstd
        // frame built from corrupt bytes is still a decompression request.
        let stored = le_u32(&bytes[expected_len - 4..expected_len]);
        let computed = crc32c::crc32c(&bytes[..expected_len - 4]);
        if stored != computed {
            return Err(ChunkIoError::Checksum { stored, computed });
        }

        let n = cell_count as usize;
        let raw_len = n * TFMC_CELL_BYTES;
        let raw = zstd::bulk::decompress(&bytes[TFMC_HEADER_LEN..TFMC_HEADER_LEN + payload_len], raw_len)
            .map_err(|e| ChunkIoError::Payload(e.to_string()))?;
        if raw.len() != raw_len {
            return Err(ChunkIoError::PayloadShape {
                cells: cell_count,
                expected: raw_len,
                actual: raw.len(),
            });
        }

        let idx_at = 0;
        let lo_at = idx_at + n * 4;
        let hits_at = lo_at + n;
        let lsr_at = hits_at + n;
        let fsd_at = lsr_at + n * 4;
        let flags_at = fsd_at + n * 2;
        let mut cells = Vec::with_capacity(n);
        for i in 0..n {
            cells.push(PersistedCell {
                index: le_u32(&raw[idx_at + i * 4..idx_at + i * 4 + 4]),
                log_odds: raw[lo_at + i] as i8,
                hits: raw[hits_at + i],
                last_seen_rev: le_u32(&raw[lsr_at + i * 4..lsr_at + i * 4 + 4]),
                first_seen_delta: le_u16(&raw[fsd_at + i * 2..fsd_at + i * 2 + 2]),
                flags: raw[flags_at + i],
            });
        }

        Ok(Self {
            scene_id,
            chunk_key,
            voxel_res,
            revision,
            owner_epoch,
            cells,
        })
    }
}

// ---------------------------------------------------------------------------
// Write-ahead log
// ---------------------------------------------------------------------------

/// One frame's change to one chunk, as `wal.log` stores it.
///
/// Each cell is its COMPLETE state after the frame — log-odds, hit count, both
/// timing fields and the flags — not a diff and not a subset. Replay therefore
/// assigns what the record says, with no field reconstructed from the movement
/// of another; a cell whose `hits` and `log_odds` have both saturated is
/// restored exactly like any other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecord {
    pub revision: u64,
    pub chunk_key: ChunkKey,
    pub cells: Vec<PersistedCell>,
}

impl WalRecord {
    fn encode(&self) -> Vec<u8> {
        let n = self.cells.len();
        debug_assert!(n <= usize::from(u16::MAX), "a record holds at most u16 cells");
        let mut out = Vec::with_capacity(WAL_RECORD_HEADER_LEN + n * WAL_CELL_BYTES + 4);
        out.extend_from_slice(&self.revision.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.0.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.1.to_le_bytes());
        out.extend_from_slice(&self.chunk_key.2.to_le_bytes());
        out.extend_from_slice(&(n as u16).to_le_bytes());
        // Array of structs, not SoA: a record is a handful of cells appended on
        // the ingest path, so locality beats a compression layout it never gets.
        for c in &self.cells {
            out.extend_from_slice(&c.index.to_le_bytes());
            out.push(c.log_odds as u8);
            out.push(c.hits);
            out.extend_from_slice(&c.last_seen_rev.to_le_bytes());
            out.extend_from_slice(&c.first_seen_delta.to_le_bytes());
            out.push(c.flags);
        }
        let crc = crc32c::crc32c(&out);
        out.extend_from_slice(&crc.to_le_bytes());
        out
    }
}

/// Why log replay stopped short of the file's end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalStop {
    /// The last record is incomplete — the process died between the write and
    /// the next one. Expected, not an error: the frame simply never happened.
    Truncated { offset: u64 },
    /// A complete-looking record whose checksum disagrees. The framing after it
    /// cannot be trusted either, so replay ends here as well; the difference
    /// from `Truncated` exists so the caller can say so out loud.
    Checksum { offset: u64 },
}

/// The result of reading `wal.log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalReplay {
    pub records: Vec<WalRecord>,
    /// Bytes that parsed cleanly. Appending must resume here, or the damaged
    /// tail would sit in the middle of the log forever.
    pub valid_bytes: u64,
    pub stop: Option<WalStop>,
}

/// The six bytes that open every `wal.log`.
fn wal_prefix() -> [u8; WAL_PREFIX_LEN] {
    let v = WAL_VERSION.to_le_bytes();
    [
        WAL_MAGIC[0],
        WAL_MAGIC[1],
        WAL_MAGIC[2],
        WAL_MAGIC[3],
        v[0],
        v[1],
    ]
}

/// Parses a WAL image, prefix included. Stops at the first record that cannot
/// be read whole and verified, and reports where — it never treats a short tail
/// as corruption. A wrong magic or version is an error, not a stop: the log is
/// scene-wide and must not be skipped past quietly.
///
/// `valid_bytes` is an absolute file offset, so a caller can truncate to it.
pub fn parse_wal(bytes: &[u8]) -> Result<WalReplay> {
    if bytes.len() < WAL_PREFIX_LEN {
        // Too short to hold a prefix, let alone a record: the log was being
        // created when the process died. Nothing was ever logged.
        return Ok(WalReplay {
            records: Vec::new(),
            valid_bytes: 0,
            stop: None,
        });
    }
    let mut magic = [0u8; 4];
    magic.copy_from_slice(&bytes[0..4]);
    if magic != WAL_MAGIC {
        return Err(ChunkIoError::WalBadMagic(magic));
    }
    let version = le_u16(&bytes[4..6]);
    if version != WAL_VERSION {
        return Err(ChunkIoError::WalUnsupportedVersion {
            found: version,
            expected: WAL_VERSION,
        });
    }

    let mut records = Vec::new();
    let mut at = WAL_PREFIX_LEN;
    let mut stop = None;
    while at < bytes.len() {
        if bytes.len() - at < WAL_RECORD_HEADER_LEN + 4 {
            stop = Some(WalStop::Truncated { offset: at as u64 });
            break;
        }
        let revision = le_u64(&bytes[at..at + 8]);
        let chunk_key = ChunkKey(
            le_i32(&bytes[at + 8..at + 12]),
            le_i32(&bytes[at + 12..at + 16]),
            le_i32(&bytes[at + 16..at + 20]),
        );
        let n = le_u16(&bytes[at + 20..at + 22]) as usize;
        let total = WAL_RECORD_HEADER_LEN + n * WAL_CELL_BYTES + 4;
        if bytes.len() - at < total {
            stop = Some(WalStop::Truncated { offset: at as u64 });
            break;
        }
        let stored = le_u32(&bytes[at + total - 4..at + total]);
        let computed = crc32c::crc32c(&bytes[at..at + total - 4]);
        if stored != computed {
            stop = Some(WalStop::Checksum { offset: at as u64 });
            break;
        }
        let mut cells = Vec::with_capacity(n);
        for i in 0..n {
            let c = at + WAL_RECORD_HEADER_LEN + i * WAL_CELL_BYTES;
            cells.push(PersistedCell {
                index: le_u32(&bytes[c..c + 4]),
                log_odds: bytes[c + 4] as i8,
                hits: bytes[c + 5],
                last_seen_rev: le_u32(&bytes[c + 6..c + 10]),
                first_seen_delta: le_u16(&bytes[c + 10..c + 12]),
                flags: bytes[c + 12],
            });
        }
        records.push(WalRecord {
            revision,
            chunk_key,
            cells,
        });
        at += total;
    }
    Ok(WalReplay {
        records,
        valid_bytes: at as u64,
        stop,
    })
}

// ---------------------------------------------------------------------------
// Scene directory
// ---------------------------------------------------------------------------

/// A snapshot that could not be read and was moved aside.
#[derive(Debug, Clone)]
pub struct Quarantined {
    pub chunk_key: Option<ChunkKey>,
    /// Where the damaged file now sits, under `chunks/corrupt/`.
    pub moved_to: PathBuf,
    pub reason: String,
}

/// What opening a scene found. Every degraded outcome is named here rather than
/// swallowed: a chunk that came back empty because its file was shredded must
/// not be indistinguishable from a chunk nobody has scanned yet.
#[derive(Debug, Clone, Default)]
pub struct OpenReport {
    pub snapshots_loaded: usize,
    pub wal_records_replayed: usize,
    pub quarantined: Vec<Quarantined>,
    /// Chunks whose every snapshot was damaged. The plan's recovery (replay
    /// from WAL if it spans the gap, else pull the chunk from a peer) is the
    /// caller's decision — this layer only reports the hole.
    pub missing_chunks: Vec<ChunkKey>,
    pub wal_stop: Option<WalStop>,
}

/// What a compaction did.
#[derive(Debug, Clone, Default)]
pub struct CompactionReport {
    pub written: Vec<(ChunkKey, u64)>,
    pub superseded_removed: usize,
    /// Chunks that compaction emptied; their snapshots are gone.
    pub dropped_chunks: Vec<ChunkKey>,
    pub cells_dropped: usize,
    pub wal_truncated: bool,
}

/// The files of one scene: `chunks/`, `chunks/corrupt/` and `wal.log`.
///
/// One process owns this at a time — the plan's single-writer rule. The type
/// holds the append handle, so there is nowhere else for a second writer to be.
#[derive(Debug)]
pub struct SceneStore {
    root: PathBuf,
    scene_id: u64,
    voxel_res: f32,
    owner_epoch: u32,
    wal: File,
}

/// A scene loaded from disk: its files and the grid they reconstruct.
#[derive(Debug)]
pub struct OpenedScene {
    pub store: SceneStore,
    pub grid: OccupancyGrid,
    pub report: OpenReport,
}

impl SceneStore {
    /// Reads every snapshot, replays `wal.log` on top and hands back the grid
    /// as it was before the last shutdown — crash or not.
    ///
    /// `voxel_res` and `scene_id` are the caller's truth (they come from the
    /// scene row); a snapshot that disagrees is a file from another scene or a
    /// damaged one, and is quarantined either way.
    pub fn open(
        root: impl AsRef<Path>,
        scene_id: u64,
        voxel_res: f32,
        owner_epoch: u32,
        max_voxels: usize,
    ) -> Result<OpenedScene> {
        let root = root.as_ref().to_path_buf();
        let chunks_dir = root.join(CHUNKS_DIR);
        fs::create_dir_all(&chunks_dir)
            .map_err(|e| io_err(format!("create {}", chunks_dir.display()), e))?;

        let mut report = OpenReport::default();
        let mut grid = OccupancyGrid::new(voxel_res, max_voxels);

        // Newest revision first per chunk, so a half-deleted superseded file
        // never wins over the snapshot compaction actually published.
        let mut by_chunk: BTreeMap<ChunkKey, Vec<(u64, PathBuf)>> = BTreeMap::new();
        for entry in fs::read_dir(&chunks_dir)
            .map_err(|e| io_err(format!("read {}", chunks_dir.display()), e))?
        {
            let entry = entry.map_err(|e| io_err(format!("read {}", chunks_dir.display()), e))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            match parse_snapshot_name(name) {
                Some((key, rev)) => by_chunk.entry(key).or_default().push((rev, path)),
                // Anything else in `chunks/` is not ours to interpret; leaving
                // it alone beats quarantining a file we do not understand.
                None => continue,
            }
        }

        for (key, mut candidates) in by_chunk {
            candidates.sort_by(|a, b| b.0.cmp(&a.0));
            let mut loaded = false;
            for (_, path) in candidates {
                match read_snapshot(&path, scene_id, voxel_res, key) {
                    Ok(snapshot) if !loaded => {
                        grid.restore_chunk(key, snapshot.revision, &snapshot.cells);
                        report.snapshots_loaded += 1;
                        loaded = true;
                    }
                    // A healthy superseded snapshot: compaction was interrupted
                    // after the new file landed. Remove it now.
                    Ok(_) => {
                        fs::remove_file(&path)
                            .map_err(|e| io_err(format!("remove {}", path.display()), e))?;
                    }
                    Err(err) => {
                        let moved_to = quarantine(&root, &path)?;
                        report.quarantined.push(Quarantined {
                            chunk_key: Some(key),
                            moved_to,
                            reason: err.to_string(),
                        });
                    }
                }
            }
            if !loaded {
                report.missing_chunks.push(key);
            }
        }

        let wal_path = root.join(WAL_FILE);
        let mut wal = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&wal_path)
            .map_err(|e| io_err(format!("open {}", wal_path.display()), e))?;
        let mut bytes = Vec::new();
        wal.read_to_end(&mut bytes)
            .map_err(|e| io_err(format!("read {}", wal_path.display()), e))?;
        let replay = if bytes.len() < WAL_PREFIX_LEN {
            // A fresh (or never-finished) log: stamp the prefix so every later
            // reader can tell this format from the next one.
            reset_wal(&mut wal)?;
            WalReplay {
                records: Vec::new(),
                valid_bytes: WAL_PREFIX_LEN as u64,
                stop: None,
            }
        } else {
            parse_wal(&bytes)?
        };
        report.wal_stop = replay.stop;

        for record in &replay.records {
            // A record already folded into a snapshot must not be applied
            // twice: replaying it would advance `first_seen_delta` past the
            // truth and make a young cell look persistent.
            let snapshot_rev = grid.chunk(record.chunk_key).map(|c| c.revision).unwrap_or(0);
            if record.revision <= snapshot_rev {
                continue;
            }
            grid.apply_delta_cells(record.chunk_key, record.revision, &record.cells);
            report.wal_records_replayed += 1;
        }

        // Drop the unusable tail before anything appends: a damaged record left
        // in place would sit between good ones forever, ending every future
        // replay at the same spot.
        if replay.valid_bytes < bytes.len() as u64 {
            wal.set_len(replay.valid_bytes)
                .map_err(|e| io_err(format!("truncate {}", wal_path.display()), e))?;
        }
        wal.seek(SeekFrom::End(0))
            .map_err(|e| io_err(format!("seek {}", wal_path.display()), e))?;

        Ok(OpenedScene {
            store: SceneStore {
                root,
                scene_id,
                voxel_res,
                owner_epoch,
                wal,
            },
            grid,
            report,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn scene_id(&self) -> u64 {
        self.scene_id
    }

    pub fn owner_epoch(&self) -> u32 {
        self.owner_epoch
    }

    /// Appends one record. Buffered by the OS; `sync` is what makes it durable,
    /// and the plan's cadence (flush 250 ms, fsync 1 s) belongs to the caller
    /// that owns the ingest clock.
    pub fn append(&mut self, record: &WalRecord) -> Result<()> {
        self.wal
            .write_all(&record.encode())
            .map_err(|e| io_err("append to wal.log", e))
    }

    /// Forces the log to stable storage.
    pub fn sync(&mut self) -> Result<()> {
        self.wal
            .flush()
            .map_err(|e| io_err("flush wal.log", e))?;
        self.wal.sync_data().map_err(|e| io_err("fsync wal.log", e))
    }

    /// Writes a fresh snapshot for every dirty chunk, drops the snapshots those
    /// supersede, and truncates the log — in that order, and the truncation
    /// only once every dirty chunk is on disk. Any earlier truncation would
    /// throw away the only record of a chunk whose write had not landed.
    ///
    /// `grid` is compacted first, so cells marked `removed-pending` leave for
    /// good here rather than being written one more time.
    ///
    /// An error leaves the log intact and the in-memory grid no longer marked
    /// dirty, so recovery is to reopen the scene rather than to retry against
    /// this grid — the disk, which still holds every record, is the truth.
    pub fn compact(&mut self, grid: &mut OccupancyGrid) -> Result<CompactionReport> {
        let mut report = CompactionReport::default();
        let dirty = grid.dirty_chunk_keys();
        report.cells_dropped = grid.compact();

        for key in dirty {
            match grid.chunk(key) {
                Some(chunk) if chunk.materialized_cells() > 0 => {
                    let snapshot = ChunkSnapshot::from_chunk(
                        self.scene_id,
                        key,
                        self.voxel_res,
                        self.owner_epoch,
                        chunk,
                    );
                    let revision = snapshot.revision;
                    self.write_snapshot(&snapshot)?;
                    report.superseded_removed += self.remove_snapshots(key, Some(revision))?;
                    report.written.push((key, revision));
                }
                // Compaction emptied the chunk: its geometry is gone, so the
                // files must go too or the next open would resurrect it.
                _ => {
                    report.superseded_removed += self.remove_snapshots(key, None)?;
                    report.dropped_chunks.push(key);
                }
            }
        }

        reset_wal(&mut self.wal)?;
        self.wal
            .sync_data()
            .map_err(|e| io_err("fsync wal.log", e))?;
        report.wal_truncated = true;
        Ok(report)
    }

    /// Writes one `.tfmc` through a temporary file and a rename, so a reader
    /// never sees a half-written snapshot under its final name.
    fn write_snapshot(&self, snapshot: &ChunkSnapshot) -> Result<PathBuf> {
        let dir = self.root.join(CHUNKS_DIR);
        fs::create_dir_all(&dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
        let final_path = dir.join(snapshot_name(snapshot.chunk_key, snapshot.revision));
        let tmp_path = final_path.with_extension("tfmc.tmp");
        let bytes = snapshot.encode()?;
        {
            let mut f = File::create(&tmp_path)
                .map_err(|e| io_err(format!("create {}", tmp_path.display()), e))?;
            f.write_all(&bytes)
                .map_err(|e| io_err(format!("write {}", tmp_path.display()), e))?;
            f.sync_all()
                .map_err(|e| io_err(format!("fsync {}", tmp_path.display()), e))?;
        }
        fs::rename(&tmp_path, &final_path)
            .map_err(|e| io_err(format!("publish {}", final_path.display()), e))?;
        Ok(final_path)
    }

    /// Removes this chunk's snapshots, optionally sparing one revision.
    fn remove_snapshots(&self, key: ChunkKey, keep: Option<u64>) -> Result<usize> {
        let dir = self.root.join(CHUNKS_DIR);
        let mut removed = 0;
        for entry in
            fs::read_dir(&dir).map_err(|e| io_err(format!("read {}", dir.display()), e))?
        {
            let entry = entry.map_err(|e| io_err(format!("read {}", dir.display()), e))?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some((found_key, rev)) = parse_snapshot_name(name) else {
                continue;
            };
            if found_key != key || keep == Some(rev) {
                continue;
            }
            fs::remove_file(&path)
                .map_err(|e| io_err(format!("remove {}", path.display()), e))?;
            removed += 1;
        }
        Ok(removed)
    }
}

/// Empties `wal.log` back to its versioned prefix. Truncating to zero instead
/// would leave a log with no magic, which the next open could not tell from a
/// foreign file.
fn reset_wal(wal: &mut File) -> Result<()> {
    wal.set_len(0).map_err(|e| io_err("truncate wal.log", e))?;
    wal.seek(SeekFrom::Start(0))
        .map_err(|e| io_err("rewind wal.log", e))?;
    wal.write_all(&wal_prefix())
        .map_err(|e| io_err("write wal.log prefix", e))
}

/// Reads and validates one snapshot file, including that it is the chunk its
/// name claims and belongs to this scene at this resolution.
fn read_snapshot(
    path: &Path,
    scene_id: u64,
    voxel_res: f32,
    expect_key: ChunkKey,
) -> Result<ChunkSnapshot> {
    let bytes =
        fs::read(path).map_err(|e| io_err(format!("read {}", path.display()), e))?;
    let snapshot = ChunkSnapshot::decode(&bytes)?;
    if snapshot.scene_id != scene_id {
        return Err(ChunkIoError::IdentityMismatch(format!(
            "scene {} in a file of scene {}",
            snapshot.scene_id, scene_id
        )));
    }
    if snapshot.chunk_key != expect_key {
        return Err(ChunkIoError::IdentityMismatch(format!(
            "header names chunk {}, file name says {}",
            snapshot.chunk_key.to_key_string(),
            expect_key.to_key_string()
        )));
    }
    if snapshot.voxel_res != voxel_res {
        return Err(ChunkIoError::IdentityMismatch(format!(
            "voxel_res {} in a scene at {}",
            snapshot.voxel_res, voxel_res
        )));
    }
    Ok(snapshot)
}

/// Moves a damaged file to `chunks/corrupt/`, keeping its name so the chunk and
/// revision it claimed stay readable by a human. It is kept, not deleted: it is
/// the only evidence of what went wrong.
fn quarantine(root: &Path, path: &Path) -> Result<PathBuf> {
    let dir = root.join(CHUNKS_DIR).join(CORRUPT_DIR);
    fs::create_dir_all(&dir).map_err(|e| io_err(format!("create {}", dir.display()), e))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unnamed")
        .to_string();
    let mut target = dir.join(&name);
    let mut attempt = 1u32;
    while target.exists() {
        target = dir.join(format!("{name}.{attempt}"));
        attempt += 1;
    }
    fs::rename(path, &target).map_err(|e| {
        io_err(
            format!("quarantine {} to {}", path.display(), target.display()),
            e,
        )
    })?;
    Ok(target)
}

fn snapshot_name(key: ChunkKey, revision: u64) -> String {
    format!("{}.{}.{}", key.to_key_string(), revision, SNAPSHOT_EXT)
}

/// `<cx>_<cy>_<cz>.<revision>.tfmc`. Chunk coordinates may be negative, which
/// is why the revision is split off from the right, not the key from the left.
fn parse_snapshot_name(name: &str) -> Option<(ChunkKey, u64)> {
    let stem = name.strip_suffix(".tfmc")?;
    let (key, revision) = stem.rsplit_once('.')?;
    Some((ChunkKey::parse(key)?, revision.parse().ok()?))
}

fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn le_u32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le_i32(b: &[u8]) -> i32 {
    i32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn le_u64(b: &[u8]) -> u64 {
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::occupancy::{flags, Cell};

    const SCENE: u64 = 0x5cee_1d00;
    const RES: f32 = 0.05;
    const EPOCH: u32 = 7;

    /// Builds a grid with three surfaces spread over two chunks, each passing
    /// the persistence gate, plus one cell that is still only a candidate.
    fn scanned_grid() -> OccupancyGrid {
        let mut g = OccupancyGrid::new(RES, 1_000_000);
        let stable: [Cell; 3] = [[1, 2, 3], [200, 4, 5], [1, 2, 4]];
        for (i, t) in [0i64, 700_000, 1_400_000, 2_100_000].iter().enumerate() {
            let s = g.begin_frame(*t);
            for cell in stable {
                g.hit(s, cell, i % 2 == 0);
            }
        }
        let s = g.begin_frame(3_000_000);
        g.hit(s, [9, 9, 9], false);
        g
    }

    /// The canonical shape of a grid, for "identical before and after".
    fn fingerprint(g: &OccupancyGrid) -> Vec<(ChunkKey, u64, Vec<PersistedCell>)> {
        g.chunk_keys()
            .into_iter()
            .map(|k| {
                let c = g.chunk(k).unwrap();
                (k, c.revision, c.persisted_cells())
            })
            .collect()
    }

    fn open(dir: &Path) -> OpenedScene {
        SceneStore::open(dir, SCENE, RES, EPOCH, 1_000_000).expect("scene opens")
    }

    #[test]
    fn a_snapshot_round_trips_every_cell_field_through_its_checksum() {
        let g = scanned_grid();
        let key = g.chunk_keys()[0];
        let chunk = g.chunk(key).unwrap();
        let snapshot = ChunkSnapshot::from_chunk(SCENE, key, RES, EPOCH, chunk);
        assert!(!snapshot.cells.is_empty());

        let bytes = snapshot.encode().unwrap();
        assert_eq!(&bytes[0..4], &TFMC_MAGIC);
        let decoded = ChunkSnapshot::decode(&bytes).unwrap();
        assert_eq!(decoded, snapshot);
        assert!(decoded.cells.iter().any(|c| c.flags & flags::STABLE != 0));
    }

    #[test]
    fn a_snapshot_with_a_flipped_byte_fails_its_checksum_instead_of_decoding() {
        let g = scanned_grid();
        let key = g.chunk_keys()[0];
        let snapshot = ChunkSnapshot::from_chunk(SCENE, key, RES, EPOCH, g.chunk(key).unwrap());
        let mut bytes = snapshot.encode().unwrap();
        // The revision lives in the header: proof the crc covers it, not just
        // the payload it would be tempting to checksum alone.
        bytes[30] ^= 0x01;
        assert!(matches!(
            ChunkSnapshot::decode(&bytes),
            Err(ChunkIoError::Checksum { .. })
        ));
    }

    #[test]
    fn the_reader_refuses_a_snapshot_written_by_an_older_format_version() {
        let g = scanned_grid();
        let key = g.chunk_keys()[0];
        let snapshot = ChunkSnapshot::from_chunk(SCENE, key, RES, EPOCH, g.chunk(key).unwrap());
        let mut bytes = snapshot.encode().unwrap();
        bytes[4..6].copy_from_slice(&(TFMC_VERSION - 1).to_le_bytes());
        // Re-checksum, so the rejection is about the version and nothing else.
        let end = bytes.len() - 4;
        let crc = crc32c::crc32c(&bytes[..end]);
        bytes[end..].copy_from_slice(&crc.to_le_bytes());

        match ChunkSnapshot::decode(&bytes) {
            Err(ChunkIoError::UnsupportedVersion { found, expected }) => {
                assert_eq!(found, TFMC_VERSION - 1);
                assert_eq!(expected, TFMC_VERSION);
            }
            other => panic!("expected a version refusal, got {other:?}"),
        }
    }

    #[test]
    fn replaying_the_wal_rebuilds_the_state_the_crash_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let mut grid;
        let expected;
        {
            let opened = open(dir.path());
            let mut store = opened.store;
            grid = opened.grid;
            // Snapshot an empty scene, then log four frames without compacting:
            // the on-disk truth is now the log alone.
            for (i, t) in [0i64, 700_000, 1_400_000, 2_100_000].iter().enumerate() {
                let s = grid.begin_frame(*t);
                let mut touched: BTreeMap<ChunkKey, Vec<u32>> = BTreeMap::new();
                for cell in [[1, 2, 3], [200, 4, 5], [1, 2, 4]] {
                    let id = grid.cell_id(cell);
                    grid.hit(s, cell, i % 2 == 0);
                    touched.entry(id.chunk).or_default().push(id.index);
                }
                for (key, indices) in touched {
                    let cells = grid.chunk(key).unwrap().delta_cells(&indices);
                    store
                        .append(&WalRecord {
                            revision: s.revision,
                            chunk_key: key,
                            cells,
                        })
                        .unwrap();
                }
            }
            store.sync().unwrap();
            expected = fingerprint(&grid);
        }

        let reopened = open(dir.path());
        assert_eq!(reopened.report.wal_records_replayed, 8);
        assert_eq!(reopened.report.wal_stop, None);
        assert_eq!(fingerprint(&reopened.grid), expected);
        assert_eq!(reopened.grid.revision(), grid.revision());
        assert_eq!(
            reopened.grid.materialized_cells(),
            grid.materialized_cells()
        );
    }

    #[test]
    fn a_wal_record_cut_in_half_by_a_crash_is_ignored_not_treated_as_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(dir.path());
        let mut store = opened.store;
        let mut grid = opened.grid;

        let mut last = None;
        for t in [0i64, 700_000, 1_400_000] {
            let s = grid.begin_frame(t);
            let id = grid.cell_id([4, 4, 4]);
            grid.hit(s, [4, 4, 4], false);
            let cells = grid.chunk(id.chunk).unwrap().delta_cells(&[id.index]);
            store
                .append(&WalRecord {
                    revision: s.revision,
                    chunk_key: id.chunk,
                    cells,
                })
                .unwrap();
            last = Some(s.revision);
        }
        store.sync().unwrap();
        let after_three = fingerprint(&grid);

        // A fourth frame whose record only half reached the disk.
        let s = grid.begin_frame(2_800_000);
        let id = grid.cell_id([4, 4, 4]);
        grid.hit(s, [4, 4, 4], false);
        let cells = grid.chunk(id.chunk).unwrap().delta_cells(&[id.index]);
        let partial = WalRecord {
            revision: s.revision,
            chunk_key: id.chunk,
            cells,
        }
        .encode();
        let wal_path = dir.path().join(WAL_FILE);
        let full_len = fs::metadata(&wal_path).unwrap().len();
        {
            let mut f = OpenOptions::new().append(true).open(&wal_path).unwrap();
            f.write_all(&partial[..partial.len() - 5]).unwrap();
            f.sync_data().unwrap();
        }
        drop(store);

        let reopened = open(dir.path());
        assert_eq!(reopened.report.wal_records_replayed, 3);
        assert_eq!(
            reopened.report.wal_stop,
            Some(WalStop::Truncated { offset: full_len })
        );
        assert_eq!(
            fingerprint(&reopened.grid),
            after_three,
            "the frame that never finished writing never happened"
        );
        assert_eq!(reopened.grid.revision(), last.unwrap());
        // The tail is gone, so the next append starts from a clean boundary.
        assert_eq!(fs::metadata(&wal_path).unwrap().len(), full_len);
    }

    /// The case a derived `last_seen_rev` could not see: once `hits` is pinned
    /// at 255 and the log-odds are at the clamp, a hit changes neither field,
    /// so nothing in the record's *values* distinguishes it from a miss. The
    /// record states both timing fields outright, so it does not have to.
    #[test]
    fn a_cell_with_saturated_hits_and_clamped_log_odds_replays_bit_identically() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(dir.path());
        let mut store = opened.store;
        let mut grid = opened.grid;
        let cell: Cell = [6, 6, 6];
        let id = grid.cell_id(cell);

        // Well past both saturation points: `hits` is a u8 and the log-odds
        // clamp is reached after a handful of hits.
        for frame in 0..300i64 {
            let s = grid.begin_frame(frame * 300_000);
            grid.hit(s, cell, false);
            let cells = grid.chunk(id.chunk).unwrap().delta_cells(&[id.index]);
            store
                .append(&WalRecord {
                    revision: s.revision,
                    chunk_key: id.chunk,
                    cells,
                })
                .unwrap();
        }
        store.sync().unwrap();

        let saturated = grid
            .chunk(id.chunk)
            .unwrap()
            .persisted_cells()
            .into_iter()
            .find(|c| c.index == id.index)
            .unwrap();
        assert_eq!(saturated.hits, u8::MAX, "hit count is pinned");
        assert_eq!(grid.log_odds(cell), 3.5, "log-odds are at the clamp");
        // It was still being seen at the last frame, long after both fields
        // stopped moving — which is precisely what a derivation would miss.
        assert_eq!(saturated.last_seen_rev, 300);
        let expected = fingerprint(&grid);
        drop(store);

        let reopened = open(dir.path());
        assert_eq!(reopened.report.wal_records_replayed, 300);
        assert_eq!(
            fingerprint(&reopened.grid),
            expected,
            "every field survives, saturated ones included"
        );
        // Spelled out, because these two are the fields the old derivation got
        // wrong and a whole-grid comparison would not name.
        let back = reopened
            .grid
            .chunk(id.chunk)
            .unwrap()
            .persisted_cells()
            .into_iter()
            .find(|c| c.index == id.index)
            .unwrap();
        assert_eq!(back.last_seen_rev, saturated.last_seen_rev);
        assert_eq!(back.first_seen_delta, saturated.first_seen_delta);
    }

    #[test]
    fn the_reader_refuses_a_wal_written_by_an_older_format_version() {
        let dir = tempfile::tempdir().unwrap();
        drop(open(dir.path()));
        let wal_path = dir.path().join(WAL_FILE);
        let mut bytes = fs::read(&wal_path).unwrap();
        assert_eq!(&bytes[0..4], &WAL_MAGIC, "the log carries its own magic");
        bytes[4..6].copy_from_slice(&(WAL_VERSION + 1).to_le_bytes());
        fs::write(&wal_path, &bytes).unwrap();

        // Refused, not quarantined: the log is scene-wide, so dropping it would
        // discard every frame since the last compaction.
        match SceneStore::open(dir.path(), SCENE, RES, EPOCH, 1_000) {
            Err(ChunkIoError::WalUnsupportedVersion { found, expected }) => {
                assert_eq!(found, WAL_VERSION + 1);
                assert_eq!(expected, WAL_VERSION);
            }
            other => panic!("expected a WAL version refusal, got {:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn compaction_leaves_the_reopened_grid_identical_to_the_grid_it_compacted() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(dir.path());
        let mut store = opened.store;
        let mut grid = opened.grid;

        for (i, t) in [0i64, 700_000, 1_400_000, 2_100_000].iter().enumerate() {
            let s = grid.begin_frame(*t);
            let mut touched: BTreeMap<ChunkKey, Vec<u32>> = BTreeMap::new();
            for cell in [[1, 2, 3], [200, 4, 5], [1, 2, 4]] {
                let id = grid.cell_id(cell);
                grid.hit(s, cell, i % 2 == 0);
                touched.entry(id.chunk).or_default().push(id.index);
            }
            for (key, indices) in touched {
                let cells = grid.chunk(key).unwrap().delta_cells(&indices);
                store
                    .append(&WalRecord {
                        revision: s.revision,
                        chunk_key: key,
                        cells,
                    })
                    .unwrap();
            }
        }
        store.sync().unwrap();

        let report = store.compact(&mut grid).unwrap();
        assert_eq!(report.written.len(), 2, "two chunks were dirty");
        assert!(report.wal_truncated);
        // Emptied back to its versioned prefix, not to nothing.
        assert_eq!(
            fs::metadata(dir.path().join(WAL_FILE)).unwrap().len(),
            WAL_PREFIX_LEN as u64
        );
        let expected = fingerprint(&grid);
        drop(store);

        let reopened = open(dir.path());
        assert_eq!(reopened.report.snapshots_loaded, 2);
        assert_eq!(reopened.report.wal_records_replayed, 0);
        assert!(reopened.report.missing_chunks.is_empty());
        assert_eq!(fingerprint(&reopened.grid), expected);

        // A second compaction of the same state supersedes nothing new and
        // leaves exactly one file per chunk.
        let mut grid2 = reopened.grid;
        let mut store2 = reopened.store;
        let s = grid2.begin_frame(2_800_000);
        let id = grid2.cell_id([1, 2, 3]);
        grid2.hit(s, [1, 2, 3], false);
        store2.compact(&mut grid2).unwrap();
        let files: Vec<_> = fs::read_dir(dir.path().join(CHUNKS_DIR))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .collect();
        assert_eq!(files.len(), 2, "superseded snapshots are dropped");
        assert!(grid2.chunk(id.chunk).is_some());
    }

    #[test]
    fn a_corrupt_snapshot_is_quarantined_and_the_rest_of_the_scene_still_opens() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(dir.path());
        let mut store = opened.store;
        let mut grid = opened.grid;
        for t in [0i64, 700_000, 1_400_000, 2_100_000] {
            let s = grid.begin_frame(t);
            for cell in [[1, 2, 3], [200, 4, 5]] {
                grid.hit(s, cell, false);
            }
        }
        store.compact(&mut grid).unwrap();
        drop(store);

        let victim = grid.cell_id([200, 4, 5]).chunk;
        let survivor = grid.cell_id([1, 2, 3]).chunk;
        let chunks_dir = dir.path().join(CHUNKS_DIR);
        let victim_file = fs::read_dir(&chunks_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(parse_snapshot_name)
                    .is_some_and(|(k, _)| k == victim)
            })
            .expect("victim snapshot exists");
        let mut bytes = fs::read(&victim_file).unwrap();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xff;
        fs::write(&victim_file, &bytes).unwrap();

        let reopened = open(dir.path());
        assert_eq!(reopened.report.quarantined.len(), 1);
        assert_eq!(reopened.report.quarantined[0].chunk_key, Some(victim));
        assert!(reopened.report.quarantined[0].moved_to.exists());
        assert_eq!(reopened.report.missing_chunks, vec![victim]);
        assert!(!victim_file.exists(), "the damaged file left chunks/");

        // The hole is reported, not papered over, and the healthy chunk is
        // unaffected.
        assert!(reopened.grid.chunk(victim).is_none());
        let kept = reopened.grid.chunk(survivor).expect("survivor loaded");
        assert_eq!(
            kept.persisted_cells(),
            grid.chunk(survivor).unwrap().persisted_cells()
        );
    }

    #[test]
    fn a_snapshot_from_another_scene_is_quarantined_rather_than_mixed_in() {
        let dir = tempfile::tempdir().unwrap();
        let opened = open(dir.path());
        let mut store = opened.store;
        let mut grid = opened.grid;
        for t in [0i64, 700_000, 1_400_000, 2_100_000] {
            let s = grid.begin_frame(t);
            grid.hit(s, [1, 2, 3], false);
        }
        store.compact(&mut grid).unwrap();
        drop(store);

        let key = grid.cell_id([1, 2, 3]).chunk;
        let path = dir
            .path()
            .join(CHUNKS_DIR)
            .join(snapshot_name(key, grid.chunk(key).unwrap().revision));
        let foreign = ChunkSnapshot {
            scene_id: SCENE + 1,
            ..ChunkSnapshot::decode(&fs::read(&path).unwrap()).unwrap()
        };
        fs::write(&path, foreign.encode().unwrap()).unwrap();

        let reopened = open(dir.path());
        assert_eq!(reopened.report.quarantined.len(), 1);
        assert_eq!(reopened.report.missing_chunks, vec![key]);
    }

    #[test]
    fn opening_a_scene_directory_that_does_not_exist_yet_creates_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let scene = dir.path().join("maps").join("41");
        let opened = SceneStore::open(&scene, SCENE, RES, EPOCH, 1_000).unwrap();
        assert_eq!(opened.grid.chunk_keys(), Vec::new());
        assert_eq!(opened.report.snapshots_loaded, 0);
        assert!(scene.join(CHUNKS_DIR).is_dir());
        assert!(scene.join(WAL_FILE).is_file());
    }
}
