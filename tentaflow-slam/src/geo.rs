// =============================================================================
// File: geo.rs — WGS84 georeference: scene-local metres → real-world lat/lon/alt.
// Purpose: turn the metric, scene-local SLAM frame into TRUE global coordinates via
// a MANUAL anchor (the operator pins the scene origin to a known lat/lon/alt +
// heading). This is how an indoor robot with NO GNSS gets real-world position
// (UNIVERSAL_POSITIONING_PLAN §5, UNIFIED_SLAM_ARCHITECTURE §6 Georef). Because all
// robots anchored to the same scene then live in ONE world frame, their maps are
// automatically shared — no separate cross-robot alignment for the common case.
//
// Chain: scene (Z-up metres) --heading--> local ENU --anchor--> ECEF --> WGS84.
// All angles in radians internally; the public `GeoAnchor` takes degrees (what a UI
// collects). Standard WGS84 ellipsoid; ECEF↔geodetic via closed-form Bowring.
// =============================================================================

/// WGS84 semi-major axis (metres).
const WGS84_A: f64 = 6_378_137.0;
/// WGS84 flattening.
const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// WGS84 semi-minor axis.
const WGS84_B: f64 = WGS84_A * (1.0 - WGS84_F);
/// First eccentricity squared, e² = f(2−f).
const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F);
/// Second eccentricity squared, e'² = (a²−b²)/b².
const WGS84_EP2: f64 = (WGS84_A * WGS84_A - WGS84_B * WGS84_B) / (WGS84_B * WGS84_B);

/// Geodetic WGS84 (lat°, lon°, alt m) → ECEF metres `[x, y, z]`.
pub fn geodetic_to_ecef(lat_deg: f64, lon_deg: f64, alt_m: f64) -> [f64; 3] {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let (slat, clat) = lat.sin_cos();
    let (slon, clon) = lon.sin_cos();
    let n = WGS84_A / (1.0 - WGS84_E2 * slat * slat).sqrt();
    [
        (n + alt_m) * clat * clon,
        (n + alt_m) * clat * slon,
        (n * (1.0 - WGS84_E2) + alt_m) * slat,
    ]
}

/// ECEF metres → geodetic WGS84 `(lat°, lon°, alt m)`, closed-form (Bowring).
pub fn ecef_to_geodetic(ecef: [f64; 3]) -> (f64, f64, f64) {
    let [x, y, z] = ecef;
    let lon = y.atan2(x);
    let p = (x * x + y * y).sqrt();
    // Near the polar axis `p ≈ 0`: longitude is undefined and latitude is ±90°.
    if p < 1.0e-9 {
        let lat = if z >= 0.0 {
            std::f64::consts::FRAC_PI_2
        } else {
            -std::f64::consts::FRAC_PI_2
        };
        let alt = z.abs() - WGS84_B;
        return (lat.to_degrees(), lon.to_degrees(), alt);
    }
    let theta = (z * WGS84_A).atan2(p * WGS84_B);
    let (st, ct) = theta.sin_cos();
    let lat = (z + WGS84_EP2 * WGS84_B * st * st * st)
        .atan2(p - WGS84_E2 * WGS84_A * ct * ct * ct);
    let slat = lat.sin();
    let n = WGS84_A / (1.0 - WGS84_E2 * slat * slat).sqrt();
    let alt = p / lat.cos() - n;
    (lat.to_degrees(), lon.to_degrees(), alt)
}

/// ENU basis vectors (East, North, Up) expressed in ECEF at a geodetic origin.
fn enu_basis(lat0_deg: f64, lon0_deg: f64) -> ([f64; 3], [f64; 3], [f64; 3]) {
    let (slat, clat) = lat0_deg.to_radians().sin_cos();
    let (slon, clon) = lon0_deg.to_radians().sin_cos();
    let east = [-slon, clon, 0.0];
    let north = [-slat * clon, -slat * slon, clat];
    let up = [clat * clon, clat * slon, slat];
    (east, north, up)
}

/// Geodetic WGS84 → local ENU metres relative to a geodetic origin (the fusion
/// engine's nav frame). Used to fold a GNSS fix into the local filter.
pub fn geodetic_to_enu(
    lat_deg: f64,
    lon_deg: f64,
    alt_m: f64,
    lat0_deg: f64,
    lon0_deg: f64,
    alt0_m: f64,
) -> [f64; 3] {
    let p = geodetic_to_ecef(lat_deg, lon_deg, alt_m);
    let o = geodetic_to_ecef(lat0_deg, lon0_deg, alt0_m);
    let d = [p[0] - o[0], p[1] - o[1], p[2] - o[2]];
    let (e, n, u) = enu_basis(lat0_deg, lon0_deg);
    [
        e[0] * d[0] + e[1] * d[1] + e[2] * d[2],
        n[0] * d[0] + n[1] * d[1] + n[2] * d[2],
        u[0] * d[0] + u[1] * d[1] + u[2] * d[2],
    ]
}

