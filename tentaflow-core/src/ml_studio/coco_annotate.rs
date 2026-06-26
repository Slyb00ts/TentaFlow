// ===== File: ml_studio/coco_annotate.rs — COCO annotation editor I/O =====
//
// Czyta/zapisuje adnotacje COCO datasetu recognition (katalog coco_path z
// podkatalogami split + `_annotations.coco.json`) na potrzeby edytora anotacji
// w ML Studio: lista obrazów do galerii, pojedynczy obraz przeskalowany do
// wyświetlenia + jego bboxy, zapis edytowanych bboxów z powrotem do COCO.
//
// `image_id` jest SYNTETYCZNY: "split|coco_image_id" — jednoznacznie wskazuje
// split (plik json) i obraz w nim (COCO image id jest unikalne tylko w splicie).

use std::path::{Path, PathBuf};

use base64::Engine;
use serde_json::{json, Value};

/// Maksymalny bok obrazu wysyłanego do przeglądarki (downscale, by zmieścić się
/// w ramce WS 1 MiB). Bboxy edytor trzyma w oryginalnych współrzędnych.
const MAX_DISPLAY_DIM: u32 = 1280;

fn split_image_id(image_id: &str) -> anyhow::Result<(String, i64)> {
    let (split, id) = image_id
        .split_once('|')
        .ok_or_else(|| anyhow::anyhow!("zły image_id (oczekiwano split|id): {}", image_id))?;
    let id: i64 = id
        .parse()
        .map_err(|_| anyhow::anyhow!("zły coco id w image_id: {}", image_id))?;
    Ok((split.to_string(), id))
}

fn annot_path(dir: &Path, split: &str) -> PathBuf {
    dir.join(split).join("_annotations.coco.json")
}

/// Lista wszystkich obrazów datasetu (po splitach) + kategorie. Zwraca
/// (images_json, categories_json).
pub fn list_images(dir: &Path) -> anyhow::Result<(String, String)> {
    if !dir.is_dir() {
        anyhow::bail!("dataset nie jest katalogiem: {}", dir.display());
    }
    let mut images: Vec<Value> = Vec::new();
    let mut categories: Vec<Value> = Vec::new();
    let mut splits: Vec<String> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() && annot_path(dir, p.file_name().and_then(|n| n.to_str()).unwrap_or("")).is_file() {
            splits.push(p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string());
        }
    }
    splits.sort();
    for split in &splits {
        let buf = std::fs::read(annot_path(dir, split))?;
        let coco: Value = serde_json::from_slice(&buf)?;
        if categories.is_empty() {
            if let Some(cats) = coco.get("categories").and_then(|c| c.as_array()) {
                let mut cs: Vec<(i64, String)> = cats
                    .iter()
                    .filter_map(|c| Some((c.get("id")?.as_i64()?, c.get("name")?.as_str()?.to_string())))
                    .collect();
                cs.sort_by_key(|(id, _)| *id);
                categories = cs.into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect();
            }
        }
        // ann_count per image_id.
        let mut counts: std::collections::HashMap<i64, u32> = std::collections::HashMap::new();
        if let Some(anns) = coco.get("annotations").and_then(|a| a.as_array()) {
            for a in anns {
                if let Some(iid) = a.get("image_id").and_then(|v| v.as_i64()) {
                    *counts.entry(iid).or_insert(0) += 1;
                }
            }
        }
        if let Some(imgs) = coco.get("images").and_then(|i| i.as_array()) {
            for im in imgs {
                let id = im.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                images.push(json!({
                    "image_id": format!("{}|{}", split, id),
                    "file_name": im.get("file_name").and_then(|v| v.as_str()).unwrap_or(""),
                    "split": split,
                    "width": im.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                    "height": im.get("height").and_then(|v| v.as_i64()).unwrap_or(0),
                    "ann_count": counts.get(&id).copied().unwrap_or(0),
                }));
            }
        }
    }
    Ok((
        serde_json::to_string(&images)?,
        serde_json::to_string(&categories)?,
    ))
}

