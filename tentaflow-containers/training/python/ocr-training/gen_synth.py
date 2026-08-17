# =============================================================================
# File: gen_synth.py
# Purpose: Synthetic single-row ADR digit sample generator (image + label) with
#          aggressive augmentation to bridge the synthetic->real domain gap.
#          The (kemler, UN) catalogue is INJECTED by the server from the training
#          request (Core reads the deployment's adr-list.json) — this module never
#          touches the filesystem for data, so the same code runs in a container
#          without the vision model dir mounted.
# Example: gen_synth.set_catalogue([("30", "1202"), ...]); img, text = make_sample()
# =============================================================================
import glob
import math
import os
import random

import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFont

IMG_H, IMG_W = 32, 128  # CRNN input (grayscale)

# Bold monospace/sans faces resembling the stencil digits on a real placard.
# Globs, not fixed paths: font packages land in different directories per distro,
# so the set is discovered at runtime and the absence of ALL of them is a hard
# error at job start (a silent fallback to one thin face would quietly cost
# accuracy on real crops).
_FONT_GLOBS = [
    "/usr/share/fonts/**/DejaVuSansMono-Bold.ttf",
    "/usr/share/fonts/**/DejaVuSans-Bold.ttf",
    "/usr/share/fonts/**/DejaVuSansCondensed-Bold.ttf",
    "/usr/share/fonts/**/LiberationSans-Bold.ttf",
    "/usr/share/fonts/**/LiberationMono-Bold.ttf",
    "/usr/share/fonts/**/NotoSansMono-Bold.ttf",
    "/usr/share/fonts/**/NotoSansMono-Black.ttf",
    "/usr/share/fonts/**/*Mono*Bold*.ttf",
    "/usr/share/fonts/**/*Sans*Bold*.ttf",
]

_FONT_CACHE: dict[tuple[str, int], ImageFont.FreeTypeFont] = {}
_FONTS: list[str] = []
_KEMLERS: list[str] = []
_UNS: list[str] = []


def discover_fonts() -> list[str]:
    """Bold TrueType faces available on this machine (deduplicated, sorted)."""
    global _FONTS
    if _FONTS:
        return _FONTS
    found: set[str] = set()
    for pattern in _FONT_GLOBS:
        for path in glob.glob(pattern, recursive=True):
            if os.path.isfile(path):
                found.add(path)
    _FONTS = sorted(found)
    return _FONTS


def set_catalogue(pairs: list[tuple[str, str]]) -> None:
    """Installs the deployment's (kemler, UN) pairs as the synthetic label source."""
    global _KEMLERS, _UNS
    _KEMLERS = [str(k) for k, _ in pairs if str(k).isdigit()]
    _UNS = [str(u) for _, u in pairs if str(u).isdigit()]


def _get_font(path, size):
    key = (path, size)
    if key not in _FONT_CACHE:
        _FONT_CACHE[key] = ImageFont.truetype(path, size)
    return _FONT_CACHE[key]


def sample_label():
    """Return a digit string for one row: half from the catalogue (real
    combinations the reader will actually meet), half random digit groups of
    plausible length (generalization). Without a catalogue every row is random."""
    if _KEMLERS and random.random() < 0.5:
        if random.random() < 0.5:
            return random.choice(_KEMLERS)
        return random.choice(_UNS)
    if random.random() < 0.5:
        n = random.choice([2, 2, 3])  # kemler
    else:
        n = 4  # UN
    return "".join(random.choice("0123456789") for _ in range(n))


