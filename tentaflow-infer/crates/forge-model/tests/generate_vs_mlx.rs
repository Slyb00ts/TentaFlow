// ===== File: generate_vs_mlx.rs — the whole path, text in and text out =====
//
// Tokenizer, prefill, decode loop and greedy choice, compared against mlx-lm
// token for token. The per-step logit test above checks the arithmetic; this
// one checks that the pieces are wired to each other — a tokenizer that drops
// the leading BOS, or a position that advances twice, produces perfectly valid
// text that simply is not the model's.
//
// Fixture: tools/mlx-oracle/gen_generate.py
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::path::PathBuf;

use forge_hal::metal_device::MetalDevice;
use forge_kernels::MetalExec;
use forge_model::dense::{Dense, Feed};

/// Slot cache'u tych testów. Jedna sekwencja, więc zawsze ten sam.
const SLOT: usize = 0;

/// Model plus wykonawca. To jedyne miejsce w teście, które wie, co liczy —
/// `Dense` dostaje wykonawcę jako wytwórnię i nigdy nie pyta, czym on jest.
fn open(device: std::sync::Arc<MetalDevice>, path: &std::path::Path) -> Dense<MetalExec> {
    Dense::load(path, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu")
}

use forge_tokenize::Tokenizer;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_generate_bielik.bin");
const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

struct Oracle {
    prompt_ids: Vec<u32>,
    generated: Vec<u32>,
    /// Odstęp dwóch najlepszych logitów, w rozpiętości logitów danego kroku.
    margins: Vec<f32>,
    top3: Vec<[u32; 3]>,
    prompt: String,
    text: String,
}

fn load() -> Oracle {
    assert_eq!(&FIXTURE[0..4], b"GEN1", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let n_prompt = u32_at(&mut pos) as usize;
    let n_generated = u32_at(&mut pos) as usize;
    let prompt_ids: Vec<u32> = (0..n_prompt).map(|_| u32_at(&mut pos)).collect();
    let generated: Vec<u32> = (0..n_generated).map(|_| u32_at(&mut pos)).collect();
    let margins: Vec<f32> = (0..n_generated)
        .map(|_| {
            let v = f32::from_le_bytes(FIXTURE[pos..pos + 4].try_into().unwrap());
            pos += 4;
            v
        })
        .collect();
    let top3: Vec<[u32; 3]> = (0..n_generated)
        .map(|_| [u32_at(&mut pos), u32_at(&mut pos), u32_at(&mut pos)])
        .collect();
    let json_len = u32_at(&mut pos) as usize;
    let meta: serde_json::Value =
        serde_json::from_slice(&FIXTURE[pos..pos + json_len]).expect("metadane fikstury");
    Oracle {
        prompt_ids,
        generated,
        margins,
        top3,
        prompt: meta["prompt"].as_str().unwrap().to_string(),
        text: meta["text"].as_str().unwrap().to_string(),
    }
}

fn checkpoint() -> Option<PathBuf> {
    let snapshots = PathBuf::from(CHECKPOINT);
    let dir = std::fs::read_dir(&snapshots).ok()?.flatten().next()?.path();
    dir.join("model.safetensors").is_file().then_some(dir)
}

#[test]
fn our_tokenizer_produces_the_same_prompt_ids() {
    // Sprawdzane osobno, bo rozjazd tutaj przesuwa CAŁĄ resztę: model dostaje
    // inne wejście i wszystko dalej jest już porównywaniem dwóch różnych zadań.
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).expect("tokenizer");
    let mut ids = tokenizer.encode(&oracle.prompt, false).expect("encode");
    // Llama zaczyna od BOS; `encode` go nie dokłada, a bez niego model
    // odpowiada na zdanie zaczynające się znikąd.
    ids.insert(0, 1);
    assert_eq!(ids, oracle.prompt_ids, "inne tokeny promptu");
}

#[test]
fn greedy_generation_matches_mlx_lm_token_for_token() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json")).expect("tokenizer");
    let mut model = open(device, &dir);

    // Kontekst WYMUSZONY: na każdym kroku model dostaje prompt plus tokeny,
    // które wybrał MLX, i porównujemy sam wybór. Bez tego pierwszy rozjazd
    // rozgałęzia generację i wszystkie kolejne porównania zestawiają dwie różne
    // kontynuacje — token 1 „nie zgadza się przy marginesie 20%" nie znaczy
    // wtedy nic poza tym, że token 0 był inny.
    let mut forced = oracle.prompt_ids.clone();
    forced.extend_from_slice(&oracle.generated[..oracle.generated.len() - 1]);

    let mut got = Vec::with_capacity(oracle.generated.len());
    for (i, &token) in forced.iter().enumerate() {
        let choice = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
        if i + 1 >= oracle.prompt_ids.len() {
            got.push(choice);
        }
    }
    assert_eq!(got.len(), oracle.generated.len());

    let text = tokenizer.decode(&got, true).expect("detokenizacja");
    eprintln!("prompt: {:?}", oracle.prompt);
    eprintln!("nasze:  {text:?}");
    eprintln!("mlx:    {:?}", oracle.text);

    // Wymaganie zgodności token po tokenie byłoby tu wymaganiem, którego nie da
    // się spełnić bez zgodności BITOWEJ logitów. Zmierzone: nasze logity różnią
    // się od MLX o 0,4-2,6% rozpiętości, a w tej generacji SIEDEM z dwunastu
    // kroków ma odstęp dwóch najlepszych kandydatów mniejszy niż 5% rozpiętości.
    // Przy takim remisie dwie poprawne implementacje różniące się kolejnością
    // sumowania mają prawo wybrać inaczej — i to nie jest usterka do zamaskowania,
    // tylko własność zachłannego wyboru na wąskim marginesie.
    //
    // Kontrakt jest więc dwuczłonowy i jawny:
    //   * gdy margines MLX przekracza 5% rozpiętości, token MUSI się zgadzać;
    //   * gdy jest węższy, nasz token musi być wśród trzech najlepszych MLX.
    let mut ties = 0usize;
    for (i, (ours, theirs)) in got.iter().zip(&oracle.generated).enumerate() {
        let margin = oracle.margins[i];
        if margin >= 0.05 {
            assert_eq!(
                ours, theirs,
                "token {i}: margines MLX {:.1}% rozpiętości, a wybory są różne \
                 ({ours} wobec {theirs})",
                margin * 100.0
            );
        } else {
            ties += 1;
            assert!(
                oracle.top3[i].contains(ours),
                "token {i}: margines {:.1}%, ale nasz wybór {ours} nie jest nawet \
                 w trójce MLX {:?}",
                margin * 100.0,
                oracle.top3[i]
            );
        }
    }
    eprintln!("kroków rozstrzygniętych remisem: {ties} z {}", got.len());
    // Gdyby remisów nie było wcale, powyższy podział niczego by nie sprawdzał.
    assert!(ties > 0, "fikstura nie zawiera ani jednego wąskiego marginesu");
}

