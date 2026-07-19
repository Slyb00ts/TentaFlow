// Phase B accuracy probe: Q4_K -> W4A8 requant reconstruction error.
// Dequantizes real Mistral FFN tensors (Q4_K), requantizes each output row to
// int4 with per-group scales (group=128), and reports reconstruction error for
// both a symmetric int4 variant (drops per-group min) and an asymmetric
// int4+zero variant. Scratch tool; nothing committed.

use forge_formats::{dequantize_to_f32, Gguf};

fn gsz() -> usize { std::env::var("GRP").ok().and_then(|v| v.parse().ok()).unwrap_or(128) }

// Symmetric int4 per group: q in [-8,7], scale = max|w|/7.
fn requant_sym(row: &[f32], out: &mut [f32]) {
    let g = gsz();
    for (chunk, o) in row.chunks(g).zip(out.chunks_mut(g)) {
        let amax = chunk.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        let scale = if amax > 0.0 { amax / 7.0 } else { 1.0 };
        for (i, &w) in chunk.iter().enumerate() {
            let q = (w / scale).round().clamp(-8.0, 7.0);
            o[i] = q * scale;
        }
    }
}

// Asymmetric int4 per group: q in [0,15], w ~= scale*(q - zero).
fn requant_asym(row: &[f32], out: &mut [f32]) {
    let g = gsz();
    for (chunk, o) in row.chunks(g).zip(out.chunks_mut(g)) {
        let mut lo = f32::INFINITY;
        let mut hi = f32::NEG_INFINITY;
        for &x in chunk {
            lo = lo.min(x);
            hi = hi.max(x);
        }
        let scale = if hi > lo { (hi - lo) / 15.0 } else { 1.0 };
        let zero = (-lo / scale).round();
        for (i, &w) in chunk.iter().enumerate() {
            let q = (w / scale + zero).round().clamp(0.0, 15.0);
            o[i] = scale * (q - zero);
        }
    }
}

fn stats(reference: &[f32], approx: &[f32]) -> (f64, f64, f64) {
    let mut se = 0.0f64;
    let mut sref = 0.0f64;
    let mut maxabs = 0.0f64;
    for (&r, &a) in reference.iter().zip(approx) {
        let d = (r - a) as f64;
        se += d * d;
        sref += (r as f64) * (r as f64);
        maxabs = maxabs.max(d.abs());
    }
    let n = reference.len() as f64;
    let rmse = (se / n).sqrt();
    let rel = (se / sref.max(1e-30)).sqrt(); // relative L2 error
    (rmse, rel, maxabs)
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test-models/gguf/mistral-7b-q4_k_m.gguf".into());
    let g = Gguf::open(&path).expect("open gguf");

    // A representative spread of FFN tensors across depth.
    let names: Vec<String> = g
        .tensors()
        .iter()
        .map(|t| t.name.clone())
        .filter(|n| {
            (n.contains("ffn_down") || n.contains("ffn_gate") || n.contains("ffn_up"))
                && (n.contains(".0.")
                    || n.contains(".7.")
                    || n.contains(".15.")
                    || n.contains(".23.")
                    || n.contains(".31."))
        })
        .collect();

    println!("model: {path}");
    println!("tensors probed: {}", names.len());
    println!(
        "{:<28} {:>8} {:>8} {:>12} {:>12} {:>12} {:>12}",
        "tensor", "rows", "cols", "sym_relL2", "sym_rmse", "asym_relL2", "asym_rmse"
    );

    let mut agg_sym = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let mut agg_asym = (0.0f64, 0.0f64, 0.0f64, 0usize);

    for name in &names {
        let t = g.tensor(name).unwrap();
        let cols = t.dims[0] as usize;
        let rows = t.dims[1] as usize;
        let numel = rows * cols;
        let data = g.tensor_data(name).unwrap();
        let ref_f32 = dequantize_to_f32(t.dtype, t.quant, data, numel).unwrap();

        let mut sym = vec![0.0f32; numel];
        let mut asym = vec![0.0f32; numel];
        for r in 0..rows {
            let row = &ref_f32[r * cols..(r + 1) * cols];
            requant_sym(row, &mut sym[r * cols..(r + 1) * cols]);
            requant_asym(row, &mut asym[r * cols..(r + 1) * cols]);
        }
        let (srmse, srel, _smax) = stats(&ref_f32, &sym);
        let (armse, arel, _amax) = stats(&ref_f32, &asym);
        println!(
            "{:<28} {:>8} {:>8} {:>12.5} {:>12.6} {:>12.5} {:>12.6}",
            name, rows, cols, srel, srmse, arel, armse
        );
        agg_sym.0 += srel;
        agg_sym.1 += srmse;
        agg_sym.3 += 1;
        agg_asym.0 += arel;
        agg_asym.1 += armse;
        agg_asym.3 += 1;
    }

    let ns = agg_sym.3 as f64;
    println!(
        "\nMEAN  symmetric  relL2={:.5}  rmse={:.6}",
        agg_sym.0 / ns,
        agg_sym.1 / ns
    );
    println!(
        "MEAN  asymmetric relL2={:.5}  rmse={:.6}",
        agg_asym.0 / ns,
        agg_asym.1 / ns
    );
    println!("(relL2 = ||W_q4k - W_w4a8|| / ||W_q4k||; lower is closer to the committed Q4_K)");
}
