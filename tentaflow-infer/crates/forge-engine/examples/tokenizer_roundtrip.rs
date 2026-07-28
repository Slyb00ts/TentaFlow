// Sprawdza, czy tokenizer GGUF poprawnie koduje i ODKODOWUJE tekst spoza ASCII.
// Powstal, zeby rozstrzygnac, czy znieksztalcony polski wynik bierze sie z
// detokenizacji (bajtowy fallback SPM), czy z liczenia na GPU.
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
        "The capital of France is Paris.",
        "Fotosynteza to proces biochemiczny, w ktorym rosliny",
        "Zażółć gęślą jaźń — ćma, śnieg, źdźbło.",
        "l'été est déjà là, français",
    ] {
        let ids = tokenizer.encode(text, false)?;
        let back = tokenizer.decode(&ids, false)?;
        let ok = if back.trim() == text.trim() { "OK " } else { "ROZNI" };
        println!("{ok} [{}] -> {} tok -> [{}]", text, ids.len(), back);
    }
    Ok(())
}
