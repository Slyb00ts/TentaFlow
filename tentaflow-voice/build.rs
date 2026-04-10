// =============================================================================
// Plik: build.rs
// Opis: Generuje struktury Rust z ONNX proto schema (onnx.proto3).
//       Robione przez prost-build w czasie kompilacji.
// =============================================================================

fn main() -> std::io::Result<()> {
    prost_build::Config::new()
        .out_dir("src/generated")
        .compile_protos(&["proto/onnx.proto"], &["proto/"])?;
    Ok(())
}
