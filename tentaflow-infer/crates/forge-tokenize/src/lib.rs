// ===== File: lib.rs — forge-tokenize public API: tokenization, streaming detok, stop matching, chat templates =====

mod chat;
mod gguf;
mod gguf_vocab;
mod rawbytes;
mod stop;
mod stream;
mod tokenizer;

pub use chat::{builtin_chat_template, resolve_chat_template, ChatMessage, ChatTemplateEngine};
pub use gguf::GgufVocab;
pub use gguf_vocab::gguf_vocab;
pub use stop::{StopMatcher, StopStep};
pub use stream::StreamDecoder;
pub use tokenizer::Tokenizer;
