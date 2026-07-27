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

# --- bramka MoE i pojedynczy ekspert (warstwa bez routingu haszowanego) -----

GATE_LAYER = 3          # warstwy 0-2 routuja przez tid2eid, nie przez wynik
TOPK = cfg["num_experts_per_tok"]
N_EXPERTS = cfg["n_routed_experts"]
ROUTE_SCALE = cfg["routed_scaling_factor"]
SWIGLU_LIMIT = cfg["swiglu_limit"]

g = f"layers.{GATE_LAYER}.ffn.gate"
gate_w = torch.from_numpy(load_matrix(f"{g}.weight").copy())
gate_b = torch.from_numpy(load_vector(f"{g}.bias").copy()).float()

# Wejscie bramki: strumien rezydualny po normie FFN — inny wzorzec niz x.
gi = np.arange(SEQLEN * dim, dtype=np.int64)
gx = ((gi * 40503 % 1999).astype(np.float32) / 999.5 - 1.0).reshape(SEQLEN, dim)
gx_t = torch.from_numpy(gx)

scores = (gx_t.float() @ gate_w.float().T)
scores = torch.nn.functional.softplus(scores).sqrt()
original = scores
biased = scores + gate_b
indices = biased.topk(TOPK, dim=-1)[1]
weights = original.gather(1, indices)
weights = weights / weights.sum(dim=-1, keepdim=True)
weights = weights * ROUTE_SCALE

def dequant_nvfp4(name):
    _, shp, packed = raw(f"{name}.weight")
    _, sshp, sraw = raw(f"{name}.weight_scale")
    _, _, graw = raw(f"{name}.weight_scale_2")
    p8 = np.frombuffer(packed, dtype=np.uint8).reshape(shp)
    E2M1 = np.array([0, .5, 1, 1.5, 2, 3, 4, 6, -0., -.5, -1, -1.5, -2, -3, -4, -6], dtype=np.float32)
    lo = E2M1[p8 & 0xF]
    hi = E2M1[p8 >> 4]
    vals = np.empty((shp[0], shp[1] * 2), dtype=np.float32)
    vals[:, 0::2] = lo
    vals[:, 1::2] = hi
    sc = e4m3(np.frombuffer(sraw, dtype=np.uint8)).reshape(sshp)
    gs = np.frombuffer(graw, dtype=np.float32)[0]
    return vals * np.repeat(sc, 16, axis=1) * gs

# Ekspert 0: SwiGLU z obcieciem, waga routingu wchodzi PRZED projekcja wyjsciowa.
e = f"layers.{GATE_LAYER}.ffn.experts.0"
w1 = torch.from_numpy(dequant_nvfp4(f"{e}.w1").copy())
w2 = torch.from_numpy(dequant_nvfp4(f"{e}.w2").copy())
w3 = torch.from_numpy(dequant_nvfp4(f"{e}.w3").copy())
gate_act = (gx_t @ w1.T).float()
up_act = (gx_t @ w3.T).float()
up_act = torch.clamp(up_act, min=-SWIGLU_LIMIT, max=SWIGLU_LIMIT)
gate_act = torch.clamp(gate_act, max=SWIGLU_LIMIT)
expert_out = (torch.nn.functional.silu(gate_act) * up_act) @ w2.T

# --- sciezka wyjscia uwagi: rope ODWROTNE + grupowana LoRA ------------------

o_groups = cfg["o_groups"]
o_lora_rank = cfg["o_lora_rank"]
wo_a = torch.from_numpy(load_matrix(f"{p}.wo_a.weight").copy())
wo_b = torch.from_numpy(load_matrix(f"{p}.wo_b.weight").copy())

# Wejscie: syntetyczne wyjscie uwagi [1, S, heads, head_dim]. Sciezka wyjsciowa
# jest funkcja czysta, wiec nie wymaga policzenia samej uwagi.
oi = np.arange(SEQLEN * n_heads * head_dim, dtype=np.int64)
attn_np = ((oi * 22695477 % 1997).astype(np.float32) / 998.5 - 1.0).reshape(
    1, SEQLEN, n_heads, head_dim)
attn_in = torch.from_numpy(attn_np.copy())

o = attn_in.clone()
apply_rotary_emb(o[..., -rope_head_dim:], freqs_cis, True)
o = o.view(1, SEQLEN, o_groups, -1)
wo_a_g = wo_a.view(o_groups, o_lora_rank, -1)
o = torch.einsum("bsgd,grd->bsgr", o.float(), wo_a_g.float())
attn_out = o.flatten(2) @ wo_b.float().T

out = sys.argv[1]
with open(out, "wb") as f:
    f.write(struct.pack("<10i", SEQLEN, dim, n_heads, head_dim, rope_head_dim,
                        GATE_LAYER, TOPK, N_EXPERTS, o_groups, o_lora_rank))
    for t in (x, qr, q, kv):
        f.write(t.detach().float().contiguous().numpy().tobytes())
    f.write(gx_t.contiguous().numpy().tobytes())
    f.write(indices.to(torch.int32).contiguous().numpy().tobytes())
    f.write(weights.float().contiguous().numpy().tobytes())
    f.write(expert_out.float().contiguous().numpy().tobytes())
    f.write(attn_in.float().contiguous().numpy().tobytes())
    f.write(attn_out.float().contiguous().numpy().tobytes())
print("bramka: pierwsze indeksy", indices[0].tolist(), "wagi", [round(v,4) for v in weights[0].tolist()])
print(f"zapisano {out}: seq={SEQLEN} dim={dim} heads={n_heads} hd={head_dim} rope={rope_head_dim} ratio={ratio}")
print("q  abs mean", q.abs().mean().item(), "kv abs mean", kv.abs().mean().item())
