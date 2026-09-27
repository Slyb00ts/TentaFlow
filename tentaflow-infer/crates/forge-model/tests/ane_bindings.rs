// ===== File: ane_bindings.rs — lista wiązań projekcji dla ramienia ANE, na Bieliku =====
//
// `projection_bindings` jest tym, co strona CoreML dostaje zamiast dostępu do
// wag: indeks, rola i kształt. Kształt jest tu sprawdzany na PRAWDZIWYM
// checkpoincie, bo to on decyduje, czy model CoreML o stałym ogonie pasuje —
// a zły kształt z tej listy nie objawiłby się błędem, tylko innym modelem.

#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::collections::HashSet;
use std::sync::Arc;

use forge_hal::metal_device::MetalDevice;
use forge_kernels::MetalExec;
use forge_model::dense::{AneProj, Dense};

const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

fn checkpoint() -> Option<std::path::PathBuf> {
    let dir = std::fs::read_dir(CHECKPOINT).ok()?.flatten().next()?.path();
    dir.join("model.safetensors").is_file().then_some(dir)
}

#[test]
#[ignore]
fn every_layer_of_bielik_binds_seven_projections_with_their_real_shapes() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let model = Dense::load(&dir, |spec| MetalExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie Bielika");
    let shape = model.shape();
    let bindings = model.projection_bindings();

    // Bielik-Minitron-7B ma 40 warstw (nie 32 jak Llama-7B) — liczba wpisów
    // wynika z checkpointu, a nie z założenia.
    let layers = shape.layers;
    assert_eq!(
        layers, 40,
        "to nie jest checkpoint, którego kształty tu pinujemy"
    );
    assert_eq!(
        bindings.len(),
        layers as usize * 7,
        "siedem projekcji na każdą z {layers} warstw"
    );

    let want = |proj: AneProj| -> (u32, u32) {
        match proj {
            AneProj::Q => (4096, 4096),
            AneProj::K | AneProj::V => (1024, 4096),
            AneProj::O => (4096, 4096),
            AneProj::Gate | AneProj::Up => (11264, 4096),
            AneProj::Down => (4096, 11264),
        }
    };
    let mut seen = HashSet::new();
    let mut ids = HashSet::new();
    for b in &bindings {
        assert!(b.layer < layers, "{b:?}");
        assert_eq!((b.rows, b.cols), want(b.proj), "{b:?}");
        assert!(seen.insert((b.layer, b.proj)), "{b:?} występuje dwa razy");
        assert!(
            ids.insert(b.id.0),
            "{b:?}: ten sam indeks wagi w dwóch wpisach"
        );
    }
    // Każda warstwa ma komplet, a nie „średnio siedem".
    for layer in 0..layers {
        for proj in [
            AneProj::Q,
            AneProj::K,
            AneProj::V,
            AneProj::O,
            AneProj::Gate,
            AneProj::Up,
            AneProj::Down,
        ] {
            assert!(
                seen.contains(&(layer, proj)),
                "warstwa {layer} bez {proj:?}"
            );
        }
    }
}
