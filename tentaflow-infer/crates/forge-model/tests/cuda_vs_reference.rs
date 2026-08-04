// ===== File: cuda_vs_reference.rs — the CUDA executor against the host oracle =====
//
// The same `Dense`, the same checkpoint, the same operations — computed twice
// by two executors that share nothing below the contract. One reads GGUF
// superblocks with Mojo kernels on the GPU; the other rewrites them into the
// affine triple and multiplies them in scalar f32 on the CPU. Neither decoder
// is derived from the other, so agreement is evidence and not a coincidence.
//
// Why the reference and not the recorded mlx-lm logits: the fixture belongs to
// the MLX 4-bit export of Bielik, and these kernels read the source's blocks
// rather than the affine triple, so they cannot be pointed at that checkpoint
// at all. The GGUF build is the same MODEL but not the same WEIGHTS — Q4_K_M
// is a different quantization, so its logits are legitimately different
// numbers, and holding them to a fixture recorded from another quantization
// would be measuring the quantization. The reference answers any input, which
// is what §4 of docs/ZADANIE_CUDA_EXECUTOR.md calls the more important half.

// Wspólny moduł niesie też fiksturę mlx-lm, której ten plik nie używa — a
// każdy plik testowy jest osobną binarką, więc kompilator liczy jej martwość
// osobno dla każdego z nich.
#[allow(dead_code)]
mod common;

use std::path::PathBuf;
use std::sync::Arc;

use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::{CudaExec, HostExec};
use forge_model::dense::Dense;

/// The GGUF build of Bielik. Found by extension rather than by name — the file
/// is named by whoever published it, and a hardcoded name turns a rename into
/// "no checkpoint, skipping".
fn checkpoint() -> Option<PathBuf> {
    let dir = PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../.runtime/models/bielik-minitron-7b-v3-gguf"
    ));
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
}

/// Pools claimed for this test.
///
/// Explicit rather than `with_default_pools`, which takes 90% of free VRAM: on
/// a unified-memory part that is a hundred gigabytes taken from the machine
/// running the reference in the same process.
fn pools() -> PoolSizes {
    PoolSizes {
        weights: 8 << 30,
        kv_cache: 2 << 30,
        kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        activations: 1 << 30,
    }
}

/// A prompt long enough to go through the batched form, then single steps
/// through the vector one.
///
/// Both forms matter and they are different kernels: the tile multiplies a
/// block of tokens at once, the step reads the whole matrix for one column.
/// Testing only the second would leave prefill unmeasured, and prefill is where
/// a wrong causal mask or a wrong position hides — a single token has neither.
const PROMPT: [u32; 6] = [1, 4234, 8123, 302, 15, 991];

#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu GGUF; wzorzec liczy minutami"]
fn the_cuda_executor_agrees_with_the_host_reference() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu GGUF");
        return;
    };
    let Ok(device) = CudaDevice::new(0, pools()) else {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    };

    let t = std::time::Instant::now();
    let mut gpu = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie modelu na CUDA");
    eprintln!("CUDA: wczytane w {:.1} s", t.elapsed().as_secs_f64());

    let t = std::time::Instant::now();
    let mut cpu = Dense::load(&path, HostExec::new).expect("wczytanie modelu na wzorcu");
    eprintln!("wzorzec: wczytany w {:.1} s", t.elapsed().as_secs_f64());

    let shape = gpu.shape();
    assert_eq!(shape.layers, 40);
    assert_eq!(shape.hidden, 4096);
    assert_eq!(shape.head_dim, 128);

    // Prefill: jeden kafel przez formę blokową po obu stronach.
    let t = std::time::Instant::now();
    let gpu_first = gpu.prefill(&PROMPT).expect("prefill CUDA");
    eprintln!("CUDA: prefill w {:.2} s", t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let cpu_first = cpu.prefill(&PROMPT).expect("prefill wzorca");
    eprintln!("wzorzec: prefill w {:.1} s", t.elapsed().as_secs_f64());
    compare("prefill", &gpu, &cpu);
    assert_eq!(gpu_first, cpu_first, "prefill wybrał inny token");

    // Dekodowanie: forma wektorowa, i pierwszy krok, który CZYTA cache
    // zapisany przez prefill — para rozdziela błąd arytmetyki od błędu cache'u.
    let mut token = gpu_first;
    for step in 1..=2 {
        let gpu_next = gpu.step_argmax(token).expect("krok CUDA");
        let cpu_next = cpu.step_argmax(token).expect("krok wzorca");
        compare(&format!("krok {step}"), &gpu, &cpu);
        assert_eq!(gpu_next, cpu_next, "krok {step} wybrał inny token");
        token = gpu_next;
    }
}