/// Local ENU metres at a geodetic origin → geodetic WGS84 `(lat°, lon°, alt m)`.
/// Inverse of [`geodetic_to_enu`]; turns the engine's ENU pose into a global fix.
pub fn enu_to_geodetic(
    enu: [f64; 3],
    lat0_deg: f64,
    lon0_deg: f64,
    alt0_m: f64,
) -> (f64, f64, f64) {
    let (e, n, u) = enu_basis(lat0_deg, lon0_deg);
    let o = geodetic_to_ecef(lat0_deg, lon0_deg, alt0_m);
    let [de, dn, du] = enu;
    let ecef = [
        o[0] + e[0] * de + n[0] * dn + u[0] * du,
        o[1] + e[1] * de + n[1] * dn + u[1] * du,
        o[2] + e[2] * de + n[2] * dn + u[2] * du,
    ];
    ecef_to_geodetic(ecef)
}

/// A manual georeference: the scene origin pinned to a real-world position + heading.
/// `heading_deg` is the compass BEARING (degrees clockwise from true North) of the
/// scene's +X axis. With heading 0, scene +X points North and scene +Y points West
/// (right-handed, Z-up). This is exactly what a "set map origin" UI collects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoAnchor {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub alt_m: f64,
    pub heading_deg: f64,
}

impl GeoAnchor {
    pub fn new(lat_deg: f64, lon_deg: f64, alt_m: f64, heading_deg: f64) -> Self {
        GeoAnchor { lat_deg, lon_deg, alt_m, heading_deg }
    }

    /// True if every field is finite and the lat/lon are in range — a malformed anchor
    /// must be rejected, never applied (it would map every pose to nonsense).
    pub fn is_valid(&self) -> bool {
        self.lat_deg.is_finite()
            && self.lon_deg.is_finite()
            && self.alt_m.is_finite()
            && self.heading_deg.is_finite()
            && (-90.0..=90.0).contains(&self.lat_deg)
            && (-180.0..=180.0).contains(&self.lon_deg)
    }

    /// Scene-local point (Z-up metres) → local ENU (East, North, Up) metres, applying
    /// the heading rotation about Up. With heading β: scene +X → bearing β, scene +Y →
    /// bearing β−90° (right-handed Z-up). So E,N are the rotated horizontal components.
    fn scene_to_enu(&self, scene: [f64; 3]) -> [f64; 3] {
        let b = self.heading_deg.to_radians();
        let (sb, cb) = b.sin_cos();
        let [x, y, z] = scene;
        [
            x * sb - y * cb, // East
            x * cb + y * sb, // North
            z,               // Up
        ]
    }

    /// Scene-local point (Z-up metres) → ECEF metres, via local ENU at the anchor.
    pub fn scene_to_ecef(&self, scene: [f64; 3]) -> [f64; 3] {
        let [e, n, u] = self.scene_to_enu(scene);
        let lat = self.lat_deg.to_radians();
        let lon = self.lon_deg.to_radians();
        let (slat, clat) = lat.sin_cos();
        let (slon, clon) = lon.sin_cos();
        let origin = geodetic_to_ecef(self.lat_deg, self.lon_deg, self.alt_m);
        // ENU basis expressed in ECEF (columns East, North, Up).
        let east = [-slon, clon, 0.0];
        let north = [-slat * clon, -slat * slon, clat];
        let up = [clat * clon, clat * slon, slat];
        [
            origin[0] + east[0] * e + north[0] * n + up[0] * u,
            origin[1] + east[1] * e + north[1] * n + up[1] * u,
            origin[2] + east[2] * e + north[2] * n + up[2] * u,
        ]
    }