def _render_row(text):
    """Render black digits on orange plate background at working resolution."""
    fonts = discover_fonts()
    if not fonts:
        raise RuntimeError("brak czcionek TrueType do generowania danych syntetycznych")
    font_path = random.choice(fonts)
    fsize = random.randint(46, 72)
    font = _get_font(font_path, fsize)
    # letter spacing
    spacing = random.randint(0, max(1, fsize // 6))
    # measure
    dummy = Image.new("RGB", (10, 10))
    dd = ImageDraw.Draw(dummy)
    widths, heights = [], []
    for ch in text:
        bb = dd.textbbox((0, 0), ch, font=font)
        widths.append(bb[2] - bb[0])
        heights.append(bb[3] - bb[1])
    txt_w = sum(widths) + spacing * (len(text) - 1)
    txt_h = max(heights)
    pad_x = random.randint(int(fsize * 0.15), int(fsize * 0.6))
    pad_y = random.randint(int(fsize * 0.12), int(fsize * 0.5))
    W = txt_w + 2 * pad_x
    H = txt_h + 2 * pad_y

    # RAL 1006 orange, randomized + fade
    base = (
        random.randint(0xD8, 0xF6),
        random.randint(0x50, 0x9C),
        random.randint(0x04, 0x1E),
    )
    img = Image.new("RGB", (W, H), base)
    draw = ImageDraw.Draw(img)

    # optional black border frame
    if random.random() < 0.7:
        bw = random.randint(2, max(3, fsize // 10))
        draw.rectangle([0, 0, W - 1, H - 1], outline=(random.randint(0, 40),) * 3, width=bw)

    # digit color: near black
    dc = (random.randint(0, 45),) * 3
    # optional horizontal condense to mimic narrow ADR font
    condense = random.uniform(0.7, 1.0)

    # draw onto a separate layer for condensing
    layer = Image.new("RGB", (txt_w, txt_h + 4), base)
    ld = ImageDraw.Draw(layer)
    x = 0
    for i, ch in enumerate(text):
        bb = ld.textbbox((0, 0), ch, font=font)
        ld.text((x - bb[0], -bb[1]), ch, font=font, fill=dc)
        x += widths[i] + spacing
    if condense < 0.999:
        layer = layer.resize((max(1, int(txt_w * condense)), txt_h + 4), Image.LANCZOS)
    lw, lh = layer.size
    ox = (W - lw) // 2 + random.randint(-4, 4)
    oy = (H - lh) // 2 + random.randint(-4, 4)
    img.paste(layer, (ox, oy))
    return np.array(img)  # RGB uint8


def _rand_perspective(img):
    h, w = img.shape[:2]
    m = min(h, w)
    d = m * random.uniform(0.0, 0.18)

    def jit():
        return random.uniform(-d, d)

    src = np.float32([[0, 0], [w, 0], [w, h], [0, h]])
    dst = np.float32([
        [0 + jit(), 0 + jit()], [w + jit(), 0 + jit()],
        [w + jit(), h + jit()], [0 + jit(), h + jit()],
    ])
    M = cv2.getPerspectiveTransform(src, dst)
    return cv2.warpPerspective(img, M, (w, h), borderMode=cv2.BORDER_REFLECT)


def _rand_affine(img):
    h, w = img.shape[:2]
    ang = random.uniform(-8, 8)
    scale = random.uniform(0.9, 1.1)
    M = cv2.getRotationMatrix2D((w / 2, h / 2), ang, scale)
    M[0, 2] += random.uniform(-0.05, 0.05) * w
    M[1, 2] += random.uniform(-0.05, 0.05) * h
    # shear
    sh = random.uniform(-0.12, 0.12)
    S = np.float32([[1, sh, 0], [0, 1, 0]])
    img = cv2.warpAffine(img, M, (w, h), borderMode=cv2.BORDER_REFLECT)
    img = cv2.warpAffine(img, S, (w, h), borderMode=cv2.BORDER_REFLECT)
    return img


def _motion_blur(img):
    k = random.choice([3, 5, 7, 9])
    kernel = np.zeros((k, k), np.float32)
    ang = random.uniform(0, math.pi)
    cx = cy = k // 2
    for i in range(k):
        x = int(round(cx + (i - cx) * math.cos(ang)))
        y = int(round(cy + (i - cx) * math.sin(ang)))
        if 0 <= x < k and 0 <= y < k:
            kernel[y, x] = 1
    s = kernel.sum()
    if s == 0:
        return img
    kernel /= s
    return cv2.filter2D(img, -1, kernel)


def augment(img):
    """Domain-gap augmentation. Public because REAL crops go through the same
    pipeline (minus the render step): a handful of hand-labelled plates would
    otherwise be memorized in a couple of epochs."""
    # geometric
    if random.random() < 0.85:
        img = _rand_perspective(img)
    if random.random() < 0.9:
        img = _rand_affine(img)

    # downscale->upscale (distance / VID low-res). Aggressive.
    if random.random() < 0.85:
        h, w = img.shape[:2]
        f = random.uniform(0.12, 0.9)
        nw, nh = max(4, int(w * f)), max(3, int(h * f))
        interp_d = random.choice([cv2.INTER_AREA, cv2.INTER_LINEAR])
        small = cv2.resize(img, (nw, nh), interpolation=interp_d)
        img = cv2.resize(small, (w, h), interpolation=random.choice([cv2.INTER_LINEAR, cv2.INTER_CUBIC]))

    # blur
    if random.random() < 0.5:
        k = random.choice([3, 3, 5])
        img = cv2.GaussianBlur(img, (k, k), 0)
    if random.random() < 0.35:
        img = _motion_blur(img)

    # photometric
    img = img.astype(np.float32)
    if random.random() < 0.9:
        alpha = random.uniform(0.6, 1.4)   # contrast
        beta = random.uniform(-40, 40)     # brightness
        img = img * alpha + beta
    img = np.clip(img, 0, 255)
    if random.random() < 0.6:
        gamma = random.uniform(0.6, 1.6)
        img = 255.0 * ((img / 255.0) ** gamma)
    # slight hue/color shift
    if random.random() < 0.5:
        shift = np.array([random.uniform(-12, 12) for _ in range(3)], np.float32)
        img = img + shift
    img = np.clip(img, 0, 255).astype(np.uint8)

    # noise
    if random.random() < 0.7:
        sigma = random.uniform(2, 22)
        noise = np.random.normal(0, sigma, img.shape).astype(np.float32)
        img = np.clip(img.astype(np.float32) + noise, 0, 255).astype(np.uint8)

    # occlusions: dirt blobs / scratches
    if random.random() < 0.5:
        h, w = img.shape[:2]
        for _ in range(random.randint(1, 4)):
            if random.random() < 0.5:
                c = (random.randint(0, w), random.randint(0, h))
                r = random.randint(2, max(3, w // 12))
                col = tuple(int(random.randint(0, 60)) for _ in range(3)) if random.random() < 0.5 \
                    else tuple(int(random.randint(120, 220)) for _ in range(3))
                cv2.circle(img, c, r, col, -1)
            else:
                p1 = (random.randint(0, w), random.randint(0, h))
                p2 = (random.randint(0, w), random.randint(0, h))
                cv2.line(img, p1, p2, tuple(int(random.randint(0, 60)) for _ in range(3)),
                         random.randint(1, 2))

    # JPEG compression
    if random.random() < 0.8:
        q = random.randint(28, 92)
        ok, enc = cv2.imencode(".jpg", img, [cv2.IMWRITE_JPEG_QUALITY, q])
        if ok:
            img = cv2.imdecode(enc, cv2.IMREAD_COLOR)
    return img


def make_sample():
    """One synthetic (grayscale 32x128, label) pair."""
    text = sample_label()
    rgb = _render_row(text)
    rgb = augment(rgb)
    gray = cv2.cvtColor(rgb, cv2.COLOR_BGR2GRAY)
    gray = cv2.resize(gray, (IMG_W, IMG_H), interpolation=cv2.INTER_AREA)
    return gray, text
