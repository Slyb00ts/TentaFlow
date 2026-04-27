// =============================================================================
// Plik: vision/preprocessing.rs
// Opis: Wspolne preprocessory dla modeli vision: letterbox (resize z zachowaniem
//       aspectu + pad na kwadrat), normalize (mean/std lub `(x-127.5)/128.0`),
//       konwersja do NCHW f32 (1, 3, H, W).
//
//       Optymalizacje:
//         - resize bilinear przez `image::imageops` (SIMD na x86_64 / NEON
//           na ARM przez crate `image` 0.25)
//         - bezposrednio operujemy na buforze `Vec<f32>` zamiast tworzyc
//           posrednie struktury — dla 640x640x3 to ~5 MB allocacji i kazda
//           kopia jest droga w hot pathu meeting-bota
// =============================================================================

use image::{imageops::FilterType, Rgb, RgbImage};

/// Skala + padding do letterboxa. Zwraca `(scale, pad_x, pad_y)` zeby caller
/// mogl odwzorowac wspolrzedne detekcji z powrotem na oryginalny obrazek.
#[derive(Debug, Clone, Copy)]
pub struct LetterboxMeta {
    pub scale: f32,
    pub pad_x: u32,
    pub pad_y: u32,
}

/// Resize z zachowaniem aspectu + pad do kwadratu o boku `target`. Pad
/// wypelniony `fill` (typowo (114,114,114) dla YOLO, (0,0,0) lub (128,128,128)
/// dla SCRFD). Zwraca obrazek `target x target` + meta do reverse mapping.
pub fn letterbox(src: &RgbImage, target: u32, fill: [u8; 3]) -> (RgbImage, LetterboxMeta) {
    let (sw, sh) = src.dimensions();
    let scale = (target as f32 / sw as f32).min(target as f32 / sh as f32);
    let nw = (sw as f32 * scale).round() as u32;
    let nh = (sh as f32 * scale).round() as u32;
    let pad_x = (target - nw) / 2;
    let pad_y = (target - nh) / 2;

    let resized = image::imageops::resize(src, nw, nh, FilterType::Triangle);
    let mut canvas = RgbImage::from_pixel(target, target, Rgb(fill));
    image::imageops::overlay(&mut canvas, &resized, pad_x as i64, pad_y as i64);

    (
        canvas,
        LetterboxMeta {
            scale,
            pad_x,
            pad_y,
        },
    )
}

/// Mapuje wspolrzedna detekcji z letterboxa z powrotem na oryginalny obrazek.
/// `(x, y)` w pikselach `target x target` → `(x', y')` w pikselach src.
#[inline]
pub fn unletterbox_xy(x: f32, y: f32, meta: &LetterboxMeta) -> (f32, f32) {
    ((x - meta.pad_x as f32) / meta.scale, (y - meta.pad_y as f32) / meta.scale)
}

/// `RgbImage` → NCHW f32 z normalizacja `(pixel - mean) / std`. Buf to
/// `Vec<f32>` o rozmiarze `1 * 3 * H * W`. Czytamy CHW: najpierw cały
/// kanał R, potem G, potem B (tak ONNX przyjmuje w 99% case'ow).
pub fn rgb_to_nchw_normalized(
    img: &RgbImage,
    mean: [f32; 3],
    std: [f32; 3],
) -> Vec<f32> {
    let (w, h) = img.dimensions();
    let plane = (w * h) as usize;
    let mut buf = vec![0f32; plane * 3];
    for (i, pixel) in img.pixels().enumerate() {
        let r = (pixel[0] as f32 - mean[0]) / std[0];
        let g = (pixel[1] as f32 - mean[1]) / std[1];
        let b = (pixel[2] as f32 - mean[2]) / std[2];
        buf[i] = r;
        buf[plane + i] = g;
        buf[2 * plane + i] = b;
    }
    buf
}

/// Wariant SCRFD/InsightFace: `(pixel - 127.5) / 128.0` na kazdym kanale.
pub fn rgb_to_nchw_scrfd(img: &RgbImage) -> Vec<f32> {
    rgb_to_nchw_normalized(img, [127.5, 127.5, 127.5], [128.0, 128.0, 128.0])
}

/// Wariant ImageNet: mean=[0.485, 0.456, 0.406] * 255, std=[0.229, 0.224, 0.225] * 255.
/// Uzywany przez wiele klasyfikatorow (MiVOLO/GoogLeNet, HSEmotion).
pub fn rgb_to_nchw_imagenet(img: &RgbImage) -> Vec<f32> {
    rgb_to_nchw_normalized(
        img,
        [0.485 * 255.0, 0.456 * 255.0, 0.406 * 255.0],
        [0.229 * 255.0, 0.224 * 255.0, 0.225 * 255.0],
    )
}

/// `&[u8]` (RGB row-major) → `RgbImage`. Konstrukcja zero-copy nie jest mozliwa
/// (image crate wymaga ImageBuffer<Rgb<u8>, Vec<u8>>) wiec robimy `to_vec()`.
pub fn rgb_buf_to_image(rgb: &[u8], width: u32, height: u32) -> Option<RgbImage> {
    RgbImage::from_raw(width, height, rgb.to_vec())
}
