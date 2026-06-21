// =============================================================================
// File: lidar/voxel.rs — voxel downsampling + a voxel-hash local map for NN.
// Purpose: KISS-ICP uses a sparse voxel HASH (not a kd-tree) both to downsample a
// scan to ~one point per voxel and as the local-map nearest-neighbour structure.
// That keeps it allocation-light and dependency-free (no kd-tree crate). Geometry
// is f32 (local extents are small); ICP queries convert to f64.
// =============================================================================

use std::collections::HashMap;

use nalgebra::{Point3, Vector3};

/// Integer voxel key. f32 world / voxel_size, floored.
type Key = [i32; 3];

#[inline]
fn key_of(p: &Point3<f64>, voxel_size: f64) -> Key {
    [
        (p.x / voxel_size).floor() as i32,
        (p.y / voxel_size).floor() as i32,
        (p.z / voxel_size).floor() as i32,
    ]
}

/// Downsample a scan to the CENTROID of each occupied voxel (order-independent, so
/// the result is deterministic regardless of point order — important for repeatable
/// registration). Returns one point per voxel.
pub fn voxel_downsample(points: &[Point3<f32>], voxel_size: f32) -> Vec<Point3<f32>> {
    debug_assert!(voxel_size > 0.0);
    let inv = 1.0 / voxel_size;
    // accumulate sum + count per voxel.
    let mut acc: HashMap<Key, (Vector3<f64>, u32)> = HashMap::new();
    for p in points {
        let k = [
            (p.x * inv).floor() as i32,
            (p.y * inv).floor() as i32,
            (p.z * inv).floor() as i32,
        ];
        let e = acc.entry(k).or_insert((Vector3::zeros(), 0));
        e.0 += Vector3::new(p.x as f64, p.y as f64, p.z as f64);
        e.1 += 1;
    }
    acc.into_values()
        .map(|(sum, n)| {
            let c = sum / n as f64;
            Point3::new(c.x as f32, c.y as f32, c.z as f32)
        })
        .collect()
}

/// A sparse voxel-hash point map: the local map ICP registers against. Caps points
/// per voxel so a long session can't grow a voxel unbounded (KISS-ICP does the same).
#[derive(Debug, Clone)]
pub struct VoxelMap {
    voxel_size: f64,
    max_points_per_voxel: usize,
    voxels: HashMap<Key, Vec<Point3<f32>>>,
    len: usize,
}

impl VoxelMap {
    pub fn new(voxel_size: f64, max_points_per_voxel: usize) -> Self {
        assert!(voxel_size > 0.0);
        assert!(max_points_per_voxel > 0);
        VoxelMap {
            voxel_size,
            max_points_per_voxel,
            voxels: HashMap::new(),
            len: 0,
        }
    }

    pub fn voxel_size(&self) -> f64 {
        self.voxel_size
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Add a world-frame point. Deduplicates loosely by capping per-voxel density;
    /// once a voxel is full the point is dropped (the voxel already represents that
    /// region). Keeps the map bounded without a global kd-tree rebuild.
    pub fn add_point(&mut self, p: Point3<f32>) {
        let k = [
            (p.x as f64 / self.voxel_size).floor() as i32,
            (p.y as f64 / self.voxel_size).floor() as i32,
            (p.z as f64 / self.voxel_size).floor() as i32,
        ];
        let bucket = self.voxels.entry(k).or_default();
        if bucket.len() < self.max_points_per_voxel {
            bucket.push(p);
            self.len += 1;
        }
    }

    pub fn add_points(&mut self, points: impl IntoIterator<Item = Point3<f32>>) {
        for p in points {
            self.add_point(p);
        }
    }

    /// Nearest map point to `query` within `max_dist`, or `None`. Searches a cube of
    /// voxels wide enough to be COMPLETE for `max_dist` (radius = ceil(max_dist /
    /// voxel_size) cells), so it never misses a closer point in an adjacent voxel.
    pub fn nearest(&self, query: &Point3<f64>, max_dist: f64) -> Option<Point3<f64>> {
        let center = key_of(query, self.voxel_size);
        let radius = (max_dist / self.voxel_size).ceil() as i32;
        let max_sq = max_dist * max_dist;
        let mut best: Option<(f64, Point3<f64>)> = None;
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    let k = [center[0] + dx, center[1] + dy, center[2] + dz];
                    let Some(bucket) = self.voxels.get(&k) else {
                        continue;
                    };
                    for p in bucket {
                        let pd = Point3::new(p.x as f64, p.y as f64, p.z as f64);
                        let d2 = (pd - query).norm_squared();
                        if d2 <= max_sq && best.as_ref().map(|(b, _)| d2 < *b).unwrap_or(true) {
                            best = Some((d2, pd));
                        }
                    }
                }
            }
        }
        best.map(|(_, p)| p)
    }
}
