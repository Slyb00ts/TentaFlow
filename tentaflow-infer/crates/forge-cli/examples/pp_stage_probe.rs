// Pierwszy token policzony na WIELU etapach: model pociety po warstwach, kazdy
// etap na swojej karcie, rezydual przekazywany miedzy nimi.
//
// Kryterium jest zgodnosc z przebiegiem jednokartowym, nie predkosc — dlatego
// probe liczy ten sam prompt caly na jednej karcie i porownuje go z wynikiem
// zlozonym z etapow.
//
//   PP_DEVICES=0,1,0   indeksy kart kolejnych etapow (dowolna liczba etapow)
//   PP_REF_GIB=6       pula wag odniesienia (caly model na jednej karcie)
//   PP_WEIGHTS_GIB=2   pula wag JEDNEGO etapu
use forge_engine::model::{Model, ModelConfig};
use forge_hal::{PoolSizes, gpu};

fn pools(var: &str, default_gib: usize) -> PoolSizes {
    PoolSizes {
        weights: std::env::var(var)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(default_gib)
            << 30,
        kv_cache: 512 << 20,
        activations: 512 << 20,
        kv_page_size: 256 << 10,
    }
}

fn argmax(v: &[f32]) -> usize {
    let mut best = 0usize;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    best
}

/// Dzieli `total` warstw na `stages` etapow. Reszta z dzielenia idzie do
/// pierwszych etapow, zeby zaden nie zostal pusty.
fn layer_ranges(total: usize, stages: usize) -> Vec<(usize, usize)> {
    let base = total / stages;
    let extra = total % stages;
    let mut out = Vec::with_capacity(stages);
    let mut first = 0usize;
    for i in 0..stages {
        let count = base + usize::from(i < extra);
        out.push((first, count));
        first += count;
    }
    out
}

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("sciezka do gguf"));
    // Tokeny podajemy wprost, zeby probe nie zalezal od tokenizera modelu.
    let tokens: Vec<u32> = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "1,4222,349,272,4304,302,6620,28804".into())
        .split(',')
        .map(|s| s.trim().parse().expect("token"))
        .collect();
    let ids = gpu::enumerate();
    let pick: Vec<usize> = std::env::var("PP_DEVICES")
        .unwrap_or_else(|_| "0,1".into())
        .split(',')
        .map(|s| s.trim().parse().expect("indeks karty"))
        .collect();
    assert!(pick.len() >= 2, "pipeline potrzebuje co najmniej dwoch etapow");
    assert!(
        pick.iter().all(|&d| d < ids.len()),
        "wskazano karte, ktorej nie ma"
    );

    let cfg = |range| ModelConfig {
        max_seq_len: 512,
        kv_pages: 16,
        prefix_cache: false,
        layer_range: range,
        ..ModelConfig::default()
    };

    // Odniesienie: caly model na jednej karcie.
    let mut whole = Model::load_gguf(
        gpu::open_id(ids[pick[0]], pools("PP_REF_GIB", 10)).unwrap(),
        &path,
        cfg(None),
    )
    .expect("wczytanie calego modelu");
    // Liczbe warstw bierzemy z modelu, a nie z argumentu: zle podana recznie
    // dawalaby etapy pokrywajace tylko czesc modelu i falszywy rozjazd.
    let total = whole.weights.layers.len();
    let ranges = layer_ranges(total, pick.len());
    println!("model ma {total} warstw, etapy {ranges:?}");

    // Droga etapowa idzie PIERWSZA, na swiezym modelu: gdyby dzialala tylko po
    // wczesniejszym `prefill_chunk`, znaczyloby to, ze zalezy od stanu
    // zostawionego przez tamta sciezke.
    let mut seq_head = whole.new_seq();
    let rows_head = whole
        .prefill_stage(&mut seq_head, &tokens)
        .expect("prefill etapowy calego modelu");
    let head_only = whole
        .stage_logits(rows_head - 1)
        .expect("glowa etapowa calego modelu");
    whole.release_seq(&mut seq_head);
    let mut seq = whole.new_seq();
    let reference = whole
        .prefill_chunk(&mut seq, &tokens)
        .expect("prefill odniesienia");
    whole.release_seq(&mut seq);
    let head_diff = reference
        .iter()
        .zip(head_only.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert_eq!(
        argmax(&head_only),
        argmax(&reference),
        "sama glowa etapowa rozjechala sie z pelnym prefillem"
    );
    println!("sama glowa etapowa na calym modelu: max |roznica| {head_diff:.6}");
    drop(whole);
    println!(
        "odniesienie na jednej karcie: token {} z {} logitow",
        argmax(&reference),
        reference.len()
    );

    let mut stages: Vec<Model> = Vec::with_capacity(pick.len());
    for (index, &(first, count)) in ranges.iter().enumerate() {
        let device = gpu::open_id(ids[pick[index]], pools("PP_WEIGHTS_GIB", 4)).expect("otwarcie karty");
        let name = device.caps().name.clone();
        let model =
            Model::load_gguf(device, &path, cfg(Some((first, count)))).expect("wczytanie etapu");
        println!("etap {index} na {name}: warstwy {first}..{}", first + count);
        stages.push(model);
    }

    let hidden = stages[0].weights.descriptor.params.hidden_size;
    let mut seqs: Vec<_> = stages.iter().map(|m| m.new_seq()).collect();
    let mut boundary: Vec<u8> = Vec::new();
    let mut rows = 0usize;

    for index in 0..stages.len() {
        if index > 0 {
            // Granica etapu: rezydual [rows, hidden] w f16. Pierwsze przejscie
            // idzie przez hosta — chodzi o zgodnosc wyniku; P2P wchodzi po niej.
            stages[index].ensure_stage_buffers().expect("bufory etapu");
            let target = stages[index].stage_hidden().expect("granica wejscia");
            stages[index]
                .device
                .write(&boundary, target, 0)
                .expect("zapis granicy");
        }
        rows = stages[index]
            .prefill_stage(&mut seqs[index], &tokens)
            .expect("etap");
        if index + 1 < stages.len() {
            boundary.resize(rows * hidden * 2, 0);
            let source = stages[index].stage_hidden().expect("granica wyjscia");
            stages[index]
                .device
                .read(source, 0, &mut boundary)
                .expect("odczyt granicy");
        }
    }

    let last = stages.len() - 1;
    let split = stages[last]
        .stage_logits(rows - 1)
        .expect("glowa etapu ostatniego");
    for (model, seq) in stages.iter_mut().zip(seqs.iter_mut()) {
        model.release_seq(seq);
    }

    assert_eq!(split.len(), reference.len(), "dlugosc logitow");
    let mut max_abs = 0f32;
    for (a, b) in reference.iter().zip(split.iter()) {
        max_abs = max_abs.max((a - b).abs());
    }
    println!(
        "podzial na {} etapow: token {}, max |roznica| {max_abs:.6}",
        stages.len(),
        argmax(&split)
    );
    assert_eq!(
        argmax(&split),
        argmax(&reference),
        "podzial na etapy wybral inny token niz jedna karta"
    );
    println!("pierwszy token policzony na wielu kartach zgadza sie z jednokartowym");
}
