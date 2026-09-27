// ===== File: dense_exec.rs — the dense vocabulary, executed on Metal =====
//
// Everything in this file touches the device. That is why it lives HERE and not
// next to the model: a model holding buffers is a model for one card, and this
// repository already paid for that twice (docs/PRZEGLAD_UKLADU.md).
//
// The model above sends `Op` and holds `WeightId`. This is where an id becomes
// a buffer, an op becomes a dispatch, and a choice between kernel forms becomes
// a lookup in the variant registry.
//
// Two properties are deliberate:
//
//   * A whole step goes into ONE command buffer. Around 500 dispatches at
//     0.61 us each is 0.3 ms of overhead per token; the same work as separate
//     command buffers would be 10 ms, and as host round trips, 47 ms
//     (docs/pomiary/eks-a1-a3-apple-m4.md).
//   * Weights are uploaded quantized and dequantized inside the kernels. A
//     dequantized copy of a 7B checkpoint would be 16 GB against 4.2, and
//     reading the weights once IS the cost of a decode step.

#[cfg(forge_ane)]
use std::cell::Cell;
use std::cell::RefCell;
#[cfg(forge_ane)]
use std::path::Path;
use std::sync::Arc;

use half::f16;

use forge_formats::affine::{to_affine_triple, AffineTriple};
use forge_graph::{
    Act, ExecSpec, Executor, Op, PackedWeight, QuantWeight, Tile, WeightId, WeightStore,
};
use forge_hal::{DevBuffer, Device, Event, KernelHandle, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_types::{DType, DenseShape, ForgeError, MemKind, QuantKind, Result};

#[cfg(forge_ane)]
use crate::ane_matmul::ComputeUnits;
#[cfg(forge_ane)]
use crate::ane_matmul::{AneBindingLite, AneLoadReport, AneMatmul, AneRole, AneStats};
use crate::cpu_matmul::{BlockOperands, CpuMatmul, Operands};
use crate::msl::{self, OutDtype, ScaleDtype};
use crate::variant::{
    self, AttentionForm, MatmulForm, Problem, RowSplit, ATTENTION_FORMS, MATMUL_FORMS,
};

/// Prompt tokens carried through the layers in one pass.
///
/// A multiple of the matrix-unit block, because that kernel writes whole
/// blocks: a chunk of 100 tokens stores 128 rows and the last 28 are padding
/// the scratch has to hold.
///
/// Not a round number picked for looks: past roughly this many tokens the
/// batched matmul stops winning, because its activation tile no longer fits in
/// cache and starts being re-read once per output row. Measured on M4 at 2.09x
/// for 128 and 0.72x for 512 (docs/pomiary/eks-a4-batched-matmul-m4.md).
const PREFILL_CHUNK: u32 = 1024;

/// A quantized weight: packed nibbles plus per-group scale and zero point.
struct Quantized {
    packed: DevBuffer,
    /// Bity czwarty i piąty, gdy `bits` wynosi sześć. Pusty bufor przy czterech
    /// — kernel czterobitowy go nie deklaruje, więc nie ma czego związać.
    high: Option<DevBuffer>,
    scales: DevBuffer,
    biases: DevBuffer,
    /// Cztery albo sześć. Jeden model potrafi mieć oba: Q4_K_M kładzie sześć
    /// bitów na attn_v, ffn_down i głowie, a cztery na reszcie.
    bits: u32,
    /// Wag na jedną skalę. Też własność TEJ wagi, nie modelu — Q4_K daje 32,
    /// Q6_K szesnaście, a MLX 64, i wszystkie trzy mogą wystąpić naraz.
    group: u32,
    rows: u32,
    cols: u32,
}

struct RawQuantized {
    blocks: DevBuffer,
    quant: QuantKind,
    global: Option<f32>,
    rows: u32,
    cols: u32,
}

/// Waga w postaci, w której wykonawca ją trzyma. Model widzi tylko `WeightId`.
enum Weight {
    Quant(Quantized),
    Q4K(RawQuantized),
    Plain(DevBuffer),
}

/// Cztery warianty jednej rodziny kerneli.
struct QuantPipes {
    /// [szerokość kodu: 4→0, 6→1][wyjście: f32→0, f16→1]
    by: [[KernelHandle; 2]; 2],
}

impl QuantPipes {
    fn get(&self, bits: u32, f16_out: bool) -> &KernelHandle {
        &self.by[usize::from(bits == 6)][usize::from(f16_out)]
    }
}

struct KQuantPipes {
    by: [KernelHandle; 2],
}

impl KQuantPipes {
    fn get(&self, f16_out: bool) -> &KernelHandle {
        &self.by[usize::from(f16_out)]
    }
}

struct Pipelines {
    /// Jedna rodzina, cztery warianty: dwie szerokości kodu razy dwa typy
    /// wyjścia. Trzymane w tablicy, a nie w czterech polach, bo wybór jest
    /// wyliczany, a nie pisany ręcznie w każdym miejscu wywołania.
    qmv: QuantPipes,
    qmm: QuantPipes,
    qmg: QuantPipes,
    k_qmv: KQuantPipes,
    k_qmm: KQuantPipes,
    k_qmg: KQuantPipes,
    k_embed: KernelHandle,
    rmsnorm: KernelHandle,
    silu_mul: KernelHandle,
    rope: KernelHandle,
    attn: KernelHandle,
    flash: KernelHandle,
    embed: KernelHandle,
    residual: KernelHandle,
    kv_append: KernelHandle,
    argmax: KernelHandle,
    /// Rozrzut ogona ANE z ciągłego bufora do slotu: [wyjście f32→0, f16→1].
    /// Kompilowane zawsze — są tanie, a dzięki temu ścieżka Metalowa bez
    /// ANE sprawdza to samo źródło, które ANE potem uruchamia.
    #[cfg_attr(not(forge_ane), allow(dead_code))]
    scatter: [KernelHandle; 2],
}

struct Scratch {
    h: DevBuffer,
    norm: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn: DevBuffer,
    /// The attention output gate. This executor refuses the operation that
    /// READS it and still gives it a buffer of its own: aliasing it onto
    /// `attn` would let the gate projection overwrite the answer it is meant
    /// to scale, which is a wrong number rather than a refusal.
    attn_gate: DevBuffer,
    proj: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    logits: DevBuffer,
    token: DevBuffer,
    /// Identyfikatory tokenów kafla, czytane przez kernel osadzeń.
    ids: DevBuffer,
}

/// The dense vocabulary on a Metal device.
pub struct MetalExec {
    /// Wszystkie wagi modelu. Model nosi indeksy, nie bufory — i to jest cała
    /// różnica między „opisem architektury" a „modelem dla tej karty".
    weights: Vec<Weight>,
    /// Cache klucza i wartości, po parze na warstwę.
    kv: Vec<(DevBuffer, DevBuffer)>,
    device: Arc<dyn Device>,
    stream: Stream,
    pipes: Pipelines,
    scratch: Scratch,
    /// Scratch for the CPU's share of a split product.
    cpu: RefCell<CpuMatmul>,
    /// Names the command buffer a split submitted.
    split_event: Event,
    cpu_share: bool,
    /// Ramię ANE, gdy podłączono katalog modeli CoreML (`attach_ane`).
    #[cfg(forge_ane)]
    ane: Option<AneMatmul>,
    /// Czy ogon ANE bierze udział w podziale. Domyślnie tak po podłączeniu.
    #[cfg(forge_ane)]
    ane_share: bool,
    /// Sloty wyjściowe części trwającej grupy, po indeksie części. Zapisywane
    /// w op-ie każdej części, czytane przy rozrzucie po `join`: część `gate`
    /// jest rozrzucana w op-ie `up`, a wtedy bieżącym slotem jest `up`.
    #[cfg(forge_ane)]
    ane_outs: RefCell<Vec<Option<(DevBuffer, bool)>>>,
    /// Czy ostrzeżenie o części grupy bez trwającego predict już poszło do
    /// logu — raz na wykonawcę, bo powtarzałoby się co warstwę.
    #[cfg(forge_ane)]
    ane_orphan_warned: Cell<bool>,
    shape: DenseShape,
    seq_cap: u32,
    /// Typ, w jakim skompilowano kernele kwantyzowane. Trzymany, żeby waga
    /// wgrana z innym typem parametrów odbiła się TU, a nie objawiła jako
    /// płynny, zły tekst.
    quant_params: DType,
}

impl MetalExec {
    /// Compiles the kernels this shape needs and allocates everything a step
    /// writes into. No weights yet — those arrive through `WeightStore`.
    pub fn new(device: Arc<dyn Device>, spec: ExecSpec) -> Result<Self> {
        let shape = spec.shape;
        let scales_dtype = quant_param_dtype(spec.quant_params)?;
        let norm_dtype = norm_weight_dtype(spec.norm_weights)?;
        let seq_cap = msl::ATTN_MAX_SEQ;

        let mut compile = |source: &str, entry: &str| -> Result<KernelHandle> {
            device.load_module(source.as_bytes())?.kernel(entry)
        };
        let pipes = Pipelines {
            qmv: quant_pipes(
                &mut compile,
                msl::qmv_affine_source,
                msl::qmv_affine_name,
                scales_dtype,
            )?,
            qmm: quant_pipes(
                &mut compile,
                msl::qmm_affine_source,
                msl::qmm_affine_name,
                scales_dtype,
            )?,
            qmg: quant_pipes(
                &mut compile,
                msl::qmg_affine_source,
                msl::qmg_affine_name,
                scales_dtype,
            )?,
            k_qmv: k_quant_pipes(
                &mut compile,
                msl::k_quants::q4_k_qmv_source,
                msl::k_quants::q4_k_qmv_name,
            )?,
            k_qmm: k_quant_pipes(
                &mut compile,
                msl::k_quants::q4_k_qmm_source,
                msl::k_quants::q4_k_qmm_name,
            )?,
            k_qmg: k_quant_pipes(
                &mut compile,
                msl::k_quants::q4_k_qmg_source,
                msl::k_quants::q4_k_qmg_name,
            )?,
            k_embed: compile(
                &msl::k_quants::q4_k_embed_source(),
                msl::k_quants::Q4_K_EMBED_NAME,
            )?,
            rmsnorm: compile(
                &msl::rmsnorm_source(norm_dtype),
                &msl::rmsnorm_name(norm_dtype),
            )?,
            silu_mul: compile(msl::SILU_MUL_SOURCE, msl::SILU_MUL_NAME)?,
            rope: compile(msl::ROPE_HALF_SPLIT_SOURCE, msl::ROPE_HALF_SPLIT_NAME)?,
            attn: compile(
                &msl::attn_decode_source(shape.head_dim),
                &msl::attn_decode_name(shape.head_dim),
            )?,
            flash: compile(
                &msl::flash_attn_source(shape.head_dim),
                &msl::flash_attn_name(shape.head_dim),
            )?,
            embed: compile(
                &msl::embed_gather_source(scales_dtype),
                &msl::embed_gather_name(scales_dtype),
            )?,
            residual: compile(msl::RESIDUAL_ADD_SOURCE, msl::RESIDUAL_ADD_NAME)?,
            kv_append: compile(msl::KV_APPEND_SOURCE, msl::KV_APPEND_NAME)?,
            argmax: compile(msl::ARGMAX_SOURCE, msl::ARGMAX_NAME)?,
            scatter: [
                compile(
                    &msl::scatter_cols_source(OutDtype::F32),
                    &msl::scatter_cols_name(OutDtype::F32),
                )?,
                compile(
                    &msl::scatter_cols_source(OutDtype::F16),
                    &msl::scatter_cols_name(OutDtype::F16),
                )?,
            ],
        };

        let f16b =
            |elems: u32| device.alloc(elems as usize * 2, MemKind::Device, Pool::Activations);
        let f32b =
            |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        // Wszystko poza logitami ma miejsce na cały kafel prefillu: dekodowanie
        // używa pierwszego wiersza tych samych buforów. Logity liczymy tylko dla
        // ostatniego tokenu, więc zostają jednym wierszem — 32 tys. kolumn razy
        // 128 byłoby 16 MB na coś, z czego czytamy 1/128.
        let n = PREFILL_CHUNK;
        let scratch = Scratch {
            h: f16b(n * shape.hidden)?,
            norm: f16b(n * shape.hidden)?,
            q: f16b(n * shape.attn_width())?,
            k: f16b(n * shape.kv_width())?,
            v: f16b(n * shape.kv_width())?,
            attn: f16b(n * shape.attn_width())?,
            attn_gate: f16b(n * shape.attn_width())?,
            proj: f32b(n * shape.hidden)?,
            gate: f16b(n * shape.inter)?,
            up: f16b(n * shape.inter)?,
            act: f16b(n * shape.inter)?,
            logits: f32b(shape.vocab)?,
            token: f32b(1)?,
            ids: device.alloc(n as usize * 4, MemKind::Device, Pool::Activations)?,
        };

        let kv_bytes = (shape.kv_heads * seq_cap * shape.head_dim) as usize * 2;
        let mut kv = Vec::with_capacity(shape.layers as usize);
        for _ in 0..shape.layers {
            kv.push((
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
            ));
        }

        let stream = device.create_stream()?;
        let split_event = device.create_event()?;
        Ok(Self {
            weights: Vec::new(),
            kv,
            device,
            stream,
            pipes,
            scratch,
            cpu: RefCell::new(CpuMatmul::new()),
            split_event,
            cpu_share: true,
            #[cfg(forge_ane)]
            ane: None,
            #[cfg(forge_ane)]
            ane_share: false,
            #[cfg(forge_ane)]
            ane_outs: RefCell::new(Vec::new()),
            #[cfg(forge_ane)]
            ane_orphan_warned: Cell::new(false),
            shape,
            seq_cap,
            quant_params: spec.quant_params,
        })
    }

    /// Turns the CPU's share of a large product on or off. On by default.
    ///
    /// Default ON because it is measured to win on every Apple part this runs
    /// on: prefill leaves the GPU at 77% of its matrix ceiling with bandwidth
    /// to spare, so the CPU adds throughput instead of taking it
    /// (docs/pomiary/eks-a7-cpu-gpu-wspolbieznie-m4.md).
    ///
    /// Reasons a caller might still turn it off:
    ///
    ///   * the CPU is wanted for something else — the two DO compete, and a
    ///     concurrent load costs this path up to a third of its rate;
    ///   * power, not speed, is the budget: the split trades watts for latency;
    ///   * pinning down where a numerical difference comes from, which is what
    ///     the correctness gate uses it for.
    ///
    /// Decode never takes this path whatever this is set to — the registry only
    /// picks the shared form for batches large enough to pay for it, and decode
    /// is bandwidth bound anyway: adding compute there measured -14%.
    pub fn set_cpu_share(&mut self, on: bool) {
        self.cpu_share = on;
    }

    /// Podłącza katalog modeli CoreML (`manifest.json` + `.mlmodelc`) jako
    /// trzecie ramię podziału. Wiązania mówią, która waga wykonawcy jest
    /// którą projekcją której warstwy; grupy bez kompletu wiązań są pomijane.
    ///
    /// `FORGE_ANE_LAYERS` (np. `0`, `0,5`, `0-3`) ogranicza wiązania do
    /// wybranych warstw — do bisekcji, nie do produkcji. `FORGE_ANE_SHAPES`
    /// (np. `256`) zawęża kształty T (patrz `AneMatmul::load`).
    #[cfg(forge_ane)]
    pub fn attach_ane(&mut self, dir: &Path, bindings: &[AneBindingLite]) -> Result<AneLoadReport> {
        self.ensure_ane_joined()?;
        let filtered: Vec<AneBindingLite> = match std::env::var("FORGE_ANE_LAYERS") {
            Ok(spec) => {
                let keep = parse_layer_set(&spec)?;
                bindings
                    .iter()
                    .copied()
                    .filter(|b| keep.contains(&b.layer))
                    .collect()
            }
            Err(_) => bindings.to_vec(),
        };
        let (ane, report) = AneMatmul::load(
            &*self.device,
            dir,
            &filtered,
            ComputeUnits::CpuAndNeuralEngine,
            PREFILL_CHUNK,
        )?;
        let max_parts = (0..report.models)
            .map(|g| ane.group(g).parts.len())
            .max()
            .unwrap_or(0);
        self.ane_outs = RefCell::new(vec![None; max_parts]);
        self.ane = Some(ane);
        self.ane_share = true;
        Ok(report)
    }

    /// Włącza lub wyłącza ogon ANE. Bez podłączonego katalogu nie ma skutku.
    #[cfg(forge_ane)]
    pub fn set_ane_share(&mut self, on: bool) {
        self.ane_share = on;
    }

    #[cfg(forge_ane)]
    pub fn ane_attached(&self) -> bool {
        self.ane.is_some()
    }

    /// Liczniki ramienia ANE od ostatniego zerowania (domyślne bez ramienia).
    #[cfg(forge_ane)]
    pub fn ane_stats(&self) -> AneStats {
        self.ane.as_ref().map(AneMatmul::stats).unwrap_or_default()
    }

    #[cfg(forge_ane)]
    pub fn reset_ane_stats(&self) {
        if let Some(ane) = &self.ane {
            ane.reset_stats();
        }
    }

    /// Ogon ANE dla tej wagi przy tym wsadzie; zero bez ramienia.
    ///
    /// Zero także wtedy, gdy ramię NIE MOŻE tego ogona policzyć: funkcja nie
    /// daje się doładować albo część grupy (Middle/Last) nie ma trwającego
    /// predict, bo jej pierwsza część już odpadła. Decyzja zapada TU, przed
    /// podziałem, bo po nim GPU i CPU liczyłyby już bez tych wierszy.
    /// Odmowa to ostrzeżenie w logu i pełne wiersze na GPU+CPU — poprawny
    /// wynik, tylko wolniejszy.
    #[cfg(forge_ane)]
    fn ane_rows_for(&self, w: WeightId, tokens: u32) -> u32 {
        let Some(ane) = &self.ane else { return 0 };
        if !self.ane_share {
            return 0;
        }
        let rows = ane.ane_rows(w, tokens);
        if rows == 0 {
            return 0;
        }
        match ane.role(w) {
            Some(AneRole::First | AneRole::Solo) => {
                if let Err(e) = ane.ensure_loaded(w, tokens) {
                    tracing::warn!(
                        "ANE: waga {} przy {tokens} tokenach: doładowanie funkcji nie powiodło \
                         się, wiersze liczy GPU+CPU: {e}",
                        w.0
                    );
                    return 0;
                }
                rows
            }
            Some(AneRole::Middle | AneRole::Last) => {
                if ane.pending() == ane.group_of(w) {
                    return rows;
                }
                if !self.ane_orphan_warned.replace(true) {
                    tracing::warn!(
                        "ANE: waga {} to część grupy bez trwającego predict (pierwsza część \
                         odpadła) — wiersze liczy GPU+CPU; to ostrzeżenie pojawia się raz",
                        w.0
                    );
                }
                0
            }
            None => 0,
        }
    }

    #[cfg(not(forge_ane))]
    fn ane_rows_for(&self, _w: WeightId, _tokens: u32) -> u32 {
        0
    }

    /// Bezpiecznik: gdy jakaś grupa jeszcze liczy, dołącz ją i rozrzuć wynik.
    /// Wołany przed każdym op-em spoza grupy i przed każdym odczytem.
    #[cfg(forge_ane)]
    fn ensure_ane_joined(&self) -> Result<()> {
        match &self.ane {
            Some(ane) if ane.pending().is_some() => self.ane_finish(),
            _ => Ok(()),
        }
    }

    #[cfg(not(forge_ane))]
    fn ensure_ane_joined(&self) -> Result<()> {
        Ok(())
    }

    /// Przed op-em: jeśli to nie jest mnożenie wagą z trwającej grupy,
    /// trwająca grupa musi się skończyć TU — inaczej jej rozrzut trafiłby
    /// za konsumenta jej slotów.
    #[cfg(forge_ane)]
    fn ane_guard(&self, op: &Op) -> Result<()> {
        let Some(ane) = &self.ane else { return Ok(()) };
        let Some(pending) = ane.pending() else {
            return Ok(());
        };
        if let Op::MatMul { w, .. } = op {
            if ane.group_of(*w) == Some(pending) {
                return Ok(());
            }
        }
        self.ane_finish()
    }

    /// Początek udziału ANE w mnożeniu wagą `w`: zapamiętuje slot wyjściowy
    /// części, a dla pierwszej części grupy zleca predict.
    ///
    /// Wołane PO `stream.synchronize()`: `x` jest wtedy zmaterializowane, a
    /// rozrzut poprzedniej grupy wykonany, więc bufor wyjściowy ANE jest wolny.
    #[cfg(forge_ane)]
    fn ane_begin(
        &self,
        w: WeightId,
        tokens: u32,
        x: &DevBuffer,
        out: &DevBuffer,
        f16_out: bool,
    ) -> Result<()> {
        let ane = self
            .ane
            .as_ref()
            .ok_or_else(|| ForgeError::Other("ANE: ogon bez ramienia".into()))?;
        let (gi, pi) = ane
            .part_of(w)
            .ok_or_else(|| ForgeError::Other(format!("ANE: waga {} bez grupy", w.0)))?;
        match ane.role(w) {
            Some(AneRole::First | AneRole::Solo) => {
                // SAFETY: slot `x` ma PREFILL_CHUNK wierszy, czyli co najmniej
                // T' (<= 1024) — wiersze ponad `tokens` to śmieci w ważnej
                // pamięci. Strumień jest zsynchronizowany, więc x jest gotowe
                // i nikt go nie pisze do końca grupy (następny zapis tego
                // slotu jest w op-ie, który `ane_guard` poprzedza joinem).
                // Zadanie trzyma klon uchwytu, więc slot żyje do `join`.
                unsafe { ane.start(w, tokens, x, 0)? };
            }
            Some(AneRole::Middle | AneRole::Last) => {
                if ane.pending() != Some(gi) {
                    return Err(ForgeError::Other(format!(
                        "ANE: część {pi} grupy {gi} bez trwającego predict tej grupy"
                    )));
                }
            }
            None => return Err(ForgeError::Other(format!("ANE: waga {} bez roli", w.0))),
        }
        self.ane_outs.borrow_mut()[pi] = Some((out.clone(), f16_out));
        Ok(())
    }

    /// Koniec udziału ANE w mnożeniu wagą `w`: dla ostatniej części grupy
    /// czeka na predict i rozrzuca wynik do slotów wszystkich części.
    #[cfg(forge_ane)]
    fn ane_end(&self, w: WeightId) -> Result<()> {
        let Some(ane) = &self.ane else { return Ok(()) };
        match ane.role(w) {
            Some(AneRole::Last | AneRole::Solo) => self.ane_finish(),
            _ => Ok(()),
        }
    }

    /// `join` + rozrzut: każdą część grupy z ciągłego `[T', width]` f16 do
    /// jej slotu, w typie tego slotu, w otwartym buforze poleceń — czyli
    /// PRZED wszystkim, co ten slot potem czyta.
    ///
    /// Po błędzie (join albo brak slotu) sloty części są CZYSZCZONE: stary
    /// slot nie może przeżyć do następnej grupy i udawać jej wyjścia.
    #[cfg(forge_ane)]
    fn ane_finish(&self) -> Result<()> {
        let r = self.ane_finish_inner();
        if r.is_err() {
            self.ane_outs
                .borrow_mut()
                .iter_mut()
                .for_each(|s| *s = None);
        }
        r
    }

    #[cfg(forge_ane)]
    fn ane_finish_inner(&self) -> Result<()> {
        let ane = self
            .ane
            .as_ref()
            .ok_or_else(|| ForgeError::Other("ANE: join bez ramienia".into()))?;
        let done = ane.join()?;
        let group = ane.group(done.group_idx);
        let mut outs = self.ane_outs.borrow_mut();
        for (pi, part) in group.parts.iter().enumerate() {
            let (dst, f16_out) = outs[pi].take().ok_or_else(|| {
                ForgeError::Other(format!(
                    "ANE: L{} {}: część {pi} bez slotu wyjściowego — op tej części nie \
                     przeszedł przez ogon ANE",
                    group.layer, group.name
                ))
            })?;
            self.launch(
                &self.pipes.scatter[usize::from(f16_out)],
                LaunchArgs::new()
                    .buf(ane.out_buf())
                    .buf(&dst)
                    .scalar(group.out_width)
                    .scalar(part.out_col0)
                    .scalar(part.rows)
                    .scalar(part.ane0)
                    .scalar(part.ane_rows)
                    .scalar(done.tokens),
                msl::scatter_groups(part.ane_rows, done.tokens),
                msl::SCATTER_THREADS,
            )?;
        }
        Ok(())
    }
}

/// Najszerszy zakres `a-b` przyjmowany przez `FORGE_ANE_LAYERS`.
#[cfg(forge_ane)]
const MAX_LAYER_RANGE: u32 = 4096;

/// `0`, `0,5,7`, `0-3`, także mieszane: `0-3,7`.
#[cfg(forge_ane)]
fn parse_layer_set(spec: &str) -> Result<std::collections::HashSet<u32>> {
    let mut set = std::collections::HashSet::new();
    for item in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let bad = || ForgeError::Format(format!("FORGE_ANE_LAYERS: '{item}' to nie zakres"));
        match item.split_once('-') {
            Some((a, b)) => {
                let a: u32 = a.trim().parse().map_err(|_| bad())?;
                let b: u32 = b.trim().parse().map_err(|_| bad())?;
                // Zakres malejący to literówka, a ogromny to `HashSet` na
                // miliardy wpisów — żaden model nie ma tylu warstw.
                if b < a || b - a >= MAX_LAYER_RANGE {
                    return Err(ForgeError::Format(format!(
                        "FORGE_ANE_LAYERS: zakres '{item}' musi rosnąć i mieć mniej niż \
                         {MAX_LAYER_RANGE} warstw"
                    )));
                }
                set.extend(a..=b);
            }
            None => {
                set.insert(item.parse().map_err(|_| bad())?);
            }
        }
    }
    Ok(set)
}

