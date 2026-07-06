# =============================================================================
# File: export_onnx.py
# Purpose: Export the trained CRNN to ONNX (dynamic batch, opset 17) + alphabet.
# =============================================================================
import os, torch
from model import CRNN, ALPHABET, IMG_H, IMG_W

HERE = os.path.dirname(os.path.abspath(__file__))

def main():
    model = CRNN()
    model.load_state_dict(torch.load(os.path.join(HERE, "crnn_best.pt"), map_location="cpu"))
    model.eval()
    dummy = torch.randn(1, 1, IMG_H, IMG_W)
    out_path = os.path.join(HERE, "adr_ocr.onnx")
    torch.onnx.export(
        model, dummy, out_path,
        input_names=["input"], output_names=["logits"],
        dynamic_axes={"input": {0: "batch"}, "logits": {0: "batch"}},
        opset_version=17, do_constant_folding=True, dynamo=False,
    )
    with open(os.path.join(HERE, "adr_ocr_alphabet.txt"), "w") as f:
        f.write(ALPHABET + "\n")
    sz = os.path.getsize(out_path) / 1e6
    print(f"exported {out_path}  {sz:.2f} MB  alphabet='{ALPHABET}'")

    # numeric parity check torch vs onnxruntime
    import onnxruntime as ort, numpy as np
    sess = ort.InferenceSession(out_path, providers=["CPUExecutionProvider"])
    x = torch.randn(3, 1, IMG_H, IMG_W)
    with torch.no_grad():
        y_t = model(x).numpy()
    y_o = sess.run(None, {"input": x.numpy()})[0]
    print("max abs diff torch vs onnx:", float(np.abs(y_t - y_o).max()))

if __name__ == "__main__":
    main()
