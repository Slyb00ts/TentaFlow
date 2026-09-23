// =============================================================================
// File: services/camera_ingest/privacy_options.rs — per-camera privacy options
// =============================================================================
//
// What the GPU privacy probe (`privacy.rs`) does to a camera's frames before
// any branch — live view, recording, snapshot, depth, analysis — sees them.
// Always compiled (unlike the probe): the options travel through the camera
// config and the addon settings on every host, and a host without the GPU path
// must refuse a camera that asks for them rather than silently show it unblurred.
//
// Robot cameras take their options from the OWNING ADDON's config, not from the
// `cameras` row: a WebRTC camera row is deleted on restart and every reconnect
// registers a fresh `camera_id`, so a per-row setting would be lost on the
// next reconnect. The addon config outlives every camera the robot registers.

use std::sync::OnceLock;

use dashmap::DashMap;

use crate::db::DbPool;

/// Addon config key: live person detection published to the detection bus.
pub const CONFIG_PERSON_DETECT: &str = "privacy_person_detect";
/// Addon config key: irreversible anonymization of every person's head.
pub const CONFIG_FACE_BLUR: &str = "privacy_face_blur";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivacyOptions {
    pub person_detect: bool,
    pub face_blur: bool,
}

impl PrivacyOptions {
    /// No privacy processing: the frame path is exactly what it was before
    /// privacy options existed.
    pub const OFF: Self = Self {
        person_detect: false,
        face_blur: false,
    };

    /// Options for a robot camera that has never been configured: faces are
    /// blurred, detections are not published (an explicit user decision —
    /// anonymization is on unless an admin turns it off).
    pub const ROBOT_DEFAULT: Self = Self {
        person_detect: false,
        face_blur: true,
    };

    /// True when the frame path must run through the privacy probe at all.
    pub fn active(&self) -> bool {
        self.person_detect || self.face_blur
    }

    /// Read the options of `addon_id` from its config; a key never set keeps the
    /// robot default. Anything other than "true"/"false" is a config error — the
    /// caller refuses to start the camera instead of guessing which way it goes.
    pub fn for_robot_addon(db: &DbPool, addon_id: &str) -> anyhow::Result<Self> {
        let mut options = Self::ROBOT_DEFAULT;
        for row in crate::db::repository::list_addon_config_rows(db, addon_id)? {
            let slot = match row.key.as_str() {
                CONFIG_PERSON_DETECT => &mut options.person_detect,
                CONFIG_FACE_BLUR => &mut options.face_blur,
                _ => continue,
            };
            *slot = parse_flag(&row.key, &row.value)?;
        }
        Ok(options)
    }
}

/// Cameras whose running session applies privacy processing. The probe is then
/// the ONLY detection publisher for the camera: the generic analysis loop must
/// not start on it (it would publish unanonymized-context detections of its own
/// and compete for the same overlay).
fn active_sessions() -> &'static DashMap<String, PrivacyOptions> {
    static ACTIVE: OnceLock<DashMap<String, PrivacyOptions>> = OnceLock::new();
    ACTIVE.get_or_init(DashMap::new)
}

/// The privacy options of a running camera session, `None` when the camera is
/// not running or runs without privacy processing.
pub fn active_for(camera_id: &str) -> Option<PrivacyOptions> {
    active_sessions().get(camera_id).map(|e| *e)
}

/// Marks a camera session as privacy-processed for exactly as long as the guard
/// lives, so every exit path of the session (stop, error, EOS) clears it.
pub struct ActivePrivacyGuard {
    camera_id: String,
}

impl ActivePrivacyGuard {
    /// `None` when `options` is off — nothing to register.
    pub fn register(camera_id: &str, options: PrivacyOptions) -> Option<Self> {
        if !options.active() {
            return None;
        }
        active_sessions().insert(camera_id.to_string(), options);
        Some(Self {
            camera_id: camera_id.to_string(),
        })
    }
}

impl Drop for ActivePrivacyGuard {
    fn drop(&mut self) {
        active_sessions().remove(&self.camera_id);
    }
}

fn parse_flag(key: &str, value: &str) -> anyhow::Result<bool> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => anyhow::bail!("addon config '{key}' must be true or false, got '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_strictly() {
        assert!(parse_flag("k", "true").unwrap());
        assert!(!parse_flag("k", " false ").unwrap());
        assert!(parse_flag("k", "yes").is_err());
        assert!(parse_flag("k", "").is_err());
    }

    #[test]
    fn robot_default_blurs_without_publishing() {
        assert!(PrivacyOptions::ROBOT_DEFAULT.face_blur);
        assert!(!PrivacyOptions::ROBOT_DEFAULT.person_detect);
        assert!(PrivacyOptions::ROBOT_DEFAULT.active());
        assert!(!PrivacyOptions::OFF.active());
    }

    #[test]
    fn guard_scopes_the_registration() {
        assert!(ActivePrivacyGuard::register("cam-off", PrivacyOptions::OFF).is_none());
        assert!(active_for("cam-off").is_none());
        {
            let _g = ActivePrivacyGuard::register("cam-on", PrivacyOptions::ROBOT_DEFAULT);
            assert_eq!(active_for("cam-on"), Some(PrivacyOptions::ROBOT_DEFAULT));
        }
        assert!(active_for("cam-on").is_none());
    }
}
