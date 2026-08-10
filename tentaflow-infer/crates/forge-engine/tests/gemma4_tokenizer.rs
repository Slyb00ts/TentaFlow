// ===== File: gemma4_tokenizer.rs — round-trip tokenizera Gemma 4 z realnego GGUF =====

use forge_formats::gguf::Gguf;
use forge_tokenize::gguf_vocab;
use forge_tokenize::Tokenizer;

const MODEL: &str = "../../test-models/gguf/gemma-4-12b-it-qat-q4_0.gguf";

#[test]
fn gemma4_encode_decode_round_trip() {
    let path = std::path::Path::new(MODEL);
    if !path.exists() {
        return;
    }
    let gguf = Gguf::open(path).expect("open gguf");
    let vocab = gguf_vocab(&gguf).expect("vocab");
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("tokenizer");

    let text = "Wymień trzy największe miasta w Polsce.";
    let ids = tok.encode(text, true).expect("encode");
    let back = tok.decode(&ids, true).expect("decode");
    eprintln!("ids={ids:?}");
    eprintln!("back={back:?}");
    for id in [258882u32, 82138, 156644, 137847, 12293, 0] {
        eprintln!("id {id} -> {:?}", tok.decode(&[id], false));
    }
    assert_eq!(back.trim(), text, "round-trip tokenizera Gemma 4");
}