/// Wynik ma być JĘZYKIEM.
///
/// Zgodność ze wzorcem mówi, że oba liczą to samo, ale nie mówi, że to samo
/// jest modelem — dwie implementacje tej samej pomyłki dałyby ten sam wynik.
/// Ten test dokłada jedyne kryterium, którego nie da się spełnić przez
/// przypadek: prompt po polsku ma dostać ciąg dalszy po polsku, przez prawdziwy
/// tokenizator tego pliku i przez podział promptu na kafle.
#[test]
#[ignore = "wymaga karty NVIDIA i checkpointu GGUF"]
fn the_cuda_executor_continues_a_polish_prompt() {
    let Some(path) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu GGUF");
        return;
    };
    let Ok(device) = CudaDevice::new(0, pools()) else {
        eprintln!("pomijam: brak urządzenia CUDA");
        return;
    };

    let gguf = forge_formats::Gguf::open(&path).expect("otwarcie GGUF");
    let vocab = forge_tokenize::gguf_vocab(&gguf).expect("słownik z GGUF");
    let tokenizer = forge_tokenize::Tokenizer::from_gguf_vocab(&vocab).expect("tokenizator");
    drop(gguf);

    let mut model = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie modelu na CUDA");
    let prompt = tokenizer
        .encode("Stolicą Polski jest", true)
        .expect("tokenizacja");
    let t = std::time::Instant::now();
    let out = model.generate(&prompt, 24).expect("generacja");
    let text = tokenizer.decode(&out, true).expect("dekodowanie");
    eprintln!(
        "{} tokenów w {:.2} s: {text:?}",
        out.len(),
        t.elapsed().as_secs_f64()
    );

    // Polskie znaki diakrytyczne albo spacje: cokolwiek, co odróżnia zdanie od
    // powtarzanego tokenu. Kryterium celowo słabe — mocnym jest test wyżej, ten
    // ma złapać wynik, który jest liczbowo spójny, a językowo niczym.
    assert!(
        text.split_whitespace().count() >= 3,
        "kontynuacja nie jest zdaniem: {text:?}"
    );
    assert!(
        out.windows(4).any(|w| w.iter().any(|t| *t != w[0])),
        "kontynuacja to jeden powtarzany token: {out:?}"
    );
}

/// Logit po logicie, wobec rozpiętości tego kroku.
///
/// Próg jest luźniejszy niż bitowy i musi być: wzorzec liczy wszystko w f32, a
/// ścieżka GPU trzyma aktywacje w f16 i wagi norm w f16 zamiast f32 źródła. To
/// są RÓŻNE zaokrąglenia tej samej formuły. Rozjazd samej formuły — permutacja
/// RoPE, maska przyczynowa, zła grupa kwantyzacji — wychodzi natomiast na
/// tokenie, i dlatego token jest sprawdzany osobno i na równość.
fn compare(what: &str, gpu: &Dense<CudaExec>, cpu: &Dense<HostExec>) {
    let got = gpu.logits().expect("logity CUDA");
    let want = cpu.logits().expect("logity wzorca");
    assert_eq!(got.len(), want.len());

    let err = common::spread_error(&got, &want);
    let ours = common::top_k(&got, 5);
    let theirs = common::top_k(&want, 5);
    eprintln!(
        "{what}: {:.3}% rozpiętości, argmax {}",
        err * 100.0,
        ours[0]
    );
    assert_eq!(
        ours[..3],
        theirs[..3],
        "{what}: czołówka rozjechała się; CUDA {ours:?}, wzorzec {theirs:?}"
    );
    assert!(
        err < 0.02,
        "{what}: {:.3}% rozpiętości to nie jest ta sama arytmetyka",
        err * 100.0
    );
}