/// Trwająca grupa ANE jest dołączana PRZED zwolnieniem pól: wątek roboczy
/// trzyma własne uchwyty buforów, więc to nie jest kwestia pamięci, ale
/// predict w toku nie powinien przeżyć wykonawcy, który go zlecił.
#[cfg(forge_ane)]
impl Drop for MetalExec {
    fn drop(&mut self) {
        let _ = self.ensure_ane_joined();
    }
}

impl WeightStore for MetalExec {
    /// Kernele Metalowe indeksują trzy osobne tablice, więc źródło oddające
    /// bloki jest przepisywane TU. Model tego nie robi, bo nie zna kerneli —
    /// a to samo źródło idzie na CUDA bez ani jednego przepisania.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId> {
        match w {
            QuantWeight::Affine(t) => self.put_affine(t),
            QuantWeight::Packed(p) if p.quant == forge_types::QuantKind::Q4K => self.put_q4k(p),
            QuantWeight::Packed(p) => {
                let t = to_affine_triple(&p.planes.codes, p.quant, p.rows, p.cols)?;
                self.put_affine(t)
            }
        }
    }

    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId> {
        let buf = upload(&*self.device, &bytes)?;
        self.weights.push(Weight::Plain(buf));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl MetalExec {
    fn put_q4k(&mut self, p: PackedWeight) -> Result<WeightId> {
        if p.planes.scales.is_some() || p.planes.global.is_some() {
            return Err(ForgeError::Format(
                "Q4_K: compactny kernel wymaga jednego bufora bloków".into(),
            ));
        }
        if p.dtype != DType::U8 || !p.cols.is_multiple_of(256) {
            return Err(ForgeError::Unsupported(format!(
                "Q4_K: oczekiwano bloków U8 o szerokości podzielnej przez 256, jest {:?} [{}x{}]",
                p.dtype, p.rows, p.cols
            )));
        }
        let want = p.rows * p.cols / 256 * 144;
        if p.planes.codes.len() != want {
            return Err(ForgeError::Format(format!(
                "Q4_K: {} B, oczekiwano {want}",
                p.planes.codes.len()
            )));
        }
        self.weights.push(Weight::Q4K(RawQuantized {
            blocks: upload(&*self.device, &p.planes.codes)?,
            quant: p.quant,
            global: p.planes.global,
            rows: p.rows as u32,
            cols: p.cols as u32,
        }));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }

    fn put_affine(&mut self, t: AffineTriple) -> Result<WeightId> {
        // Trzy właściwości sprawdzane TU, przy użyciu, a nie zakładane przy
        // wołaniu. Każda z nich była już raz źródłem poprawnie wyglądającego,
        // złego tekstu.
        if t.param_dtype != self.quant_params {
            return Err(ForgeError::Format(format!(
                "waga ma parametry {:?}, a kernele skompilowano dla {:?}",
                t.param_dtype, self.quant_params
            )));
        }
        if !t.cols.is_multiple_of(t.group) {
            return Err(ForgeError::Unsupported(format!(
                "{} kolumn nie dzieli się na grupy po {}",
                t.cols, t.group
            )));
        }
        let high = match t.bits {
            4 => None,
            6 => Some(upload(&*self.device, bytemuck::cast_slice(&t.high))?),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{other} bitów na wagę, a kernele znają cztery i sześć"
                )))
            }
        };
        self.weights.push(Weight::Quant(Quantized {
            packed: upload(&*self.device, bytemuck::cast_slice(&t.packed))?,
            high,
            scales: upload(&*self.device, &t.scales)?,
            biases: upload(&*self.device, &t.biases)?,
            bits: t.bits,
            group: t.group as u32,
            rows: t.rows as u32,
            cols: t.cols as u32,
        }));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl Executor for MetalExec {
    /// Jedno wejście dla całego słownictwa. Rozgałęzienie jest TU, w wykonawcy,
    /// a nie w modelu — dzięki temu backend, który czegoś nie umie, odmawia w
    /// jednym miejscu, zamiast implementować zaślepkę.
    fn run(&self, op: &Op) -> Result<()> {
        let step = match op {
            Op::Embed { step, .. }
            | Op::RmsNorm { step, .. }
            | Op::MatMul { step, .. }
            | Op::HeadNorm { step, .. }
            | Op::Rope { step, .. }
            | Op::KvAppend { step, .. }
            | Op::Attention { step, .. }
            | Op::SiluMul { step }
            | Op::MoeFfn { step, .. }
            | Op::SigmoidMul { step, .. }
            | Op::DeltaNet { step, .. }
            | Op::FusedNormMatMul { step, .. }
            | Op::FusedMatMulResidual { step, .. }
            | Op::Residual { step, .. }
            | Op::LogitsOfLast { step, .. } => step,
        };
        // Cache jest jedną ciągłą połacią na warstwę, więc ten wykonawca trzyma
        // JEDNĄ sekwencję i mówi to przez `Tile::max_lanes`. Krok z wieloma
        // lane'ami odbija się TU, zamiast policzyć się nad cudzym kontekstem —
        // stronicowanie po tej stronie to osobna praca i osobne kernele MSL.
        let pos = match step.lanes() {
            [lane] if lane.slot == 0 => lane.pos,
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{} lane'ów, a ten wykonawca trzyma jeden ciągły cache",
                    other.len()
                )))
            }
        };
        let tokens = step.tokens();
        #[cfg(forge_ane)]
        self.ane_guard(op)?;
        match op {
            Op::Embed { table, tokens, .. } => self.op_embed(*table, tokens),
            Op::RmsNorm { out, x, w, .. } => self.op_rmsnorm(*out, *x, *w, tokens),
            Op::MatMul { out, w, x, .. } => self.op_matmul(*out, *w, *x, tokens),
            Op::FusedNormMatMul {
                out,
                w,
                norm_w,
                x,
                step,
            } => {
                self.run(&Op::RmsNorm {
                    out: Act::Norm,
                    x: *x,
                    w: *norm_w,
                    step: step.clone(),
                })?;
                self.run(&Op::MatMul {
                    out: *out,
                    w: *w,
                    x: Act::Norm,
                    step: step.clone(),
                })
            }
            Op::FusedMatMulResidual { w, x, step } => {
                self.run(&Op::MatMul {
                    out: Act::Proj,
                    w: *w,
                    x: *x,
                    step: step.clone(),
                })?;
                self.run(&Op::Residual {
                    src: Act::Proj,
                    step: step.clone(),
                })
            }
            Op::HeadNorm { act, w, heads, .. } => self.op_head_norm(*act, *w, *heads, tokens),
            // Refused in one place rather than stubbed. The MSL catalogue has
            // no kernel that reads its expert id on device, and writing one
            // needs the machine with the Mac; a fallback that ran the experts
            // some other way would be a second implementation of the routing,
            // which is what this whole layout exists to avoid.
            Op::MoeFfn { .. } => Err(ForgeError::Unsupported(
                "MoeFfn: ścieżka Metalowa nie ma kerneli mieszanki ekspertów".into(),
            )),
            // Refused rather than written blind. The kernel is two lines of
            // MSL, and there is no Apple part here to run them on — an
            // unverified elementwise kernel in the middle of attention would
            // produce fluent, wrong text on the one machine nobody is testing.
            Op::SigmoidMul { .. } => Err(ForgeError::Unsupported(
                "SigmoidMul: ścieżka Metalowa nie ma kernela bramki uwagi".into(),
            )),
            Op::DeltaNet { .. } => Err(ForgeError::Unsupported(
                "DeltaNet: ścieżka Metalowa nie ma kerneli miksera rekurencyjnego".into(),
            )),
            Op::Rope { act, heads, .. } => self.op_rope(*act, *heads, pos, tokens),
            Op::KvAppend { layer, .. } => self.op_kv_append(*layer, pos, tokens),
            Op::Attention { layer, .. } => self.op_attention(*layer, pos + tokens, tokens),
            Op::SiluMul { .. } => self.op_silu_mul(tokens),
            Op::Residual { src, .. } => self.op_residual(*src, tokens),
            Op::LogitsOfLast { w, x, .. } => self.op_logits_of_last(*w, *x, tokens),
        }
    }

    fn sync(&self) -> Result<()> {
        self.ensure_ane_joined()?;
        self.stream.synchronize()
    }

    fn read(&self, act: Act, len: usize) -> Result<Vec<f32>> {
        self.ensure_ane_joined()?;
        self.stream.synchronize()?;
        if Self::is_half(act) {
            let mut raw = vec![0u8; len * 2];
            self.device.read(self.buf(act), 0, &mut raw)?;
            return Ok(raw
                .chunks_exact(2)
                .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
                .collect());
        }
        let mut raw = vec![0u8; len * 4];
        self.device.read(self.buf(act), 0, &mut raw)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }

    fn argmax(&self, act: Act, lanes: usize) -> Result<Vec<u32>> {
        if lanes != 1 {
            return Err(ForgeError::Unsupported(format!(
                "wybór dla {lanes} lane'ów, a ten wykonawca trzyma jeden"
            )));
        }
        self.ensure_ane_joined()?;
        self.launch(
            &self.pipes.argmax,
            LaunchArgs::new()
                .buf(&self.scratch.token)
                .buf(self.buf(act))
                .scalar(self.shape.vocab),
            1,
            msl::ARGMAX_THREADS,
        )?;
        self.stream.synchronize()?;
        let mut raw = [0u8; 4];
        self.device.read(&self.scratch.token, 0, &mut raw)?;
        Ok(vec![u32::from_le_bytes(raw)])
    }

    fn seq_cap(&self) -> u32 {
        self.seq_cap
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: PREFILL_CHUNK,
            max_lanes: 1,
            align: msl::QMG_BM,
        }
    }
}

