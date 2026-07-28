// Odczytuje JEDEN wiersz tablicy embeddingow prosto z pamieci karty i porownuje
// go z referencja. Powstal przy diagnozie Bielika: `llama-eval-callback` podaje
// wartosci wzorcowe, wiec to jest pierwszy punkt, w ktorym mozna rozstrzygnac,
// czy FORGE czyta te sama tablice.
use forge_engine::model::{Model, ModelConfig};
use forge_hal::{PoolSizes, gpu};

fn main() -> forge_types::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = std::path::PathBuf::from(args.next().expect("sciezka do gguf"));
    let token: usize = args.next().expect("id tokenu").parse().expect("liczba");

    let device = gpu::open(
        0,
        PoolSizes {
            weights: 10 << 30,
            kv_cache: 1 << 30,
            activations: 1 << 30,
            kv_page_size: 256 << 10,
        },
    )?;
    let model = Model::load_gguf(
        device.clone(),
        &path,
        ModelConfig {
            max_seq_len: 256,
            kv_pages: 8,
            prefix_cache: false,
            ..ModelConfig::default()
        },
    )?;
    // Weryfikacja wag glowy: bufor na karcie musi byc bajt w bajt tym samym, co
    // plik GGUF — DevWeight::Q8_0 trzyma surowe bloki, bez przepakowania.
    let gguf = forge_formats::Gguf::open(&path)?;
    let head_bytes = gguf.tensor_data("output.weight")?.to_vec();
    drop(gguf);
    if let forge_engine::weights::DevWeight::Q8_0 { buf, rows, cols } = &model.weights.lm_head {
        let row_bytes = (cols / 32) * 34;
        let mut mismatched = 0usize;
        let mut first_bad = usize::MAX;
        let mut on_gpu = vec![0u8; row_bytes];
        for r in [0usize, 1, 1000, 32000, 32100, rows - 2, rows - 1] {
            model.device.read(buf, r * row_bytes, &mut on_gpu)?;
            let want = &head_bytes[r * row_bytes..(r + 1) * row_bytes];
            if on_gpu != want {
                mismatched += 1;
                first_bad = first_bad.min(r);
            }
        }
        println!("glowa: rows={rows} cols={cols}, niezgodnych wierszy {mismatched}, pierwszy {first_bad}");
    }
    let hidden = model.weights.descriptor.params.hidden_size;
    let mut bytes = vec![0u8; hidden * 2];
    model
        .device
        .read(&model.weights.token_embd_f16, token * hidden * 2, &mut bytes)?;
    let values: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|c| half::f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();
    let sum: f32 = values.iter().sum();
    println!(
        "token {token}: pierwsze {:?} ostatnie {:?} suma {sum:.6}",
        values[..3].iter().map(|v| format!("{v:.4}")).collect::<Vec<_>>(),
        values[hidden - 3..].iter().map(|v| format!("{v:.4}")).collect::<Vec<_>>(),
    );
    Ok(())
}
