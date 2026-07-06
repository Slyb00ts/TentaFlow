# =============================================================================
# File: eval.py
# Purpose: Honest eval of the trained ADR CRNN on REAL plate crops vs PP-OCRv5.
#          Splits each plate into top/bottom rows, reads kemler(top)/UN(bottom),
#          snaps UN to the ADR catalog (Levenshtein<=1). Reports primary
#          (upright top/bottom) and an orientation-search variant (VID plates are
#          rotated ~90 deg). NO tuning on this eval set.
# =============================================================================
import os, json, glob
import numpy as np
import cv2
import onnxruntime as ort

HERE = os.path.dirname(os.path.abspath(__file__))
IMG_H, IMG_W = 32, 128
ONNX = os.path.join(HERE, "adr_ocr.onnx")

with open(os.path.join(HERE, "adr_ocr_alphabet.txt")) as f:
    ALPHABET = f.read().strip()

with open(os.path.join(HERE, "adr-list.json"), encoding="utf-8") as f:
    PAIRS = json.load(f)["pary"]
UN_TO_KEMLER = {p["un"]: str(p["kemler"]) for p in PAIRS}
UN_LIST = list(UN_TO_KEMLER.keys())


def lev(a, b):
    if a == b:
        return 0
    la, lb = len(a), len(b)
    if la == 0:
        return lb
    if lb == 0:
        return la
    prev = list(range(lb + 1))
    for i in range(1, la + 1):
        cur = [i] + [0] * lb
        for j in range(1, lb + 1):
            cost = 0 if a[i - 1] == b[j - 1] else 1
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + cost)
        prev = cur
    return prev[lb]


def snap_un(read_un):
    """Return (best_un, dist) nearest catalog UN by Levenshtein."""
    best, bd = None, 99
    for u in UN_LIST:
        d = lev(read_un, u)
        if d < bd:
            bd, best = d, u
    return best, bd


class Reader:
    def __init__(self):
        prov = ort.get_available_providers()
        use = ["CUDAExecutionProvider", "CPUExecutionProvider"] if "CUDAExecutionProvider" in prov \
            else ["CPUExecutionProvider"]
        self.sess = ort.InferenceSession(ONNX, providers=use)
        self.iname = self.sess.get_inputs()[0].name

    def _prep(self, gray):
        g = cv2.resize(gray, (IMG_W, IMG_H), interpolation=cv2.INTER_AREA)
        x = (g.astype(np.float32) / 255.0 - 0.5) / 0.5
        return x[None, :, :]  # [1,H,W]

    def read_batch(self, grays):
        """grays: list of HxW uint8 -> list of (text, confidence)."""
        if not grays:
            return []
        X = np.stack([self._prep(g) for g in grays]).astype(np.float32)  # [N,1,H,W]
        logits = self.sess.run(None, {self.iname: X})[0]  # [N,T,C]
        # softmax
        m = logits.max(-1, keepdims=True)
        e = np.exp(logits - m)
        probs = e / e.sum(-1, keepdims=True)
        idx = probs.argmax(-1)  # [N,T]
        maxp = probs.max(-1)    # [N,T]
        out = []
        for row, pr in zip(idx, maxp):
            prev = 0
            s, confs = [], []
            for v, p in zip(row, pr):
                if v != 0 and v != prev:
                    s.append(ALPHABET[v - 1])
                    confs.append(p)
                prev = v
            text = "".join(s)
            conf = float(np.mean(confs)) if confs else 0.0
            out.append((text, conf))
        return out


def split_rows(img, margin=0.06):
    """Split into top/bottom halves with a margin gap around the midline."""
    h = img.shape[0]
    mid = h // 2
    gap = int(h * margin)
    top = img[0:max(1, mid - gap)]
    bot = img[min(h - 1, mid + gap):h]
    return top, bot


def to_gray(img):
    if img.ndim == 3:
        return cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    return img


def eval_upright(reader, files):
    """Primary method: assume upright plate, top=kemler, bottom=UN."""
    results = {}
    tops, bots, keys = [], [], []
    for f in files:
        img = cv2.imread(f, cv2.IMREAD_COLOR)
        g = to_gray(img)
        t, b = split_rows(g)
        tops.append(t); bots.append(b); keys.append(f)
    tr = reader.read_batch(tops)
    br = reader.read_batch(bots)
    for f, (kt, _), (ut, _) in zip(keys, tr, br):
        results[f] = (kt, ut)
    return results


