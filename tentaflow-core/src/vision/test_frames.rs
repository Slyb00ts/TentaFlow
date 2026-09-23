// =============================================================================
// File: vision/test_frames.rs — real-image NV12 fixtures for the GPU detector tests
// =============================================================================
//
// The COCO and YuNet GPU tests feed the SAME preprocess the privacy probe
// uses (`preprocess_nv12_device_into` on an NV12 surface in CUDA memory), so
// they need the reference photo as a device NV12 frame. The photo is resized
// to the 1280×720 geometry the Python reference was measured on, converted to
// full-range BT.601 NV12 on the host (the inverse of the kernel's own formula,
// so the round trip only loses the 4:2:0 chroma detail a camera also loses)
// and uploaded with plain `cudaMalloc`/`cudaMemcpy`.

#![cfg(all(test, feature = "inference-vision-gpu", feature = "vision-ort"))]

/// Frame geometry of the Python reference detections.
const W: u32 = 1280;
const H: u32 = 720;

/// Loads `TENTAFLOW_YOLO_TEST_IMAGE` resized to 1280×720 RGB24.
pub(crate) fn bus_rgb_1280x720() -> Vec<u8> {
    let path = std::env::var("TENTAFLOW_YOLO_TEST_IMAGE").expect("TENTAFLOW_YOLO_TEST_IMAGE");
    let img = image::open(&path).expect("decode test image").to_rgb8();
    let (sw, sh) = img.dimensions();
    crate::vision::resize::resize_rgb(img.as_raw(), sw, sh, W, H).expect("resize")
}

#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-cuda-preprocess"
))]
pub(crate) use device::*;

#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    feature = "vision-cuda-preprocess"
))]
mod device {
    use std::os::raw::{c_int, c_void};

    use super::{bus_rgb_1280x720, H, W};
    use crate::vision::gpu_preprocess::{ColorCoeffs, Nv12DevicePlanes};

    extern "C" {
        fn cudaMalloc(dev_ptr: *mut *mut c_void, size: usize) -> c_int;
        fn cudaFree(dev_ptr: *mut c_void) -> c_int;
        fn cudaMemcpy(dst: *mut c_void, src: *const c_void, count: usize, kind: c_int) -> c_int;
    }
    const HOST_TO_DEVICE: c_int = 1;

    /// Host NV12 frame with tightly packed planes (`stride == w`).
    pub(crate) struct HostNv12 {
        pub y: Vec<u8>,
        pub uv: Vec<u8>,
        pub w: u32,
        pub h: u32,
        pub color: ColorCoeffs,
    }

    /// The reference photo as RGB24 plus its NV12 conversion.
    pub(crate) fn bus_frame_1280x720() -> (Vec<u8>, HostNv12) {
        let rgb = bus_rgb_1280x720();
        let nv12 = rgb_to_nv12(&rgb, W, H);
        (rgb, nv12)
    }

    /// Full-range BT.601 RGB→NV12: per-pixel luma, 2×2-averaged chroma.
    fn rgb_to_nv12(rgb: &[u8], w: u32, h: u32) -> HostNv12 {
        let color = ColorCoeffs {
            kr: 0.299,
            kb: 0.114,
            full_range: true,
        };
        let (kr, kb) = (color.kr, color.kb);
        let kg = 1.0 - kr - kb;
        let (w, h) = (w as usize, h as usize);
        let px = |x: usize, y: usize| {
            let p = &rgb[(y * w + x) * 3..(y * w + x) * 3 + 3];
            (p[0] as f32, p[1] as f32, p[2] as f32)
        };
        let luma = |(r, g, b): (f32, f32, f32)| kr * r + kg * g + kb * b;
        let mut y_plane = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                y_plane[y * w + x] = luma(px(x, y)).round().clamp(0.0, 255.0) as u8;
            }
        }
        let mut uv = vec![0u8; w * h / 2];
        for cy in 0..h / 2 {
            for cx in 0..w / 2 {
                let (mut u, mut v) = (0.0f32, 0.0f32);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = px(cx * 2 + dx, cy * 2 + dy);
                    let l = luma(p);
                    u += (p.2 - l) / (2.0 * (1.0 - kb));
                    v += (p.0 - l) / (2.0 * (1.0 - kr));
                }
                let at = cy * w + cx * 2;
                uv[at] = (u / 4.0 + 128.0).round().clamp(0.0, 255.0) as u8;
                uv[at + 1] = (v / 4.0 + 128.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        HostNv12 {
            y: y_plane,
            uv,
            w: w as u32,
            h: h as u32,
            color,
        }
    }

    /// An NV12 frame uploaded to CUDA device 0, freed on drop.
    pub(crate) struct DeviceNv12 {
        y: *mut c_void,
        uv: *mut c_void,
        w: u32,
        h: u32,
    }

    impl DeviceNv12 {
        pub(crate) fn upload(frame: &HostNv12) -> Self {
            let copy = |bytes: &[u8]| {
                let mut dev: *mut c_void = std::ptr::null_mut();
                assert_eq!(
                    unsafe { cudaMalloc(&mut dev, bytes.len()) },
                    0,
                    "cudaMalloc"
                );
                assert_eq!(
                    unsafe { cudaMemcpy(dev, bytes.as_ptr().cast(), bytes.len(), HOST_TO_DEVICE) },
                    0,
                    "cudaMemcpy"
                );
                dev
            };
            Self {
                y: copy(&frame.y),
                uv: copy(&frame.uv),
                w: frame.w,
                h: frame.h,
            }
        }

        pub(crate) fn planes(&self) -> Nv12DevicePlanes {
            Nv12DevicePlanes {
                y_ptr: self.y as u64,
                y_stride: self.w as usize,
                uv_ptr: self.uv as u64,
                uv_stride: self.w as usize,
                w: self.w,
                h: self.h,
            }
        }
    }

    impl Drop for DeviceNv12 {
        fn drop(&mut self) {
            unsafe {
                cudaFree(self.y);
                cudaFree(self.uv);
            }
        }
    }
}
