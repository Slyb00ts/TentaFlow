// Burn-wgpu spike: import RF-DETR ONNX → generated Burn model at build time.
// This codegen step is the make-or-break for DETR op coverage on the universal
// (wgpu/Vulkan/Metal) backend — if an op is unsupported, it fails loudly here.
fn main() {
    burn_onnx::ModelGen::new()
        .input("/home/critix/repos/rust/TentaFlow/.runtime/models/vision/rfdetr-base.onnx")
        .out_dir("model/")
        .run_from_script();
}
