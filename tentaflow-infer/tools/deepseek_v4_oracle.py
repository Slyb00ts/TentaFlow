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

# --- kompresor strumienia KV -------------------------------------------------

def act_quant_inplace(x, block=64):
    """Symulacja kwantyzacji aktywacji do FP8 (QAT), skala zaokraglana do potegi
    dwojki jak przy scale_fmt='ue8m0'. Odpowiednik act_quant(..., inplace=True)."""
    shape = x.shape
    xg = x.reshape(-1, shape[-1] // block, block).float()
    amax = xg.abs().amax(-1, keepdim=True).clamp_min(1e-4)
    s = torch.pow(2.0, torch.ceil(torch.log2(amax / 448.0)))
    q = (xg / s).to(torch.float8_e4m3fn).float() * s
    return q.reshape(shape).to(x.dtype)


def compressor_prefill(x, ratio, wkv_c, wgate_c, ape, norm_w, freqs_cis, head_dim, rope_dim, eps):
    """Sciezka prefill kompresora: bramkowany pooling po oknach `ratio` tokenow.
    Dla ratio 4 okna sa Z ZAKLADKA — projekcje daja dwa razy szerszy wektor,
    ktorego pierwsza polowa opisuje okno przesuniete o jeden blok wstecz."""
    overlap = ratio == 4
    seqlen = x.size(1)
    d = head_dim
    xf = x.float()
    kv = xf @ wkv_c.float().T
    score = xf @ wgate_c.float().T
    cutoff = seqlen - seqlen % ratio
    assert cutoff == seqlen, "oracle liczy przypadek bez reszty"

    kv = kv.unflatten(1, (-1, ratio))
    score = score.unflatten(1, (-1, ratio)) + ape
    if overlap:
        b, n = kv.size(0), kv.size(1)
        kv2 = kv.new_zeros((b, n, 2 * ratio, d))
        kv2[:, :, ratio:] = kv[:, :, :, d:]
        kv2[:, 1:, :ratio] = kv[:, :-1, :, :d]
        sc2 = score.new_full((b, n, 2 * ratio, d), float("-inf"))
        sc2[:, :, ratio:] = score[:, :, :, d:]
        sc2[:, 1:, :ratio] = score[:, :-1, :, :d]
        kv, score = kv2, sc2
    kv = (kv * score.softmax(dim=2)).sum(dim=2)

    kv = rms_norm(kv, norm_w, eps)
    apply_rotary_emb(kv[..., -rope_dim:], freqs_cis[:cutoff:ratio])
    kv[..., :-rope_dim] = act_quant_inplace(kv[..., :-rope_dim], 64)
    return kv


comp = f"{p}.compressor"
c_wkv = torch.from_numpy(load_matrix(f"{comp}.wkv.weight").copy())
c_wgate = torch.from_numpy(load_matrix(f"{comp}.wgate.weight").copy())
c_ape = torch.from_numpy(load_vector(f"{comp}.ape").copy()).float()
c_norm = torch.from_numpy(load_vector(f"{comp}.norm.weight").copy()).float()
comp_out = compressor_prefill(x, ratio, c_wkv, c_wgate, c_ape, c_norm,
                              freqs_cis, head_dim, rope_head_dim, eps)

# --- indekser rzadkiej uwagi -------------------------------------------------

E2M1_VALUES = torch.tensor([0., .5, 1., 1.5, 2., 3., 4., 6.])


def fp4_act_quant_inplace(x, block=32):
    """Symulacja kwantyzacji aktywacji do FP4 (E2M1) ze skala zaokraglona do
    potegi dwojki — odpowiednik fp4_act_quant(..., inplace=True)."""
    shape = x.shape
    xg = x.reshape(-1, shape[-1] // block, block).float()
    amax = xg.abs().amax(-1, keepdim=True).clamp_min(6 * 2.0**-126)
    s = torch.pow(2.0, torch.ceil(torch.log2(amax / 6.0)))
    scaled = (xg / s).clamp(-6.0, 6.0)
    sign = torch.sign(scaled)
    mag = scaled.abs()
    idx = (mag.unsqueeze(-1) - E2M1_VALUES).abs().argmin(dim=-1)
    q = sign * E2M1_VALUES[idx]
    return (q * s).reshape(shape).to(x.dtype)


def hadamard(x):
    """Szybka transformata Walsha-Hadamarda po ostatnim wymiarze, znormalizowana
    przez 1/sqrt(n) — odpowiednik rotate_activation."""
    n = x.size(-1)
    assert n & (n - 1) == 0, n
    y = x.clone().float()
    step = 1
    while step < n:
        y = y.reshape(*y.shape[:-1], n // (2 * step), 2, step)
        a = y[..., 0, :].clone()
        b = y[..., 1, :].clone()
        y[..., 0, :] = a + b
        y[..., 1, :] = a - b
        y = y.reshape(*y.shape[:-3], n)
        step *= 2
    return (y * n**-0.5).to(x.dtype)


def indexer_compressor_prefill(x, ratio, wkv_c, wgate_c, ape, norm_w, freqs_cis,
                               head_dim, rope_dim, eps):
    """Kompresor indeksera: ta sama matematyka co zwykly, ale wyjscie przechodzi
    przez rotacje Hadamarda i kwantyzacje FP4 zamiast FP8."""
    overlap = ratio == 4
    d = head_dim
    xf = x.float()
    kv = (xf @ wkv_c.float().T).unflatten(1, (-1, ratio))
    score = (xf @ wgate_c.float().T).unflatten(1, (-1, ratio)) + ape
    if overlap:
        b, n = kv.size(0), kv.size(1)
        kv2 = kv.new_zeros((b, n, 2 * ratio, d))
        kv2[:, :, ratio:] = kv[:, :, :, d:]
        kv2[:, 1:, :ratio] = kv[:, :-1, :, :d]
        sc2 = score.new_full((b, n, 2 * ratio, d), float("-inf"))
        sc2[:, :, ratio:] = score[:, :, :, d:]
        sc2[:, 1:, :ratio] = score[:, :-1, :, :d]
        kv, score = kv2, sc2
    kv = (kv * score.softmax(dim=2)).sum(dim=2)
    kv = rms_norm(kv, norm_w, eps)
    apply_rotary_emb(kv[..., -rope_dim:], freqs_cis[::ratio][:kv.size(1)])
    kv = hadamard(kv.bfloat16()).float()
    return fp4_act_quant_inplace(kv, 32)


index_n_heads = cfg["index_n_heads"]
index_head_dim = cfg["index_head_dim"]
index_topk = cfg["index_topk"]
ix = f"{p}.indexer"
i_wq_b = torch.from_numpy(load_matrix(f"{ix}.wq_b.weight").copy())
i_wproj = torch.from_numpy(load_matrix(f"{ix}.weights_proj.weight").copy())
i_wkv = torch.from_numpy(load_matrix(f"{ix}.compressor.wkv.weight").copy())
i_wgate = torch.from_numpy(load_matrix(f"{ix}.compressor.wgate.weight").copy())
i_ape = torch.from_numpy(load_vector(f"{ix}.compressor.ape").copy()).float()
i_norm = torch.from_numpy(load_vector(f"{ix}.compressor.norm.weight").copy()).float()

iq = (qr @ i_wq_b.T).unflatten(-1, (index_n_heads, index_head_dim))
apply_rotary_emb(iq[..., -rope_head_dim:], freqs_cis)
iq = hadamard(iq.bfloat16()).float()
iq = fp4_act_quant_inplace(iq, 32)
index_kv = indexer_compressor_prefill(x, ratio, i_wkv, i_wgate, i_ape, i_norm,
                                      freqs_cis, index_head_dim, rope_head_dim, eps)
softmax_scale = index_head_dim ** -0.5
iw = (x.float() @ i_wproj.float().T) * (softmax_scale * index_n_heads ** -0.5)
index_score = torch.einsum("bshd,btd->bsht", iq, index_kv)
index_score = (index_score.relu() * iw.unsqueeze(-1)).sum(dim=2)

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

# --- rzadka uwaga po zebranych indeksach ------------------------------------

window_size = cfg["sliding_window"]
attn_sink = torch.from_numpy(load_vector(f"{p}.attn_sink").copy()).float()

# Indeksy okna przesuwnego (prefill): przyczynowe, wzgledem bufora KV tokenow.
base = torch.arange(SEQLEN).unsqueeze(1)
win_idx = (base - window_size + 1).clamp(0) + torch.arange(min(SEQLEN, window_size))
win_idx = torch.where(win_idx > base, -1, win_idx)

# Indeksy skompresowane: wpis `n` widoczny dopiero gdy (t+1)//ratio > n.
offset = SEQLEN
cmp_idx = torch.arange(SEQLEN // ratio).repeat(SEQLEN, 1)
cmp_mask = cmp_idx >= torch.arange(1, SEQLEN + 1).unsqueeze(1) // ratio
cmp_idx = torch.where(cmp_mask, -1, cmp_idx + offset)
topk_idxs = torch.cat([win_idx, cmp_idx], dim=-1)

kv_full = torch.cat([kv.squeeze(0), comp_out.squeeze(0)], dim=0)
softmax_scale_attn = head_dim ** -0.5
qh = q.squeeze(0)                       # [S, heads, head_dim]
sparse_out = torch.zeros_like(qh)
for t in range(SEQLEN):
    idxs = topk_idxs[t]
    valid = idxs[idxs >= 0]
    keys = kv_full[valid]               # [K, head_dim]
    scores = torch.einsum("hd,kd->hk", qh[t].float(), keys.float()) * softmax_scale_attn
    mx = scores.amax(dim=-1)
    exp = torch.exp(scores - mx.unsqueeze(-1))
    denom = exp.sum(dim=-1) + torch.exp(attn_sink - mx)
    sparse_out[t] = (exp @ keys.float()) / denom.unsqueeze(-1)

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
    f.write(struct.pack("<15i", SEQLEN, dim, n_heads, head_dim, rope_head_dim,
                        GATE_LAYER, TOPK, N_EXPERTS, o_groups, o_lora_rank, ratio,
                        index_n_heads, index_head_dim, window_size,
                        topk_idxs.size(-1)))
    for t in (x, qr, q, kv):
        f.write(t.detach().float().contiguous().numpy().tobytes())
    f.write(gx_t.contiguous().numpy().tobytes())
    f.write(indices.to(torch.int32).contiguous().numpy().tobytes())
    f.write(weights.float().contiguous().numpy().tobytes())
    f.write(expert_out.float().contiguous().numpy().tobytes())
    f.write(attn_in.float().contiguous().numpy().tobytes())
    f.write(attn_out.float().contiguous().numpy().tobytes())
    f.write(comp_out.float().contiguous().numpy().tobytes())
    f.write(index_kv.float().contiguous().numpy().tobytes())
    f.write(index_score.float().contiguous().numpy().tobytes())
    f.write(topk_idxs.to(torch.int32).contiguous().numpy().tobytes())
    f.write(sparse_out.float().contiguous().numpy().tobytes())
print("rzadka uwaga: abs mean", sparse_out.abs().mean().item(), "indeksow", topk_idxs.size(-1))
print("indekser: score", tuple(index_score.shape), "abs mean", index_score.abs().mean().item())
print("kompresor: wpisow", comp_out.size(1), "abs mean", comp_out.abs().mean().item())
print("bramka: pierwsze indeksy", indices[0].tolist(), "wagi", [round(v,4) for v in weights[0].tolist()])
print(f"zapisano {out}: seq={SEQLEN} dim={dim} heads={n_heads} hd={head_dim} rope={rope_head_dim} ratio={ratio}")
print("q  abs mean", q.abs().mean().item(), "kv abs mean", kv.abs().mean().item())
