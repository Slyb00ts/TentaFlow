// =============================================================================
// File: examples/adr_rowtrim_ab.rs — A/B row content-trim on real ADR crops
// =============================================================================
//
// Measures the `[vision] adr_row_trim` row content-trim (adr_ocr) on REAL
// captured placard crops. For every `adr_*_raw.png` in the dump dir it runs
// `AdrOcr::read_adr` twice — trim OFF then trim ON — and reports per-crop
// (kemler, un) both ways, how many reads CHANGED, and how many land on the known
// ground truth for this batch (kemler == "99", un == "3257"). The captured batch
// is a single "99"/"3257" placard, so those counts are the arbiter of the fix.
//
//   cargo run --release \
//     --features inference-vision-gpu,inference-supertonic \
//     --example adr_rowtrim_ab -- [DUMP_DIR]
//
// DUMP_DIR defaults to `.runtime/ocr-dumps`. Only `*_raw.png` (full placard RGB)
// crops are used; `*_tensor.png` previews are skipped.
#![cfg(feature = "inference-vision-gpu")]

use anyhow::Result;

use tentaflow_core::paths;
use tentaflow_core::vision::adr_ocr::AdrOcr;

// Ground truth for the captured batch (a single orange placard).
const GT_KEMLER: &str = "99";
const GT_UN: &str = "3257";

fn read_with_trim(engine: &AdrOcr, rgb: &[u8], w: u32, h: u32, trim: bool) -> (String, String) {
    // adr_ocr reads the trim flag fresh per call, so flipping the bench-only
    // programmatic override flips the behavior between the two arms.
    tentaflow_core::vision::adr_ocr::set_row_trim_override(trim);
    engine
        .read_adr(rgb, w, h)
        .unwrap_or_else(|| (String::new(), String::new()))
}

fn main() -> Result<()> {
    // Default vision settings: crucially `ocr_dump_dir = None`, so we never
    // re-dump into the folder we are iterating (would spawn new crops).
    tentaflow_core::vision::settings::init(Default::default())?;

    let dump_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".runtime/ocr-dumps".to_string());

    let dir = paths::vision_models_dir();
    println!("AdrOcr from: {}", dir.display());
    let engine = AdrOcr::from_dir(&dir)?;
    println!("model loaded. dump dir: {dump_dir}\n");

    let mut crops: Vec<std::path::PathBuf> = std::fs::read_dir(&dump_dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("adr_") && n.ends_with("_raw.png"))
                .unwrap_or(false)
        })
        .collect();
    crops.sort();
    if crops.is_empty() {
        println!("No adr_*_raw.png crops in {dump_dir}");
        return Ok(());
    }

    let (mut off_gt, mut on_gt) = (0usize, 0usize);
    let (mut off_un_ok, mut on_un_ok) = (0usize, 0usize);
    let mut changed = 0usize;
    let mut improved = 0usize; // OFF wrong -> ON exact GT
    let mut regressed = 0usize; // OFF exact GT -> ON wrong
    let total = crops.len();

    for path in &crops {
        let img = image::open(path)?.to_rgb8();
        let (w, h) = img.dimensions();
        let (ok, ou) = read_with_trim(&engine, img.as_raw(), w, h, false);
        let (nk, nu) = read_with_trim(&engine, img.as_raw(), w, h, true);

        let off_hit = ok == GT_KEMLER && ou == GT_UN;
        let on_hit = nk == GT_KEMLER && nu == GT_UN;
        if off_hit {
            off_gt += 1;
        }
        if on_hit {
            on_gt += 1;
        }
        if ou == GT_UN {
            off_un_ok += 1;
        }
        if nu == GT_UN {
            on_un_ok += 1;
        }
        let did_change = ok != nk || ou != nu;
        if did_change {
            changed += 1;
        }
        if !off_hit && on_hit {
            improved += 1;
        }
        if off_hit && !on_hit {
            regressed += 1;
        }

        let name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let flag = if did_change { "CHANGED" } else { "same" };
        println!(
            "{name}: OFF=({ok:>4},{ou:>5})  ON=({nk:>4},{nu:>5})  {flag}{}{}",
            if !off_hit && on_hit { " +GT" } else { "" },
            if off_hit && !on_hit { " -GT" } else { "" },
        );
    }

    println!("\n== A/B on {total} real crops (GT kemler=\"{GT_KEMLER}\" un=\"{GT_UN}\") ==");
    println!("changed reads : {changed}/{total}");
    println!("exact GT (kemler & un):  OFF {off_gt}/{total}  ->  ON {on_gt}/{total}");
    println!("un == \"{GT_UN}\" (bottom row): OFF {off_un_ok}/{total}  ->  ON {on_un_ok}/{total}");
    println!("improved (OFF wrong -> ON GT): {improved}");
    println!("regressed (OFF GT -> ON wrong): {regressed}");
    if off_un_ok > on_un_ok {
        println!(
            "WARNING: bottom-row regression — un==\"{GT_UN}\" dropped {} with trim ON",
            off_un_ok - on_un_ok
        );
    }
    Ok(())
}