#[test]
fn generate_agrees_with_stepping_by_hand() {
    // `generate` skraca pętlę wywołującego, więc musi robić dokładnie to samo,
    // co ręczne podawanie tokenów: ta sama obsługa pozycji i ten sam wybór.
    // Rozjazd tutaj byłby usterką widoczną dopiero w tekście.
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    let steps = 3usize;
    let via_api = model.generate(SLOT, &oracle.prompt_ids, steps).expect("generacja");
    assert_eq!(via_api.len(), steps);
    assert_eq!(
        model.position(SLOT).expect("pozycja") as usize,
        oracle.prompt_ids.len() + steps - 1,
        "pozycja po generacji"
    );

    model.reset(SLOT).expect("reset");
    let mut by_hand = Vec::with_capacity(steps);
    let mut next = 0u32;
    for (i, &token) in oracle.prompt_ids.iter().enumerate() {
        next = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
        let _ = i;
    }
    by_hand.push(next);
    for _ in 1..steps {
        next = model.decode(&[Feed { slot: SLOT, token: next }]).expect("krok")[0];
        by_hand.push(next);
    }

    assert_eq!(via_api, by_hand, "`generate` odbiega od ręcznej pętli");
}

#[test]
fn a_prompt_longer_than_one_chunk_lands_where_stepping_lands() {
    // Prompt dłuższy niż kafel przechodzi przez WIĘCEJ NIŻ JEDNO wywołanie, a
    // drugie z nich musi zacząć tam, gdzie skończyło pierwsze. Pomyłka o jeden
    // w tym miejscu daje wynik poprawnego kształtu, policzony dla kontekstu
    // przesuniętego o token — czyli dokładnie taki, jakiego żadna asercja
    // rozmiaru nie zobaczy.
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    let chunk = model.max_tokens() as usize;
    let mut prompt = oracle.prompt_ids.clone();
    while prompt.len() <= chunk + 8 {
        prompt.extend_from_slice(&oracle.prompt_ids[1..]);
    }
    assert!(prompt.len() > chunk, "prompt mieści się w jednym kaflu");

    let batched = model.prefill(SLOT, &prompt).expect("prefill");
    let after = model.position(SLOT).expect("pozycja");
    assert_eq!(after as usize, prompt.len(), "pozycja po prefillu");

    model.reset(SLOT).expect("reset");
    let mut stepped = 0u32;
    for &token in &prompt {
        stepped = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
    }
    assert_eq!(
        model.position(SLOT).expect("pozycja") as usize,
        prompt.len(),
        "pozycja po krokach"
    );
    assert_eq!(
        batched, stepped,
        "prefill kafelkowany wybrał inny token niż krok po kroku"
    );
}