    /// Scene-local point (Z-up metres) → real-world WGS84 `(lat°, lon°, alt m)`.
    pub fn scene_to_wgs84(&self, scene: [f64; 3]) -> (f64, f64, f64) {
        ecef_to_geodetic(self.scene_to_ecef(scene))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn ecef_of_equator_prime_meridian_is_semi_major_axis() {
        let e = geodetic_to_ecef(0.0, 0.0, 0.0);
        assert!(approx(e[0], WGS84_A, 1e-3));
        assert!(approx(e[1], 0.0, 1e-6));
        assert!(approx(e[2], 0.0, 1e-6));
    }

    #[test]
    fn geodetic_ecef_round_trips() {
        for &(lat, lon, alt) in &[
            (52.2297, 21.0122, 118.5),  // Warsaw
            (-33.8688, 151.2093, 58.0), // Sydney
            (0.0, 0.0, 0.0),
            (89.9, 10.0, 500.0), // near pole, off-axis
        ] {
            let (la, lo, al) = ecef_to_geodetic(geodetic_to_ecef(lat, lon, alt));
            assert!(approx(la, lat, 1e-7), "lat {la} vs {lat}");
            assert!(approx(lo, lon, 1e-7), "lon {lo} vs {lon}");
            assert!(approx(al, alt, 1e-3), "alt {al} vs {alt}");
        }
    }

    #[test]
    fn scene_origin_maps_to_anchor_exactly() {
        let a = GeoAnchor::new(52.2297, 21.0122, 118.5, 37.0);
        let (lat, lon, alt) = a.scene_to_wgs84([0.0, 0.0, 0.0]);
        assert!(approx(lat, 52.2297, 1e-9));
        assert!(approx(lon, 21.0122, 1e-9));
        assert!(approx(alt, 118.5, 1e-6));
    }

    #[test]
    fn heading_zero_x_is_north_y_is_west() {
        // Heading 0 → scene +X points North, scene +Y points West. 100 m along +X
        // moves ~100/111320° North with ~unchanged lon; 100 m along +Y moves West
        // (lon DECREASES in the northern hemisphere).
        let a = GeoAnchor::new(52.0, 21.0, 0.0, 0.0);
        let north = a.scene_to_wgs84([100.0, 0.0, 0.0]);
        assert!(north.0 > 52.0, "scene +X increases latitude (north)");
        assert!(approx(north.1, 21.0, 1e-4), "scene +X barely changes lon");
        let west = a.scene_to_wgs84([0.0, 100.0, 0.0]);
        assert!(west.1 < 21.0, "scene +Y decreases lon (west)");
        assert!(approx(west.0, 52.0, 1e-4), "scene +Y barely changes lat");
    }

    #[test]
    fn heading_ninety_rotates_x_to_east() {
        // Heading 90 → scene +X points East. 100 m along +X increases lon.
        let a = GeoAnchor::new(52.0, 21.0, 0.0, 90.0);
        let east = a.scene_to_wgs84([100.0, 0.0, 0.0]);
        assert!(east.1 > 21.0, "with heading 90, scene +X increases lon (east)");
        assert!(approx(east.0, 52.0, 1e-4), "and barely changes lat");
    }

    #[test]
    fn horizontal_distance_is_metric() {
        // 100 m North then back to geodetic should be ~100 m of ground distance: at
        // 52° lat, 1° lat ≈ 111.32 km, so 100 m ≈ 8.98e-4°.
        let a = GeoAnchor::new(52.0, 21.0, 0.0, 0.0);
        let (lat, _, _) = a.scene_to_wgs84([100.0, 0.0, 0.0]);
        let dlat_deg = lat - 52.0;
        assert!(approx(dlat_deg, 100.0 / 111_320.0, 5e-6), "metric north step");
    }

    #[test]
    fn altitude_follows_scene_z() {
        let a = GeoAnchor::new(0.0, 0.0, 100.0, 0.0);
        let up = a.scene_to_wgs84([0.0, 0.0, 25.0]);
        assert!(approx(up.2, 125.0, 1e-3), "scene +Z raises altitude");
    }

    #[test]
    fn enu_geodetic_round_trips() {
        let (lat0, lon0, alt0) = (52.2297, 21.0122, 118.5);
        for enu in [[0.0, 0.0, 0.0], [100.0, -50.0, 10.0], [-1234.0, 5678.0, -20.0]] {
            let (la, lo, al) = enu_to_geodetic(enu, lat0, lon0, alt0);
            let back = geodetic_to_enu(la, lo, al, lat0, lon0, alt0);
            for k in 0..3 {
                assert!((back[k] - enu[k]).abs() < 1e-3, "enu[{k}] {} vs {}", back[k], enu[k]);
            }
        }
    }

    #[test]
    fn geodetic_to_enu_directions() {
        // 100 m East and 100 m North from the origin land on +E / +N respectively.
        let (lat0, lon0, alt0) = (52.0, 21.0, 0.0);
        let (lat_n, lon_n, alt_n) = enu_to_geodetic([0.0, 100.0, 0.0], lat0, lon0, alt0);
        assert!(lat_n > lat0, "+N raises latitude");
        let enu_n = geodetic_to_enu(lat_n, lon_n, alt_n, lat0, lon0, alt0);
        assert!((enu_n[1] - 100.0).abs() < 1e-3 && enu_n[0].abs() < 1e-3);
    }

    #[test]
    fn rejects_out_of_range_or_nonfinite_anchor() {
        assert!(GeoAnchor::new(52.0, 21.0, 0.0, 0.0).is_valid());
        assert!(!GeoAnchor::new(91.0, 21.0, 0.0, 0.0).is_valid());
        assert!(!GeoAnchor::new(52.0, 181.0, 0.0, 0.0).is_valid());
        assert!(!GeoAnchor::new(f64::NAN, 21.0, 0.0, 0.0).is_valid());
    }
}
