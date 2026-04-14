// =============================================================================
// Plik: build.rs
// Opis: Generuje struktury Rust z ONNX proto schema (onnx.proto3).
//       Robione przez prost-build w czasie kompilacji.
// =============================================================================

fn main() -> std::io::Result<()> {
    // PROTOC wybierany wg priorytetu:
    //   1) env PROTOC ustawiony -> uzywaj systemowy (CI, uzytkownicy z protoc w PATH)
    //   2) Linux/macOS -> fallback do protobuf-src (kompiluje protoc z sourca)
    //   3) Windows -> blad czytelny (MSVC link protobuf-src zawsze pada, uzytkownik
    //      ma zainstalowac `choco install protoc` lub podac PROTOC recznie)
    if std::env::var_os("PROTOC").is_none() {
        #[cfg(not(target_os = "windows"))]
        {
            std::env::set_var("PROTOC", protobuf_src::protoc());
        }
        #[cfg(target_os = "windows")]
        {
            panic!(
                "PROTOC env var nie jest ustawiony. Na Windows zainstaluj protoc \
                 (`choco install protoc` lub `scoop install protobuf`) i upewnij \
                 sie ze jest w PATH, albo ustaw PROTOC wskazujac na protoc.exe."
            );
        }
    }
    prost_build::Config::new()
        .out_dir("src/generated")
        .compile_protos(&["proto/onnx.proto"], &["proto/"])?;
    Ok(())
}