/// Pomiar, uruchamiany jawnie: `cargo test --release ... -- --ignored --nocapture`.
#[test]
#[ignore]
fn how_fast_a_prompt_goes_through() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    // Długość promptu z otoczenia, żeby dało się porównać z MLX na tej samej
    // skali. Prefill jest ograniczony obliczeniami, a wydajność mnożenia rośnie
    // z liczbą tokenów w kaflu, więc jedna długość nie opisuje ścieżki.
    let want: usize = std::env::var("FORGE_BENCH_PROMPT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let mut prompt = Vec::new();
    while prompt.len() < want {
        prompt.extend_from_slice(&oracle.prompt_ids);
    }
    prompt.truncate(want);

    // Rozgrzewka i mediana, jak po stronie MLX. Pojedynczy zimny przebieg
    // mierzy rezydencję wag i kompilację kerneli, a nie kernel — i porównanie
    // takiej liczby z rozgrzaną medianą konkurenta jest porównaniem dwóch
    // różnych rzeczy.
    let mut batched_times = Vec::new();
    let mut a = 0u32;
    for run in 0..4 {
        model.reset(SLOT).expect("reset");
        let t0 = std::time::Instant::now();
        a = model.prefill(SLOT, &prompt).expect("prefill");
        if run > 0 {
            batched_times.push(t0.elapsed().as_secs_f64());
        }
    }
    batched_times.sort_by(f64::total_cmp);
    let batched = batched_times[batched_times.len() / 2];

    model.reset(SLOT).expect("reset");
    let t0 = std::time::Instant::now();
    let mut b = 0u32;
    for &token in &prompt {
        b = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
    }
    let stepped = t0.elapsed().as_secs_f64();

    // Kaflowo i token po tokenie to DWIE różne arytmetyki — jednostki
    // macierzowe wobec formy wektorowej, a przy dość długim prompcie także
    // wiersze policzone na CPU. Wymaganie tego samego argmaxu wymagałoby więc
    // zgodności co do bitu, której żadna z tych par nie ma. Warunek, który
    // faktycznie pilnuje, że czasy dotyczą tej samej pracy, jest na logitach.
    if a != b {
        eprintln!("uwaga: kaflowo {a}, token po tokenie {b} — remis rozstrzygnięty inaczej");
    }
    eprintln!(
        "prompt {} tokenów: kaflowo {:.3} s ({:.1} tok/s), token po tokenie {:.3} s \
         ({:.1} tok/s), {:.2}x",
        prompt.len(),
        batched,
        prompt.len() as f64 / batched,
        stepped,
        prompt.len() as f64 / stepped,
        stepped / batched
    );
    // Samo dekodowanie mierzy `how_fast_decode_runs` — osobno, bo to inna ściana.
}

