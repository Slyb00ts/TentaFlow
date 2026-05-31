// =============================================================================
// File: services/camera_ingest/local.rs — GStreamer local camera source builder
// =============================================================================
//
// Maps TentaFlow's stable camera vendors (`local_camera`, `v4l2`) onto the
// platform-specific GStreamer source element and reuses the RGB appsink path
// used by file playback.

use std::sync::Arc;

use super::error::{CameraIngestError, Result};
use super::fakefile::{
    build_pipeline_from_description, FakeFilePipeline, FrameCounters, FrameMailbox,
};
use super::session::CameraConfig;

pub fn build_local_pipeline(
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<FakeFilePipeline> {
    let source = local_source_description(&config.vendor, &config.url)?;
    let caps = local_caps_description(config);
    let desc = format!(
        "{source} ! videoconvert ! videoscale ! {caps} ! appsink name=sink emit-signals=false sync=false max-buffers=1 drop=true"
    );
    build_pipeline_from_description(&desc, config.camera_id.clone(), mailbox, counters)
}

pub fn validate_local_source(vendor: &str, url: &str) -> Result<()> {
    local_source_description(vendor, url).map(|_| ())
}

/// A locally attached camera device suitable for a wizard dropdown:
/// `device_path` is the value to feed back as the camera `url`, `label` is a
/// human-readable name, `vendor` is the matching stable TentaFlow vendor.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalCameraDevice {
    pub device_path: String,
    pub label: String,
    pub vendor: String,
}

/// Enumerate locally attached camera devices.
///
/// Enumeration is platform-specific and currently implemented only for Linux
/// via the v4l2 sysfs tree (`/sys/class/video4linux`). On every other platform
/// there is no dependency-free way to list devices, so the result is an empty
/// list rather than a fabricated one — callers treat that as "no enumeration on
/// this platform", not as an error.
pub fn list_local_devices() -> Vec<LocalCameraDevice> {
    #[cfg(target_os = "linux")]
    {
        list_v4l2_devices()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// Read the v4l2 sysfs tree and return one entry per `videoN` node that exposes
/// a readable `name`. Nodes are sorted by their numeric index so the dropdown is
/// stable across calls. Missing or unreadable `name` files are skipped — a v4l2
/// node without a name is typically a metadata-only sub-device we cannot ingest.
#[cfg(target_os = "linux")]
fn list_v4l2_devices() -> Vec<LocalCameraDevice> {
    let mut indexed: Vec<(u32, LocalCameraDevice)> = Vec::new();
    let entries = match std::fs::read_dir("/sys/class/video4linux") {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let node = match file_name.to_str() {
            Some(n) => n,
            None => continue,
        };
        let index: u32 = match node.strip_prefix("video").and_then(|n| n.parse().ok()) {
            Some(i) => i,
            None => continue,
        };
        let name_path = entry.path().join("name");
        let label = match std::fs::read_to_string(&name_path) {
            Ok(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    continue;
                }
                trimmed.to_string()
            }
            Err(_) => continue,
        };
        indexed.push((
            index,
            LocalCameraDevice {
                device_path: format!("/dev/video{index}"),
                label,
                vendor: "v4l2".to_string(),
            },
        ));
    }
    indexed.sort_by_key(|(index, _)| *index);
    indexed.into_iter().map(|(_, device)| device).collect()
}

fn local_caps_description(config: &CameraConfig) -> String {
    let mut caps = String::from("video/x-raw,format=RGB");
    if let Some((width, height)) = config.resolution {
        caps.push_str(&format!(",width={width},height={height}"));
    }
    if config.target_fps > 0 {
        caps.push_str(&format!(",framerate={}/1", config.target_fps));
    }
    caps
}

fn local_source_description(vendor: &str, url: &str) -> Result<String> {
    match vendor {
        "v4l2" => v4l2_source(url),
        "local_camera" => platform_local_source(url),
        other => Err(CameraIngestError::UnsupportedVendor(other.to_string())),
    }
}

fn platform_local_source(url: &str) -> Result<String> {
    if cfg!(target_os = "linux") {
        return v4l2_source(if url.is_empty() { "/dev/video0" } else { url });
    }
    if cfg!(target_os = "macos") || cfg!(target_os = "ios") {
        return Ok(avfoundation_source(url));
    }
    if cfg!(target_os = "windows") {
        return Ok(windows_media_source(url));
    }
    if cfg!(target_os = "android") {
        if !url.is_empty() {
            return Err(CameraIngestError::InvalidUrl(
                "android local_camera does not accept device path".into(),
            ));
        }
        return Ok("ahcsrc".to_string());
    }
    Err(CameraIngestError::UnsupportedVendor(format!(
        "local_camera on {}",
        std::env::consts::OS
    )))
}

fn v4l2_source(url: &str) -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err(CameraIngestError::UnsupportedVendor(
            "v4l2 is available only on Linux".into(),
        ));
    }
    let device = if url.is_empty() {
        "/dev/video0"
    } else {
        url.strip_prefix("v4l2://").unwrap_or(url)
    };
    if !device.starts_with("/dev/video") {
        return Err(CameraIngestError::InvalidUrl(format!(
            "v4l2 device must be /dev/video*, got {device}"
        )));
    }
    Ok(format!("v4l2src device=\"{}\"", escape_gst_string(device)))
}

fn avfoundation_source(url: &str) -> String {
    if let Ok(index) = url.parse::<i32>() {
        format!("avfvideosrc device-index={index}")
    } else {
        "avfvideosrc".to_string()
    }
}

fn windows_media_source(url: &str) -> String {
    if url.is_empty() {
        "mfvideosrc".to_string()
    } else {
        format!("mfvideosrc device-path=\"{}\"", escape_gst_string(url))
    }
}

fn escape_gst_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4l2_rejects_non_device_path() {
        let err = v4l2_source("/tmp/video0").unwrap_err();
        assert!(matches!(err, CameraIngestError::InvalidUrl(_)));
    }

    #[test]
    fn list_local_devices_does_not_panic_without_hardware() {
        // On a machine with no cameras (or non-Linux) this must return cleanly
        // with an empty list rather than panicking. Every entry it does return
        // must be a well-formed device path.
        let devices = list_local_devices();
        for device in &devices {
            assert!(!device.device_path.is_empty());
            assert!(!device.label.is_empty());
            assert!(!device.vendor.is_empty());
        }
        #[cfg(target_os = "linux")]
        for device in &devices {
            assert!(device.device_path.starts_with("/dev/video"));
            assert_eq!(device.vendor, "v4l2");
        }
    }

    #[test]
    fn local_camera_accepts_default_source() {
        let source = platform_local_source("");
        assert!(
            source.is_ok()
                || matches!(source.unwrap_err(), CameraIngestError::UnsupportedVendor(_))
        );
    }
}