impl MetalExec {
    /// Bufor tego slotu.
    fn buf(&self, a: Act) -> &DevBuffer {
        match a {
            Act::Hidden => &self.scratch.h,
            Act::Norm => &self.scratch.norm,
            Act::Query => &self.scratch.q,
            Act::Key => &self.scratch.k,
            Act::Value => &self.scratch.v,
            Act::Attn => &self.scratch.attn,
            Act::AttnGate => &self.scratch.attn_gate,
            Act::Proj => &self.scratch.proj,
            Act::Gate => &self.scratch.gate,
            Act::Up => &self.scratch.up,
            Act::Activated => &self.scratch.act,
            Act::Logits => &self.scratch.logits,
        }
    }

    /// Czy slot trzyma połówkową precyzję. Własność slotu, nie wywołania.
    fn is_half(a: Act) -> bool {
        !matches!(a, Act::Proj | Act::Logits)
    }

    /// Osadzenia tokenów kafla do `Act::Hidden`.
    fn op_embed(&self, table: WeightId, tokens: &[u32]) -> Result<()> {
        let ids: Vec<u8> = tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
        self.device.write(&ids, &self.scratch.ids, 0)?;
        let n = tokens.len() as u32;
        match self.weight(table)? {
            Weight::Quant(w) => self.launch(
                &self.pipes.embed,
                LaunchArgs::new()
                    .buf(self.buf(Act::Hidden))
                    .buf(&w.packed)
                    .buf(&w.scales)
                    .buf(&w.biases)
                    .buf(&self.scratch.ids)
                    .scalar(self.shape.hidden)
                    .scalar(w.group)
                    .scalar(n),
                msl::elementwise_groups(n * self.shape.hidden),
                msl::ELEMENTWISE_THREADS,
            ),
            Weight::Q4K(w) => self.launch(
                &self.pipes.k_embed,
                LaunchArgs::new()
                    .buf(self.buf(Act::Hidden))
                    .buf(&w.blocks)
                    .buf(&self.scratch.ids)
                    .scalar(self.shape.hidden)
                    .scalar(n),
                msl::elementwise_groups(n * self.shape.hidden),
                msl::ELEMENTWISE_THREADS,
            ),
            Weight::Plain(_) => Err(ForgeError::Other(
                "embedding wymaga wagi kwantyzowanej".into(),
            )),
        }
    }

