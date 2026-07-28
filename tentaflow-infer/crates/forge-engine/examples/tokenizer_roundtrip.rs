// ===== File: tokenizer_roundtrip.rs — kontrola tokenizera i hiperparametrow =====
//
// Wypisuje hiperparametry z GGUF oraz koduje i ODKODOWUJE teksty spoza ASCII.
// Powstal przy diagnozie Bielika, zeby rozstrzygnac, czy zly wynik bierze sie
// z tokenizacji, czy z liczenia — identyfikatory mozna porownac wprost
// z `llama-tokenize`, a hiperparametry z metadanymi pliku.
use forge_tokenize::Tokenizer;

fn main() -> forge_types::Result<()> {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("sciezka do gguf"));
    let gguf = forge_formats::Gguf::open(&path)?;
    let descriptor = forge_formats::ModelDescriptor::detect(&gguf)?;
    println!("{:#?}", descriptor.params);
    let vocab = forge_engine::gguf_vocab::gguf_vocab(&gguf)?;
    drop(gguf);
    let tokenizer = Tokenizer::from_gguf_vocab(&vocab)?;
    for text in [
        "The capital of Japan is",
        "The capital of France is Paris.",
        "Fotosynteza to proces biochemiczny, w ktorym rosliny",
        "Zażółć gęślą jaźń — ćma, śnieg, źdźbło.",
        "l'été est déjà là, français",
    ] {
        // `add_special=true` to ta sama ścieżka, którą idzie prompt w `run`,
        // więc porównanie identyfikatorow z `llama-tokenize` jest tu uczciwe.
        let ids = tokenizer.encode(text, true)?;
        println!("  ids: {ids:?}");
        let back = tokenizer.decode(&ids, false)?;
        let ok = if back.trim() == text.trim() { "OK " } else { "ROZNI" };
        println!("{ok} [{}] -> {} tok -> [{}]", text, ids.len(), back);
    }
    Ok(())
}