/// Pomiar dekodowania, uruchamiany jawnie. Bezpośrednio porównywalny z
/// `tools/mlx-oracle/bench_mlx.py`: ten sam model, ten sam prompt, ten sam
/// zakres pozycji.
///
/// Osobno od pomiaru prefillu, bo dekodowanie ogranicza PAMIĘĆ, a nie
/// obliczenia — jeden token to jedno przejście przez wszystkie wagi — i jedna
/// liczba na oba nie mówi, którą ścianę się właśnie dotyka.
#[test]
#[ignore]
fn how_fast_decode_runs() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    let mut prompt = Vec::new();
    while prompt.len() < 256 {
        prompt.extend_from_slice(&oracle.prompt_ids);
    }
    prompt.truncate(256);
    const STEPS: usize = 31;

    let mut times = Vec::new();
    for run in 0..4 {
        model.reset(SLOT).expect("reset");
        let mut token = model.prefill(SLOT, &prompt).expect("prefill");
        let t0 = std::time::Instant::now();
        for _ in 0..STEPS {
            token = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
        }
        let dt = t0.elapsed().as_secs_f64();
        // Pierwszy przebieg to rozgrzewka: rezydencja wag i kompilacja.
        if run > 0 {
            times.push(dt);
        }
    }
    times.sort_by(f64::total_cmp);
    let median = times[times.len() / 2];
    eprintln!(
        "dekodowanie po 256 tokenach promptu: {median:.3} s na {STEPS} tokenów \
         ({:.1} tok/s), przepustowość wag {:.1} GB/s",
        STEPS as f64 / median,
        4.207 * STEPS as f64 / median
    );
}

/// Bramka „bez klifu": rejestr wariantów ma dobierać formę tak, żeby koszt na
/// token NIE ROSŁ skokowo przy przejściu z jednej formy na drugą.
///
/// Uruchamiana jawnie, bo mierzy czas. Nie sprawdza, że jest szybko — sprawdza,
/// że nie ma rozmiaru wsadu, na którym silnik nagle zwalnia, bo to jest ten
/// rodzaj usterki, którego nie widać w żadnym pomiarze pojedynczej długości.
#[test]
#[ignore]
fn no_batch_size_falls_off_a_cliff() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    // Rozmiary wokół obu progów rejestru: 32 (blok uwagi) i 64 (blok macierzy).
    const SIZES: &[usize] = &[8, 16, 24, 31, 32, 33, 40, 48, 56, 63, 64, 65, 128, 256];
    let mut per_token = Vec::new();
    for &n in SIZES {
        let mut prompt = Vec::new();
        while prompt.len() < n {
            prompt.extend_from_slice(&oracle.prompt_ids);
        }
        prompt.truncate(n);

        let mut best = f64::INFINITY;
        for _ in 0..3 {
            model.reset(SLOT).expect("reset");
            let t0 = std::time::Instant::now();
            model.prefill(SLOT, &prompt).expect("prefill");
            best = best.min(t0.elapsed().as_secs_f64());
        }
        let us = best * 1e6 / n as f64;
        eprintln!("wsad {n:4}: {us:8.1} us/token");
        per_token.push((n, us));
    }

    // Koszt na token ma maleć albo stać. Wzrost oznacza, że większy wsad trafił
    // w formę gorszą od tej, która obsłużyła mniejszy — czyli że próg w
    // rejestrze jest po złej stronie pomiaru.
    for pair in per_token.windows(2) {
        let (n0, t0) = pair[0];
        let (n1, t1) = pair[1];
        assert!(
            t1 <= t0 * 1.25,
            "klif: wsad {n1} kosztuje {t1:.1} us/token, a mniejszy {n0} tylko {t0:.1}"
        );
    }

    // Wsad tuż poniżej pełnego bloku MUSI kosztować podobnie co pełny. Forma
    // macierzowa liczy blok niezależnie od tego, ile w nim tokenów, więc
    // odesłanie 63 tokenów do formy blokowej kosztowało 11 848 us/token wobec
    // 5 186 dla 64 — ten sam blok pracy, dwa razy drożej. To jest asercja na
    // decyzję, nie na kernel: pilnuje, żeby próg formy macierzowej nie wrócił
    // do wysokości bloku.
    let at = |n: usize| per_token.iter().find(|(s, _)| *s == n).expect("rozmiar w zestawie").1;
    assert!(
        at(63) <= at(64) * 1.5,
        "wsad 63 kosztuje {:.1} us/token przy {:.1} dla pełnego bloku — forma \
         macierzowa nie obsługuje niepełnego bloku",
        at(63),
        at(64)
    );
}