    /// Głowa wyjściowa dla ostatniego tokenu kafla.
    fn op_logits_of_last(&self, w: WeightId, x: Act, tokens: u32) -> Result<()> {
        let last = (tokens - 1) as usize * self.shape.hidden as usize * 2;
        match self.weight(w)? {
            Weight::Quant(weight) => self.gemv(
                self.pipes.qmv.get(weight.bits, false),
                self.buf(Act::Logits),
                weight,
                self.buf(x),
                last,
            ),
            Weight::Q4K(weight) => self.k_gemv(
                self.pipes.k_qmv.get(false),
                self.buf(Act::Logits),
                weight,
                self.buf(x),
                last,
            ),
            Weight::Plain(_) => Err(ForgeError::Other(
                "logity wymagają wagi kwantyzowanej".into(),
            )),
        }
    }

    /// `out = norm(x) * waga`.
    fn op_rmsnorm(&self, out: Act, x: Act, w: WeightId, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.rmsnorm,
            LaunchArgs::new()
                .buf(self.buf(out))
                .buf(self.buf(x))
                .buf(self.plain(w)?)
                .scalar(self.shape.hidden)
                .scalar(self.shape.eps),
            tokens,
            msl::RMSNORM_THREADS,
        )
    }

    /// Ta sama norma, ale wierszem jest GŁOWICA — ten sam potok, inny podział
    /// bufora. Liczona w miejscu: każda grupa czyta i zapisuje swój wiersz.
    fn op_head_norm(&self, act: Act, w: WeightId, heads: u32, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.rmsnorm,
            LaunchArgs::new()
                .buf(self.buf(act))
                .buf(self.buf(act))
                .buf(self.plain(w)?)
                .scalar(self.shape.head_dim)
                .scalar(self.shape.eps),
            tokens * heads,
            msl::RMSNORM_THREADS,
        )
    }

    /// `out = x * wagaᵀ`. Formę wybiera rejestr, typ zapisu wynika ze slotu.
    fn op_matmul(&self, out: Act, w: WeightId, x: Act, tokens: u32) -> Result<()> {
        match self.weight(w)? {
            Weight::Quant(weight) => self.matmul(
                self.buf(out),
                w,
                weight,
                self.buf(x),
                tokens,
                Self::is_half(out),
            ),
            Weight::Q4K(weight) => self.k_matmul(
                self.buf(out),
                weight,
                self.buf(x),
                tokens,
                Self::is_half(out),
            ),
            Weight::Plain(_) => Err(ForgeError::Other(
                "mnożenie wymaga wagi kwantyzowanej".into(),
            )),
        }
    }

    fn op_rope(&self, a: Act, heads: u32, pos: u32, tokens: u32) -> Result<()> {
        let threads = msl::ELEMENTWISE_THREADS;
        self.launch(
            &self.pipes.rope,
            LaunchArgs::new()
                .buf(self.buf(a))
                .scalar(heads)
                .scalar(self.shape.head_dim)
                .scalar(pos)
                .scalar(self.shape.rope_theta)
                .scalar(tokens),
            msl::rope_groups(heads, self.shape.head_dim, tokens, threads),
            threads,
        )
    }

    /// Dopisuje klucz i wartość tego kafla do cache'u warstwy.
    fn op_kv_append(&self, layer: usize, pos: u32, tokens: u32) -> Result<()> {
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        self.kv_append(kc, self.buf(Act::Key), pos, tokens)?;
        self.kv_append(vc, self.buf(Act::Value), pos, tokens)
    }

    /// `Hidden += src`. The kernel reads and writes the same index, so aliasing
    /// the output onto the input is safe and saves a buffer that would
    /// otherwise be copied every layer.
    fn op_residual(&self, src: Act, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.residual,
            LaunchArgs::new()
                .buf(&self.scratch.h)
                .buf(&self.scratch.h)
                .buf(self.buf(src))
                .scalar(tokens * self.shape.hidden),
            msl::elementwise_groups(tokens * self.shape.hidden),
            msl::ELEMENTWISE_THREADS,
        )
    }

    /// `Activated = silu(Gate) * Up`.
    fn op_silu_mul(&self, tokens: u32) -> Result<()> {
        let n = tokens * self.shape.inter;
        self.launch(
            &self.pipes.silu_mul,
            LaunchArgs::new()
                .buf(self.buf(Act::Activated))
                .buf(self.buf(Act::Gate))
                .buf(self.buf(Act::Up))
                .scalar(n),
            msl::silu_mul_groups(n),
            msl::SILU_MUL_THREADS,
        )
    }

    /// Uwaga nad cache'em warstwy. Formę wybiera rejestr, tak samo jak przy
    /// mnożeniu — model nie ma tu zdania.
    fn op_attention(&self, layer: usize, seq: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        let form = ATTENTION_FORMS
            .pick(&Problem::new(tokens, s.heads, s.head_dim))
            .ok_or_else(|| ForgeError::Unsupported("brak wariantu uwagi".into()))?;
        let (kernel, groups, threads) = match form.form {
            AttentionForm::Blocked if msl::flash_fits(tokens, s.head_dim) => (
                &self.pipes.flash,
                msl::flash_attn_groups(s.heads, tokens),
                msl::FLASH_THREADS,
            ),
            _ => (
                &self.pipes.attn,
                msl::attn_groups(s.heads, tokens),
                msl::ATTN_THREADS,
            ),
        };
        self.launch(
            kernel,
            LaunchArgs::new()
                .buf(self.buf(Act::Attn))
                .buf(self.buf(Act::Query))
                .buf(kc)
                .buf(vc)
                .scalar(s.heads)
                .scalar(s.kv_heads)
                .scalar(seq)
                .scalar(self.seq_cap)
                .scalar(s.attn_scale())
                .scalar(tokens),
            groups,
            threads,
        )
    }

    fn weight(&self, id: WeightId) -> Result<&Weight> {
        match self.weights.get(id.0 as usize) {
            Some(weight) => Ok(weight),
            _ => Err(ForgeError::Other(format!("brak wagi {}", id.0))),
        }
    }

    fn plain(&self, id: WeightId) -> Result<&DevBuffer> {
        match self.weights.get(id.0 as usize) {
            Some(Weight::Plain(b)) => Ok(b),
            _ => Err(ForgeError::Other(format!("waga {} nie jest zwykła", id.0))),
        }
    }

    fn gemv(
        &self,
        kernel: &KernelHandle,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        x_offset: usize,
    ) -> Result<()> {
        self.launch(
            kernel,
            weight_args(LaunchArgs::new().buf(out), w, x, x_offset)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group),
            msl::qmv_affine_4bit_groups(w.rows),
            msl::QMV_THREADS,
        )
    }

    fn k_gemv(
        &self,
        kernel: &KernelHandle,
        out: &DevBuffer,
        w: &RawQuantized,
        x: &DevBuffer,
        x_offset: usize,
    ) -> Result<()> {
        self.launch(
            kernel,
            LaunchArgs::new()
                .buf(out)
                .buf(&w.blocks)
                .buf_at(x, x_offset)?
                .scalar(w.rows)
                .scalar(w.cols),
            msl::k_quants::q4_k_qmv_groups(w.rows),
            msl::QMV_THREADS,
        )
    }

    fn k_matmul(
        &self,
        out: &DevBuffer,
        w: &RawQuantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
    ) -> Result<()> {
        if tokens == 1 {
            return self.k_gemv(self.pipes.k_qmv.get(f16_out), out, w, x, 0);
        }
        if tokens >= msl::QMG_BLOCK && msl::qmg_fits(w.rows, w.cols) {
            let problem = Problem {
                tokens,
                rows: w.rows,
                cols: w.cols,
                bits: 4,
                ane_rows: 0,
            };
            if self.cpu_share {
                if let Some(split) = variant::split_rows(&problem) {
                    return self.k_matmul_shared(out, w, x, tokens, f16_out, split);
                }
            }
            let (gx, gy) = msl::k_quants::q4_k_qmg_groups(w.rows, tokens);
            return self.launch_k_qmg(out, w, x, tokens, f16_out, w.rows, (gx, gy));
        }
        let (gx, gy) = msl::k_quants::q4_k_qmm_groups(w.rows, tokens);
        self.device.launch(
            self.pipes.k_qmm.get(f16_out),
            &LaunchConfig {
                grid: (gx, gy, 1),
                block: (msl::QMM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(out)
                .buf(&w.blocks)
                .buf(x)
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(tokens),
            &self.stream,
        )
    }

    fn launch_k_qmg(
        &self,
        out: &DevBuffer,
        w: &RawQuantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
        rows: u32,
        grid: (u32, u32),
    ) -> Result<()> {
        self.device.launch(
            self.pipes.k_qmg.get(f16_out),
            &LaunchConfig {
                grid: (grid.0, grid.1, 1),
                block: (msl::QMG_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(out)
                .buf(&w.blocks)
                .buf(x)
                .scalar(rows)
                .scalar(w.cols)
                .scalar(tokens),
            &self.stream,
        )
    }

    fn k_matmul_shared(
        &self,
        out: &DevBuffer,
        w: &RawQuantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
        split: RowSplit,
    ) -> Result<()> {
        let operands = BlockOperands {
            blocks: host_slice(&w.blocks)?,
            quant: w.quant,
            global: w.global,
            x: host_slice(x)?,
            out: out
                .host_ptr()
                .ok_or_else(|| ForgeError::Other("Metal: wyjście bez adresu hosta".into()))?,
            out_f16: f16_out,
            tokens,
            rows: w.rows,
            cols: w.cols,
        };
        let mut cpu = self.cpu.borrow_mut();
        cpu.check_blocks(&operands, split.gpu_rows, split.cpu_rows)?;
        cpu.unpack_blocks(&operands, split.gpu_rows as usize, split.cpu_rows as usize)?;

        // The GPU produces activations in the open command buffer. The CPU
        // can decode static weight blocks first, but it must wait before BNNS
        // reads x and writes its disjoint output rows.
        self.stream.synchronize()?;
        let (gx, gy) = msl::k_quants::q4_k_qmg_groups(split.gpu_rows, tokens);
        self.launch_k_qmg(out, w, x, tokens, f16_out, w.rows, (gx, gy))?;
        self.device.record_event(&self.split_event, &self.stream)?;

        // SAFETY: the GPU grid covers rows below split.gpu_rows and BNNS writes
        // the remaining rows, so the two writers never overlap.
        unsafe { cpu.multiply_blocks(&operands, split.gpu_rows, split.cpu_rows)? };
        drop(cpu);
        self.split_event.synchronize()
    }

    /// Projection for a whole batch.
    ///
    /// Which form serves which batch is a decision of the registry, not of this
    /// function: the thresholds and the measurements behind them live in
    /// `crate::variant`, where they can be checked for totality and for the
    /// absence of a cliff. Here there is only the mapping from a chosen form to
    /// the pipeline that implements it.
    fn matmul(
        &self,
        out: &DevBuffer,
        w_id: WeightId,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
    ) -> Result<()> {
        // Ogon ANE jest faktem o wadze (ma model CoreML albo nie) pomnożonym
        // przez politykę (`ane_share`). Bez ramienia to zero i rejestr
        // odpowiada bit w bit tak, jak przed jego istnieniem.
        let problem = Problem {
            tokens,
            rows: w.rows,
            cols: w.cols,
            bits: w.bits,
            ane_rows: self.ane_rows_for(w_id, tokens),
        };
        let chosen = MATMUL_FORMS.pick(&problem).ok_or_else(|| {
            ForgeError::Unsupported(format!("brak wariantu mnożenia dla {problem:?}"))
        })?;
        match chosen.form {
            MatmulForm::Vector => self.gemv(self.pipes.qmv.get(w.bits, f16_out), out, w, x, 0),
            MatmulForm::RegisterBlocked => self.matmul_blocked(out, w, x, tokens, f16_out),
            MatmulForm::MatrixUnits => {
                self.matmul_matrix_units(out, w_id, w, x, tokens, f16_out, None)
            }
            MatmulForm::MatrixUnitsSharedWithCpu if !self.cpu_share => {
                self.matmul_matrix_units(out, w_id, w, x, tokens, f16_out, None)
            }
            MatmulForm::MatrixUnitsSharedWithCpu => {
                let split = variant::split_rows(&problem).ok_or_else(|| {
                    ForgeError::Other(format!("podział wybrany dla {problem:?}, ale niemożliwy"))
                })?;
                self.matmul_matrix_units(out, w_id, w, x, tokens, f16_out, Some(split))
            }
            // Trzy jednostki. Rejestr wybiera tę formę tylko z niezerowym
            // ogonem ANE; udział CPU jest tu polityką jak wyżej — wyłączony
            // oddaje swoje wiersze GPU, ogon ANE zostaje.
            MatmulForm::MatrixUnitsSharedWithCpuAndAne => {
                let mut split = variant::split_rows(&problem).ok_or_else(|| {
                    ForgeError::Other(format!("podział wybrany dla {problem:?}, ale niemożliwy"))
                })?;
                if !self.cpu_share {
                    split.gpu_rows += split.cpu_rows;
                    split.cpu_rows = 0;
                }
                self.matmul_matrix_units(out, w_id, w, x, tokens, f16_out, Some(split))
            }
        }
    }

    /// The matrix-unit form, optionally sharing its rows with the CPU.
    ///
    /// The kernel always receives the FULL row count — that is the stride it
    /// writes with — and the grid is what decides which rows it touches. So
    /// giving it a shorter grid leaves the tail of every row untouched, which
    /// is exactly the window the CPU then fills.
    #[allow(clippy::too_many_arguments)]
    fn matmul_matrix_units(
        &self,
        out: &DevBuffer,
        w_id: WeightId,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
        split: Option<RowSplit>,
    ) -> Result<()> {
        let k = self.pipes.qmg.get(w.bits, f16_out);
        let Some(split) = split else {
            let (gx, gy) = msl::qmg_affine_4bit_groups(w.rows, tokens);
            return self.launch_qmg(k, out, w, x, tokens, (gx, gy));
        };
        #[cfg(not(forge_ane))]
        let _ = w_id;

        if split.cpu_rows == 0 {
            // Tylko GPU i ANE. Ten sam porządek co niżej, bez rozpakowywania:
            // czekanie na x, start ANE, GPU na czele, zatwierdzenie bufora
            // poleceń (żeby GPU liczyło, gdy ANE liczy), na koniec join.
            self.stream.synchronize()?;
            #[cfg(forge_ane)]
            if split.ane_rows > 0 {
                self.ane_begin(w_id, tokens, x, out, f16_out)?;
            }
            let (gx, gy) = msl::qmg_affine_4bit_groups(split.gpu_rows, tokens);
            self.launch_qmg(k, out, w, x, tokens, (gx, gy))?;
            self.device.record_event(&self.split_event, &self.stream)?;
            self.split_event.synchronize()?;
            #[cfg(forge_ane)]
            if split.ane_rows > 0 {
                self.ane_end(w_id)?;
            }
            return Ok(());
        }

        let operands = Operands {
            packed: host_slice(&w.packed)?,
            high: w.high.as_ref().map(host_slice).transpose()?,
            scales: host_slice(&w.scales)?,
            biases: host_slice(&w.biases)?,
            param_dtype: self.quant_params,
            x: host_slice(x)?,
            out: out
                .host_ptr()
                .ok_or_else(|| ForgeError::Other("Metal: wyjście bez adresu hosta".into()))?,
            out_f16: f16_out,
            tokens,
            rows: w.rows,
            cols: w.cols,
            group: w.group,
            bits: w.bits,
        };
        let mut cpu = self.cpu.borrow_mut();
        cpu.check(&operands, split.gpu_rows, split.cpu_rows)?;

        // Unpacking FIRST, before the wait below. It needs only the weights,
        // which are static, so it costs nothing here: it runs in the window
        // where the CPU would otherwise be idle watching the GPU finish the
        // activations. Measured at 938 us a product, this is the difference
        // between paying for it and hiding it.
        cpu.unpack(&operands, split.gpu_rows as usize, split.cpu_rows as usize);

        // The CPU half reads `x` with its own load instructions, so `x` has to
        // BE there. Everything that produces it is sitting in the open command
        // buffer, queued and not yet run: the GPU half is ordered after it and
        // is therefore safe, but the CPU is not ordered against the GPU at all.
        // Without this wait the CPU multiplies whatever the buffer happened to
        // hold — which is not a crash, just a different model.
        self.stream.synchronize()?;

        // Ogon ANE startuje TU: po zmaterializowaniu x (ta sama pułapka co
        // dla CPU — ANE czyta x własnymi instrukcjami), a przed GPU, żeby
        // wszystkie trzy jednostki liczyły naraz. Dla grupy dwuczęściowej
        // (gate+up) predict zaczyna się w `gate`, a kończy w `up`.
        #[cfg(forge_ane)]
        if split.ane_rows > 0 {
            self.ane_begin(w_id, tokens, x, out, f16_out)?;
        }

        let (gx, gy) = msl::qmg_affine_4bit_groups(split.gpu_rows, tokens);
        self.launch_qmg(k, out, w, x, tokens, (gx, gy))?;

        // Submit, or the dispatch would sit in the open command buffer and the
        // CPU would spend its share racing an idle GPU. This is the one place
        // that deliberately pays for a command buffer of its own — 19,6 us
        // against 0,61 (EKS-A3) — and `split_rows` only allows it where that is
        // a couple of percent of the work being overlapped.
        self.device.record_event(&self.split_event, &self.stream)?;

        // SAFETY: the GPU dispatch above writes rows below `gpu_rows` and this
        // writes from `gpu_rows` up, into the same shared allocation. The two
        // ranges are disjoint, so no ordering between them is needed — only the
        // wait below, before anything reads the whole result.
        unsafe { cpu.multiply(&operands, split.gpu_rows, split.cpu_rows)? };
        drop(cpu);
        self.split_event.synchronize()?;
        #[cfg(forge_ane)]
        if split.ane_rows > 0 {
            self.ane_end(w_id)?;
        }
        Ok(())
    }

    /// The matrix-unit dispatch itself. The kernel always receives the FULL row
    /// count — that is the stride it writes with — and the grid is what decides
    /// which rows it touches.
    fn launch_qmg(
        &self,
        k: &KernelHandle,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        grid: (u32, u32),
    ) -> Result<()> {
        self.device.launch(
            k,
            &LaunchConfig {
                grid: (grid.0, grid.1, 1),
                block: (msl::QMG_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &weight_args(LaunchArgs::new().buf(out), w, x, 0)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group)
                .scalar(tokens),
            &self.stream,
        )
    }

    /// Register-blocked loop, for batches too small to fill a matrix block.
    fn matmul_blocked(
        &self,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
    ) -> Result<()> {
        let k = self.pipes.qmm.get(w.bits, f16_out);
        let (gx, gy) = msl::qmm_affine_4bit_groups(w.rows, tokens);
        self.device.launch(
            k,
            &LaunchConfig {
                grid: (gx, gy, 1),
                block: (msl::QMM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &weight_args(LaunchArgs::new().buf(out), w, x, 0)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group)
                .scalar(tokens),
            &self.stream,
        )
    }

    fn kv_append(&self, cache: &DevBuffer, src: &DevBuffer, pos: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        self.launch(
            &self.pipes.kv_append,
            LaunchArgs::new()
                .buf(cache)
                .buf(src)
                .scalar(s.kv_heads)
                .scalar(s.head_dim)
                .scalar(self.seq_cap)
                .scalar(pos)
                .scalar(tokens),
            msl::elementwise_groups(tokens * s.kv_width()),
            msl::ELEMENTWISE_THREADS,
        )
    }

    fn launch(
        &self,
        kernel: &KernelHandle,
        args: LaunchArgs,
        groups: u32,
        threads: u32,
    ) -> Result<()> {
        self.device.launch(
            kernel,
            &LaunchConfig {
                grid: (groups, 1, 1),
                block: (threads, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &self.stream,
        )
    }
}

/// Host view of a device buffer.
///
/// On Apple every allocation is shared, so this is literally the memory the GPU
/// reads — no copy and no transfer. It is only ever taken for buffers the GPU
/// is READING during a split (weights and activations); the one buffer both
/// units write is handed over as a raw pointer with its disjointness spelled
/// out at the call site.
fn host_slice<T>(buf: &DevBuffer) -> Result<&[T]> {
    let ptr = buf
        .host_ptr()
        .ok_or_else(|| ForgeError::Other("Metal: bufor bez adresu hosta".into()))?;
    if ptr as usize % std::mem::align_of::<T>() != 0 {
        return Err(ForgeError::Other(format!(
            "Metal: adres {ptr:p} nie jest wyrównany do {} B",
            std::mem::align_of::<T>()
        )));
    }
    // SAFETY: the allocation is `buf.len()` bytes of shared memory, alive for
    // as long as the buffer, and nothing writes it while the borrow lasts.
    Ok(
        unsafe {
            std::slice::from_raw_parts(ptr as *const T, buf.len() / std::mem::size_of::<T>())
        },
    )
}

/// Typ parametrów kwantyzacji w wersji, którą znają kernele.
fn quant_param_dtype(d: DType) -> Result<ScaleDtype> {
    match d {
        DType::F16 => Ok(ScaleDtype::F16),
        DType::BF16 => Ok(ScaleDtype::Bf16),
        other => Err(ForgeError::Unsupported(format!(
            "skale w {other:?}, a kernel zna f16 i bf16"
        ))),
    }
}

/// Typ wagi normalizacji. Osobno od skal, bo to osobna właściwość źródła: GGUF
/// trzyma normy w f32, a skale w f16.
fn norm_weight_dtype(d: DType) -> Result<ScaleDtype> {
    match d {
        DType::F16 => Ok(ScaleDtype::F16),
        DType::BF16 => Ok(ScaleDtype::Bf16),
        DType::F32 => Ok(ScaleDtype::F32),
        other => Err(ForgeError::Unsupported(format!(
            "waga normy ma typ {other:?}, a kernel zna f16, bf16 i f32"
        ))),
    }
}

/// Kompiluje cztery warianty jednej rodziny: dwie szerokości kodu razy dwa
/// typy wyjścia. Wypisywanie ich ręcznie znaczyłoby dwanaście wywołań, w
/// których łatwo pomylić jeden parametr i dostać kernel liczący co innego.
fn quant_pipes(
    compile: &mut impl FnMut(&str, &str) -> Result<KernelHandle>,
    source: fn(msl::Bits, ScaleDtype, OutDtype) -> String,
    name: fn(msl::Bits, ScaleDtype, OutDtype) -> String,
    scales: ScaleDtype,
) -> Result<QuantPipes> {
    let mut one = |bits, out| compile(&source(bits, scales, out), &name(bits, scales, out));
    Ok(QuantPipes {
        by: [
            [
                one(msl::Bits::Four, OutDtype::F32)?,
                one(msl::Bits::Four, OutDtype::F16)?,
            ],
            [
                one(msl::Bits::Six, OutDtype::F32)?,
                one(msl::Bits::Six, OutDtype::F16)?,
            ],
        ],
    })
}

fn k_quant_pipes(
    compile: &mut impl FnMut(&str, &str) -> Result<KernelHandle>,
    source: fn(OutDtype) -> String,
    name: fn(OutDtype) -> String,
) -> Result<KQuantPipes> {
    Ok(KQuantPipes {
        by: [
            compile(&source(OutDtype::F32), &name(OutDtype::F32))?,
            compile(&source(OutDtype::F16), &name(OutDtype::F16))?,
        ],
    })
}

/// Bufory wagi w kolejności, której oczekuje kernel.
///
/// Sześciobitowy deklaruje jeden bufor więcej, więc kolejność skalarów zależy
/// od szerokości kodu. Zbierane w jednym miejscu, bo rozjazd między tym a
/// deklaracją kernela nie jest błędem kompilacji — jest złym wynikiem.
fn weight_args(
    args: LaunchArgs,
    w: &Quantized,
    x: &DevBuffer,
    x_offset: usize,
) -> Result<LaunchArgs> {
    let args = args
        .buf(&w.packed)
        .buf(&w.scales)
        .buf(&w.biases)
        .buf_at(x, x_offset)?;
    match (&w.high, w.bits) {
        (Some(h), 6) => Ok(args.buf(h)),
        (None, 4) => Ok(args),
        _ => Err(ForgeError::Other(format!(
            "waga deklaruje {} bitów, a bufor wyższych bitów {}",
            w.bits,
            if w.high.is_some() {
                "jest"
            } else {
                "go nie ma"
            }
        ))),
    }
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}