/// Pojedynczy obraz (przeskalowany do wyświetlenia, JPEG base64) + jego bboxy w
/// oryginalnych współrzędnych. Zwraca (image_b64, mime, orig_w, orig_h, anns_json).
pub fn get_image(
    dir: &Path,
    image_id: &str,
) -> anyhow::Result<(String, String, u32, u32, String)> {
    let (split, coco_id) = split_image_id(image_id)?;
    let buf = std::fs::read(annot_path(dir, &split))?;
    let coco: Value = serde_json::from_slice(&buf)?;
    let file_name = coco
        .get("images")
        .and_then(|i| i.as_array())
        .and_then(|imgs| {
            imgs.iter()
                .find(|im| im.get("id").and_then(|v| v.as_i64()) == Some(coco_id))
        })
        .and_then(|im| im.get("file_name").and_then(|v| v.as_str()))
        .ok_or_else(|| anyhow::anyhow!("obraz {} nie znaleziony w {}", coco_id, split))?
        .to_string();

    let img_path = dir.join(&split).join(&file_name);
    let dyn_img = image::open(&img_path)
        .map_err(|e| anyhow::anyhow!("nie można wczytać {}: {}", img_path.display(), e))?;
    let (ow, oh) = (
        image::GenericImageView::dimensions(&dyn_img).0,
        image::GenericImageView::dimensions(&dyn_img).1,
    );
    // Downscale do MAX_DISPLAY_DIM (zachowując proporcje) — tylko gdy za duży.
    let scaled = if ow.max(oh) > MAX_DISPLAY_DIM {
        dyn_img.resize(MAX_DISPLAY_DIM, MAX_DISPLAY_DIM, image::imageops::FilterType::Triangle)
    } else {
        dyn_img
    };
    let mut jpeg: Vec<u8> = Vec::new();
    scaled
        .to_rgb8()
        .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .map_err(|e| anyhow::anyhow!("kodowanie JPEG: {}", e))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&jpeg);

    let anns: Vec<Value> = coco
        .get("annotations")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|a| a.get("image_id").and_then(|v| v.as_i64()) == Some(coco_id))
                .filter_map(|a| {
                    let bbox = a.get("bbox")?.as_array()?;
                    let mut out = json!({
                        "id": a.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                        "category_id": a.get("category_id").and_then(|v| v.as_i64()).unwrap_or(0),
                        "bbox": bbox,
                    });
                    // Per-box schema attribute values and the detector confidence are
                    // carried through verbatim so the annotation editor can reload the
                    // attributes panel and render predicted (autolabeled) boxes dashed.
                    if let Some(attrs) = a.get("attributes") {
                        out["attributes"] = attrs.clone();
                    }
                    if let Some(score) = a.get("score") {
                        out["score"] = score.clone();
                    }
                    if let Some(predicted) = a.get("predicted") {
                        out["predicted"] = predicted.clone();
                    }
                    Some(out)
                })
                .collect()
        })
        .unwrap_or_default();

    Ok((
        b64,
        "image/jpeg".to_string(),
        ow,
        oh,
        serde_json::to_string(&anns)?,
    ))
}

/// Zapisuje edytowane bboxy obrazu z powrotem do `_annotations.coco.json` splitu:
/// usuwa stare anotacje tego obrazu, dopisuje nowe (świeże id), reszta bez zmian.
/// Zapis atomowy (temp + rename), by nie uszkodzić pliku przy błędzie.
pub fn save_annotations(dir: &Path, image_id: &str, annotations_json: &str) -> anyhow::Result<()> {
    let (split, coco_id) = split_image_id(image_id)?;
    let path = annot_path(dir, &split);
    let buf = std::fs::read(&path)?;
    let mut coco: Value = serde_json::from_slice(&buf)?;
    let new_anns: Vec<Value> = serde_json::from_str(annotations_json)
        .map_err(|e| anyhow::anyhow!("annotations_json invalid: {}", e))?;

    let arr = coco
        .get_mut("annotations")
        .and_then(|a| a.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("brak pola annotations w COCO"))?;
    // Usuń stare anotacje tego obrazu.
    arr.retain(|a| a.get("image_id").and_then(|v| v.as_i64()) != Some(coco_id));
    // Nowe id startują powyżej maksymalnego istniejącego.
    let mut next_id = arr
        .iter()
        .filter_map(|a| a.get("id").and_then(|v| v.as_i64()))
        .max()
        .unwrap_or(0)
        + 1;
    for a in &new_anns {
        let cat = a.get("category_id").and_then(|v| v.as_i64()).unwrap_or(0);
        let bbox = a.get("bbox").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let w = bbox.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let h = bbox.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let mut ann = json!({
            "id": next_id,
            "image_id": coco_id,
            "category_id": cat,
            "bbox": bbox,
            "area": w * h,
            "iscrowd": 0,
        });
        // Persist per-box schema attribute values and detector confidence when the
        // editor supplies them; absent keys keep the COCO record minimal.
        if let Some(attrs) = a.get("attributes") {
            ann["attributes"] = attrs.clone();
        }
        if let Some(score) = a.get("score") {
            ann["score"] = score.clone();
        }
        if let Some(predicted) = a.get("predicted") {
            ann["predicted"] = predicted.clone();
        }
        arr.push(ann);
        next_id += 1;
    }

    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(&coco)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}