def eval_orient(reader, files):
    """Orientation-search: try 0/90/180/270, pick by model confidence, then read."""
    results = {}
    for f in files:
        img = cv2.imread(f, cv2.IMREAD_COLOR)
        g = to_gray(img)
        best = None  # (conf, kemler, un)
        for k in range(4):
            rg = np.rot90(g, k)
            t, b = split_rows(rg)
            (kt, kc), (ut, uc) = reader.read_batch([t, b])
            # confidence: reward plausible lengths (kemler 2-3, un 4)
            conf = kc + uc
            if 2 <= len(kt) <= 3:
                conf += 0.15
            if len(ut) == 4:
                conf += 0.25
            if best is None or conf > best[0]:
                best = (conf, kt, ut)
        results[f] = (best[1], best[2])
    return results


def score(results, labels):
    """Return dict of counts. results: file->(kemler_read, un_read)."""
    dscn_snap = dscn_strict = 0
    vid_snap = vid_strict = 0
    n_dscn = n_vid = 0
    lab_un = lab_kem = lab_both = 0
    for f, (kread, uread) in results.items():
        base = os.path.basename(f)
        is_dscn = base.startswith("DSCN")
        if is_dscn:
            n_dscn += 1
        else:
            n_vid += 1
        best_un, d = snap_un(uread)
        snap = d <= 1
        strict = snap and (lev(kread, UN_TO_KEMLER[best_un]) <= 1)
        if is_dscn:
            dscn_snap += int(snap); dscn_strict += int(strict)
        else:
            vid_snap += int(snap); vid_strict += int(strict)
        if base in labels:
            gk, gu = labels[base]
            # after snap, compare snapped pair to ground truth
            un_ok = (snap and best_un == gu)
            kem_ok = un_ok and (UN_TO_KEMLER[best_un] == gk)
            lab_un += int(un_ok)
            lab_kem += int(kem_ok)
            lab_both += int(kem_ok)
    return dict(n_dscn=n_dscn, n_vid=n_vid,
                dscn_snap=dscn_snap, dscn_strict=dscn_strict,
                vid_snap=vid_snap, vid_strict=vid_strict,
                lab_un=lab_un, lab_both=lab_both, n_lab=len(labels))


def main():
    reader = Reader()
    files = sorted(glob.glob(os.path.join(HERE, "real_crops", "*.png")))
    labels = {}
    with open(os.path.join(HERE, "labels.tsv")) as f:
        for line in f:
            parts = line.strip().split("\t")
            if len(parts) == 3:
                labels[parts[0]] = (parts[1], parts[2])

    print(f"crops: {len(files)}  labeled: {len(labels)}  provider: {reader.sess.get_providers()[0]}")

    up = eval_upright(reader, files)
    ori = eval_orient(reader, files)

    su = score(up, labels)
    so = score(ori, labels)

    def row(name, s):
        print(f"\n[{name}]")
        print(f"  DSCN snap<=1 : {s['dscn_snap']}/{s['n_dscn']}   strict(pair): {s['dscn_strict']}/{s['n_dscn']}")
        print(f"  VID  snap<=1 : {s['vid_snap']}/{s['n_vid']}   strict(pair): {s['vid_strict']}/{s['n_vid']}")
        print(f"  34-labeled   : UN {s['lab_un']}/{s['n_lab']}   both kemler+UN {s['lab_both']}/{s['n_lab']}")

    row("PRIMARY upright top/bottom", su)
    row("ORIENTATION-search 0/90/180/270", so)

    print("\n================ NASZ vs PP-OCR (strict pair = honest) ================")
    print(f"PRIMARY : DSCN {su['dscn_strict']}/78 vs 34/78,  VID {su['vid_strict']}/973 vs 0/973,  34-labeled: {su['lab_both']}/34 zgodnych")
    print(f"ORIENT  : DSCN {so['dscn_strict']}/78 vs 34/78,  VID {so['vid_strict']}/973 vs 0/973,  34-labeled: {so['lab_both']}/34 zgodnych")
    print("\n(literal snap<=1 UN-only, as prescribed, may include catalog false-positives):")
    print(f"PRIMARY : DSCN {su['dscn_snap']}/78,  VID {su['vid_snap']}/973")
    print(f"ORIENT  : DSCN {so['dscn_snap']}/78,  VID {so['vid_snap']}/973")

    # dump per-file for spot check
    with open(os.path.join(HERE, "eval_dump.tsv"), "w") as f:
        f.write("file\tup_kemler\tup_un\tori_kemler\tori_un\n")
        for k in sorted(up):
            b = os.path.basename(k)
            f.write(f"{b}\t{up[k][0]}\t{up[k][1]}\t{ori[k][0]}\t{ori[k][1]}\n")


if __name__ == "__main__":
    main()
