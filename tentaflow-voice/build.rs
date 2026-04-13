// =============================================================================
// Plik: build.rs
// Opis: Generuje struktury Rust z ONNX proto schema (onnx.proto3).
//       Robione przez prost-build w czasie kompilacji.
// =============================================================================

fn main() -> std::io::Result<()> {
    // Uzyj systemowego protoc gdy PROTOC jest ustawione, w przeciwnym razie
    // skompiluj z crate'a protobuf-src — dzieki temu build nie wymaga
    // instalacji protobuf-compiler w systemie.
    if std::env::var_os("PROTOC").is_none() {
        std::env::set_var("PROTOC", protobuf_src::protoc());
    }
    prost_build::Config::new()
        .out_dir("src/generated")
        .compile_protos(&["proto/onnx.proto"], &["proto/"])?;
    Ok(())
}