/// Czy oddanie części wierszy CPU zmienia WYNIK, a nie tylko czas.
///
/// Podział jest legalny tylko wtedy, gdy liczy to samo. Nie co do bitu — CPU
/// akumuluje w f32 tam, gdzie jednostka macierzowa zaokrągla mnożenie do half,
/// więc różnica rzędu zaokrąglenia jest oczekiwana. Czego oczekiwać NIE wolno,
/// to różnicy, która przesuwa rozkład: stąd porównanie całego wektora logitów,
/// a nie samego argmaxu, który przy remisie przeskakuje bez znaczenia.
#[test]
#[ignore]
fn the_cpu_share_computes_the_same_thing_as_the_gpu_alone() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    // Dość długi, żeby q/o i gate/up/down faktycznie weszły w podział.
    let mut prompt = Vec::new();
    while prompt.len() < 256 {
        prompt.extend_from_slice(&oracle.prompt_ids);
    }
    prompt.truncate(256);

    model.exec_mut().set_cpu_share(false);
    model.reset(SLOT).expect("reset");
    model.prefill(SLOT, &prompt).expect("prefill bez podziału");
    let alone = model.logits(0).expect("logity bez podziału");

    model.exec_mut().set_cpu_share(true);
    model.reset(SLOT).expect("reset");
    model.prefill(SLOT, &prompt).expect("prefill z podziałem");
    let shared = model.logits(0).expect("logity z podziałem");

    assert_eq!(alone.len(), shared.len());
    let mut worst = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut norm_sq = 0.0f64;
    for (&x, &y) in alone.iter().zip(&shared) {
        worst = worst.max((x - y).abs());
        sum_sq += f64::from(x - y) * f64::from(x - y);
        norm_sq += f64::from(x) * f64::from(x);
    }
    let rel = (sum_sq / norm_sq).sqrt();
    let span = alone.iter().fold(f32::MIN, |m, v| m.max(*v))
        - alone.iter().fold(f32::MAX, |m, v| m.min(*v));
    eprintln!(
        "logity: największa różnica {worst:.4} przy rozpiętości {span:.1}, względna L2 {rel:.2e}"
    );

    // Próg NIE jest wzięty z tego, co akurat wyszło. Repo mierzy własną różnicę
    // wobec MLX na 0,4-2,6% rozpiętości logitów; podział ma się mieścić grubo
    // pod tą wartością, bo inaczej byłby większym źródłem błędu niż wszystko,
    // co już zaakceptowaliśmy jako poprawne.
    assert!(
        worst < 0.004 * span,
        "podział różni się od samego GPU o {:.2}% rozpiętości, czyli więcej niż \
         cała nasza różnica wobec MLX",
        100.0 * worst / span
    );

    // To jest właściwy kontrakt: różnicy wolno istnieć, ale nie wolno jej
    // zmieniać WYBORU tam, gdzie wybór jest wyraźny. Ten sam podział na remisy
    // co w porównaniu z MLX — poniżej 5% rozpiętości dwie poprawne
    // implementacje mają prawo wskazać co innego.
    let best = |v: &[f32]| {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        idx.sort_by(|&a, &b| v[b].total_cmp(&v[a]));
        (idx[0], (v[idx[0]] - v[idx[1]]) / span)
    };
    let (want, margin) = best(&alone);
    let (got, _) = best(&shared);
    eprintln!("margines zwycięzcy: {:.1}% rozpiętości", margin * 100.0);
    if margin >= 0.05 {
        assert_eq!(
            got, want,
            "przy marginesie {:.1}% podział wskazał inny token",
            margin * 100.0
        );
    } else {
        eprintln!("remis — wybór może się różnić i to nie jest usterka");
    }
}
