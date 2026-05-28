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
    fn local_camera_accepts_default_source() {
        let source = platform_local_source("");
        assert!(
            source.is_ok()
                || matches!(source.unwrap_err(), CameraIngestError::UnsupportedVendor(_))
        );
    }
}
