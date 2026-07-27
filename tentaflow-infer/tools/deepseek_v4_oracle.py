#!/usr/bin/env python3
# =============================================================================
# Plik: deepseek_v4_oracle.py
# Opis: Liczy referencyjne aktywacje sciezki Q/KV uwagi DeepSeeka V4 na
#       prawdziwych wagach i zrzuca je do pliku binarnego dla testu Rusta.
#       RMSNorm, precompute_freqs_cis i apply_rotary_emb sa skopiowane
#       DOSLOWNIE z inference/model.py — chodzi o oracle, nie o reinterpretacje.
# Przyklad: python tools/deepseek_v4_oracle.py /tmp/ds_layer2_qkv.bin
#       potem: FORGE_DEEPSEEK_V4_ORACLE=/tmp/ds_layer2_qkv.bin cargo test
#              -p forge-formats --test deepseek_v4_attention
# =============================================================================
import json, math, struct, sys
import numpy as np
import torch

CKPT = "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4"
LAYER = 2
SEQLEN = 8

# --- skopiowane doslownie z inference/model.py -------------------------------

def precompute_freqs_cis(dim, seqlen, original_seq_len, base, factor, beta_fast, beta_slow):
    def find_correction_dim(num_rotations, dim, base, max_seq_len):
        return dim * math.log(max_seq_len / (num_rotations * 2 * math.pi)) / (2 * math.log(base))

    def find_correction_range(low_rot, high_rot, dim, base, max_seq_len):
        low = math.floor(find_correction_dim(low_rot, dim, base, max_seq_len))
        high = math.ceil(find_correction_dim(high_rot, dim, base, max_seq_len))
        return max(low, 0), min(high, dim - 1)

    def linear_ramp_factor(min, max, dim):
        if min == max:
            max += 0.001
        linear_func = (torch.arange(dim, dtype=torch.float32) - min) / (max - min)
        return torch.clamp(linear_func, 0, 1)

    freqs = 1.0 / (base ** (torch.arange(0, dim, 2, dtype=torch.float32) / dim))
    if original_seq_len > 0:
        low, high = find_correction_range(beta_fast, beta_slow, dim, base, original_seq_len)
        smooth = 1 - linear_ramp_factor(low, high, dim // 2)
        freqs = freqs / factor * (1 - smooth) + freqs * smooth
    t = torch.arange(seqlen)
    freqs = torch.outer(t, freqs)
    return torch.polar(torch.ones_like(freqs), freqs)


def apply_rotary_emb(x, freqs_cis, inverse=False):
    y = x
    x = torch.view_as_complex(x.float().unflatten(-1, (-1, 2)))
    if inverse:
        freqs_cis = freqs_cis.conj()
    if x.ndim == 3:
        freqs_cis = freqs_cis.view(1, x.size(1), x.size(-1))
    else:
        freqs_cis = freqs_cis.view(1, x.size(1), 1, x.size(-1))
    x = torch.view_as_real(x * freqs_cis).flatten(-2)
    y.copy_(x)
    return y


def rms_norm(x, weight, eps):
    dtype = x.dtype
    x = x.float()
    var = x.square().mean(-1, keepdim=True)
    x = x * torch.rsqrt(var + eps)
    return (weight * x).to(dtype)

# --- odczyt checkpointu ------------------------------------------------------

INDEX = json.load(open(f"{CKPT}/model.safetensors.index.json"))["weight_map"]

def raw(name):
    shard = INDEX[name]
    with open(f"{CKPT}/{shard}", "rb") as f:
        n = struct.unpack("<Q", f.read(8))[0]
        hdr = json.loads(f.read(n))
        base = 8 + n
        m = hdr[name]
        s, e = m["data_offsets"]
        f.seek(base + s)
        return m["dtype"], m["shape"], f.read(e - s)

def e4m3(b):
    b = b.astype(np.uint32)
    sign = np.where(b >> 7 & 1, -1.0, 1.0)
    ex = (b >> 3) & 0xF
    ma = b & 0x7
    v = np.where(ex == 0, ma / 8.0 * 2.0**-6, (1 + ma / 8.0) * 2.0 ** (ex.astype(np.int32) - 7))
    return (sign * v).astype(np.float32)

def load_matrix(name):
    """Waga FP8 z kafelkowa skala E8M0 albo zwykle bf16."""
    dt, shape, data = raw(name)
    if dt == "BF16":
        u = np.frombuffer(data, dtype=np.uint16).astype(np.uint32) << 16
        return u.view(np.float32).reshape(shape)
    assert dt == "F8_E4M3", (name, dt)
    w = e4m3(np.frombuffer(data, dtype=np.uint8)).reshape(shape)
    sdt, sshape, sdata = raw(name.replace(".weight", ".scale"))
    assert sdt == "F8_E8M0", sdt
    sc = np.frombuffer(sdata, dtype=np.uint8).astype(np.int32) - 127
    sc = np.power(2.0, sc).astype(np.float32).reshape(sshape)
    tile_r = shape[0] // sshape[0]
    tile_c = shape[1] // sshape[1]
    return w * np.repeat(np.repeat(sc, tile_r, axis=0), tile_c, axis=1)

def load_vector(name):
    dt, shape, data = raw(name)
    if dt == "BF16":
        u = np.frombuffer(data, dtype=np.uint16).astype(np.uint32) << 16
        return u.view(np.float32).reshape(shape)
    if dt == "F32":
        return np.frombuffer(data, dtype=np.float32).reshape(shape)
    raise AssertionError((name, dt))

# --- referencyjna sciezka Q/KV ----------------------------------------------

cfg = json.load(open(f"{CKPT}/config.json"))
dim = cfg["hidden_size"]
n_heads = cfg["num_attention_heads"]
head_dim = cfg["head_dim"]
rope_head_dim = cfg["qk_rope_head_dim"]
eps = cfg["rms_norm_eps"]
ratio = cfg["compress_ratios"][LAYER]

p = f"layers.{LAYER}.attn"
wq_a = torch.from_numpy(load_matrix(f"{p}.wq_a.weight").copy())
q_norm_w = torch.from_numpy(load_vector(f"{p}.q_norm.weight").copy()).float()
wq_b = torch.from_numpy(load_matrix(f"{p}.wq_b.weight").copy())
wkv = torch.from_numpy(load_matrix(f"{p}.wkv.weight").copy())
kv_norm_w = torch.from_numpy(load_vector(f"{p}.kv_norm.weight").copy()).float()

# Wejscie deterministyczne, o rozkladzie zblizonym do strumienia rezydualnego.
idx = np.arange(SEQLEN * dim, dtype=np.int64)
x_np = ((idx * 2654435761 % 2003).astype(np.float32) / 1001.5 - 1.0).reshape(1, SEQLEN, dim)
x = torch.from_numpy(x_np)

# Warstwy z kompresja uzywaja YaRN i wlasnej bazy rope; pozostale czystego rope.
if ratio:
    original_seq_len, rope_theta = 65536, float(cfg["compress_rope_theta"])
else:
    original_seq_len, rope_theta = 0, float(cfg["rope_theta"])
freqs_cis = precompute_freqs_cis(rope_head_dim, SEQLEN, original_seq_len, rope_theta, 16, 32, 1)

qr = rms_norm(x @ wq_a.T, q_norm_w, eps)
q = (qr @ wq_b.T).unflatten(-1, (n_heads, head_dim))
q = q * torch.rsqrt(q.square().mean(-1, keepdim=True) + eps)
apply_rotary_emb(q[..., -rope_head_dim:], freqs_cis)

kv = rms_norm(x @ wkv.T, kv_norm_w, eps)
apply_rotary_emb(kv[..., -rope_head_dim:], freqs_cis)

out = sys.argv[1]
with open(out, "wb") as f:
    f.write(struct.pack("<5i", SEQLEN, dim, n_heads, head_dim, rope_head_dim))
    for t in (x, qr, q, kv):
        f.write(t.detach().float().contiguous().numpy().tobytes())
print(f"zapisano {out}: seq={SEQLEN} dim={dim} heads={n_heads} hd={head_dim} rope={rope_head_dim} ratio={ratio}")
print("q  abs mean", q.abs().mean().item(), "kv abs mean", kv.abs().mean().item())
