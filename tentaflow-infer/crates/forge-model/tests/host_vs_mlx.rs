// ===== File: host_vs_mlx.rs — the reference executor against the same oracle =====
//
// The same `Dense`, the same checkpoint, the same recorded mlx-lm logits — and
// an executor that shares NOTHING with the GPU one but the contract between
// them. That is the whole test: if the model description were secretly written
// for Metal, this could not produce the same tokens.
//
// It is held to the EXTERNAL oracle rather than to the Metal executor on
// purpose. Two implementations of mine agreeing would only prove I made the
// same mistake twice; mlx-lm made its own.
//
// Notably this file does not mention `forge_hal`, and does not need a GPU: it
// is the one gate on the forward pass that a machine without an accelerator can
// still run.

mod common;

use forge_kernels::HostExec;
use forge_model::dense::{Dense, Feed};

/// Around fifteen seconds a token, and NOT marked ignored for it. A reference
/// nobody runs is a reference nobody can trust, and the machines that most need
/// this gate are the ones with no accelerator to run the fast path on.
#[test]
fn the_host_reference_decodes_the_same_tokens_as_mlx_lm() {
    let oracle = common::load();
    let Some(dir) = common::checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };

    let mut model =
        Dense::load(&dir, |spec| HostExec::new(spec)).expect("wczytanie modelu na wzorcu");
    let shape = model.shape();
    assert_eq!(shape.layers, 40);
    assert_eq!(shape.vocab as usize, oracle.vocab);

    // Dwa kroki, nie pięć jak na GPU: pierwszy nie dotyka cache'u, drugi już
    // czyta to, co pierwszy zapisał, więc para rozdziela błąd arytmetyki od
    // błędu cache'u — a każdy kolejny kosztowałby kolejne dziesiątki sekund,
    // nie odpowiadając na nowe pytanie.
    for (step, &token) in oracle.tokens.iter().take(2).enumerate() {
        let t = std::time::Instant::now();
        model
            .decode(&[Feed { slot: 0, token }])
            .expect("krok dekodowania");
        let got = model.logits(0).expect("logity");
        let want = &oracle.logits[step];
        assert_eq!(got.len(), want.len());

        let err = common::spread_error(&got, want);
        let ours = common::top_k(&got, 5);
        let theirs = common::top_k(want, 5);
        eprintln!(
            "krok {}: {:.2}% rozpiętości, argmax {}, {:.1} s",
            step + 1,
            err * 100.0,
            ours[0],
            t.elapsed().as_secs_f64()
        );

        // Token jest jedyną liczbą, która wychodzi z modelu na zewnątrz.
        assert_eq!(
            ours[0],
            theirs[0],
            "krok {}: inny token; nasza piątka {ours:?}, MLX {theirs:?}",
            step + 1
        );
        assert_eq!(
            ours[..3],
            theirs[..3],
            "krok {}: kolejność czołówki",
            step + 1
        );

        // Próg skalibrowany, nie wybrany: na TEJ SAMEJ wyroczni ścieżka
        // Metalowa daje 2,71% w pierwszym kroku i 1,21% w drugim, bo pierwszy
        // token nie ma kontekstu i logity są prawie płaskie. Wzorzec liczy
        // wszystko w f32, więc jego odległość od półprecyzyjnej wyroczni jest
        // INNA, a nie mniejsza — stąd pasmo dwukrotnie szersze. Rozjazd formuły
        // i tak wychodzi wcześniej, na tokenie.
        assert!(
            err < 0.05,
            "krok {}: {:.3}% rozpiętości to nie jest ta sama arytmetyka",
            step + 1,
            err * 100.0
        );
    }
}

/// Ta sama waga, przeczytana przez wzorzec i przez formułę afiniczną wprost.
///
/// Wzorzec jest oracle'em dla kerneli, więc sam potrzebuje czegoś, wobec czego
/// jest sprawdzalny — inaczej „zgadza się z wzorcem" znaczy tylko tyle, że dwa
/// pliki niosą ten sam błąd.
#[test]
fn the_reference_refuses_a_weight_it_was_not_set_up_for() {
    use forge_formats::affine::AffineTriple;
    use forge_graph::{ExecSpec, QuantWeight, WeightStore};
    use forge_types::{DType, DenseShape};

    let shape = DenseShape {
        hidden: 64,
        layers: 1,
        heads: 4,
        kv_heads: 2,
        head_dim: 16,
        inter: 128,
        vocab: 32,
        eps: 1e-5,
        rope_theta: 1e4,
        rope_rot: 16,
    };
    let mut exec = HostExec::new(ExecSpec {
        shape,
        ssm: None,
        attends: vec![true].into(),
        quant_params: DType::F16,
        norm_weights: DType::F32,
    })
    .expect("wzorzec");

    // Skale w bf16 wobec wykonawcy nastawionego na f16. Te same bajty, inne
    // liczby — dokładnie ta pomyłka dawała wcześniej płynny, zły tekst.
    let mut t = AffineTriple::new_f16(8, 64, 32);
    t.param_dtype = DType::BF16;
    assert!(
        exec.put_quant(QuantWeight::Affine(t)).is_err(),
        "zły typ parametrów przeszedł"
    );

    // Grupa, która nie dzieli wiersza, adresowałaby skale poza ich tablicą.
    let mut t = AffineTriple::new_f16(8, 64, 32);
    t.group = 48;
    assert!(
        exec.put_quant(QuantWeight::Affine(t)).is_err(),
        "grupa niedzieląca wiersza przeszła"
    );

    let t = AffineTriple::new_f16(8, 64, 32);
    assert!(
        exec.put_quant(QuantWeight::Affine(t)).is_ok(),
        "poprawna waga odrzucona"
    );
}
