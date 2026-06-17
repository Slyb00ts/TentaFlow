// Regeneration tool for the vendored Burn vision models. Generates Rust + .bpk
// from the ONNX files; copy the resulting out/model/{rfdetr-base,model_stan,
// plate_ocr}.rs into tentaflow-core/src/vision/generated/ when the architecture
// changes (then RE-APPLY the manual resize-size fix documented in plate.rs).
fn gen(input: &str) {
    burn_onnx::ModelGen::new().input(input).out_dir("model/").run_from_script();
}
fn main() {
    let dir = "/home/critix/repos/rust/TentaFlow/.runtime/models/vision";
    for f in ["rfdetr-base.onnx", "model_stan.onnx", "plate_ocr.onnx"] {
        let p = format!("{dir}/{f}");
        if std::path::Path::new(&p).exists() {
            gen(&p);
        }
    }
}
