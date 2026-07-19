// =============================================================================
// Plik: llama_engine.rs
// Opis: Silnik continuous batching nad llama.cpp — jeden model, jeden kontekst
//       z wieloma slotami sekwencji, równoległe zapytania w jednym llama_decode,
//       streaming per-request bez blokowania scheduler-a (anty-hang).
// Przykład: let engine = LlamaEngine::load(&path, EngineConfig::default())?;
//           let stream = engine.submit(GenRequest { prompt, .. })?;
// =============================================================================

use std::path::Path;
use std::sync::mpsc::Receiver;

use crate::llama::{FlashAttentionMode, LlamaError};

#[cfg(feature = "llama")]
use std::sync::mpsc::{Sender, SyncSender, TrySendError};
#[cfg(feature = "llama")]
use std::sync::Arc;

#[cfg(feature = "llama")]
use crate::llama::{
    build_sampler_chain, check_stop_sequence, is_eog_with_model, token_to_piece_with_model,
    tokenize_with_model, LlamaContextGuard, LlamaRuntime, LlamaSamplerGuard,
};

#[cfg(feature = "llama")]
use crate::llama::sys;

// Tryb speculative decoding silnika.
//
// `NgramSimple` używa wbudowanego draftera ngramowego z biblioteki (bez modelu
// draftującego): dla każdej sekwencji proponuje do `n_max` tokenów na podstawie
// powtórzeń w dotychczasowym kontekście, a my weryfikujemy je jednym wspólnym
// llama_decode i rollbackujemy odrzucone.
//
// `Mtp` (Multi-Token Prediction, self-speculative) używa głowy MTP wbudowanej w
// TEN SAM model docelowy (zero duplikacji wag) jako draftera. Wymaga drugiego
// kontekstu draftującego (ctx_dft) utworzonego na tym samym modelu z
// ctx_type=LLAMA_CONTEXT_TYPE_MTP. Po każdym decode kontekstu docelowego
// biblioteka mirroruje embeddingi nextn target→draft (common_speculative_process),
// potem draftuje do `n_max` tokenów; weryfikacja/rollback ctx_tgt jak w ngram, a
// wewnętrzny stan ctx_dft sprząta biblioteka w common_speculative_accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpeculativeMode {
    #[default]
    Off,
    NgramSimple {
        n_max: u32,
        n_min: u32,
    },
    Mtp {
        n_max: u32,
    },
}

impl SpeculativeMode {
    // Liczba snapshotów stanu rekurencyjnego wymagana, by rollback (seq_rm) działał
    // dla modeli rekurencyjnych. Musi być >= maksymalnej długości draftu. Dotyczy
    // kontekstu docelowego (ctx_tgt) — dla MTP draft też produkuje do n_max tokenów.
    fn required_n_rs_seq(self) -> u32 {
        match self {
            SpeculativeMode::Off => 0,
            SpeculativeMode::NgramSimple { n_max, .. } => n_max.max(1),
            SpeculativeMode::Mtp { n_max } => n_max.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub n_seq_max: u32,
    pub ctx_per_seq: u32,
    pub n_batch: u32,
    pub n_ubatch: u32,
    pub n_gpu_layers: u32,
    // Indeks karty głównej i wagi rozkładu warstw — przekazywane do
    // LlamaLoadConfig przy ładowaniu modelu. Wybór kart embedded idzie wyłącznie
    // tędy (jeden proces core, CUDA_VISIBLE_DEVICES nie działa po starcie).
    pub main_gpu: i32,
    pub tensor_split: Vec<f32>,
    pub threads: Option<u32>,
    pub flash_attn: FlashAttentionMode,
    pub kv_unified: bool,
    pub n_rs_seq: u32,
    pub speculative: SpeculativeMode,
    pub queue_capacity: usize,
    pub stream_capacity: usize,
    // Maksymalny czas BEZ postępu dostarczania tokenów do konsumenta dla slotu,
    // który ma niepuste `pending`. Konsument „żywy ale niemy" (kanał wiecznie
    // pełny, nigdy nie czyta i nie rozłącza się) trzymałby inaczej slot + KV +
    // inflight w nieskończoność i po wyczerpaniu queue_capacity silnik odmawiałby
    // wszystkich przyjęć (cichy hang admission). Po przekroczeniu progu slot jest
    // zwalniany z FinishReason::Error. Liczy się TYLKO realny zastój dostarczania
    // (pending nie maleje), nie normalne czekanie slotu na swoją turę decode.
    pub stream_stall_timeout: std::time::Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            n_seq_max: 4,
            ctx_per_seq: 2048,
            n_batch: 2048,
            n_ubatch: 512,
            n_gpu_layers: crate::llama::DEFAULT_GPU_LAYERS,
            main_gpu: 0,
            tensor_split: Vec::new(),
            threads: None,
            flash_attn: FlashAttentionMode::Auto,
            kv_unified: false,
            n_rs_seq: 0,
            speculative: SpeculativeMode::Off,
            queue_capacity: 256,
            stream_capacity: 256,
            stream_stall_timeout: std::time::Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SamplingParams {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub repeat_penalty: f32,
    pub seed: u32,
}

impl Default for SamplingParams {
    fn default() -> Self {
        Self {
            temperature: 0.7,
            top_p: 0.9,
            top_k: 40,
            repeat_penalty: 1.0,
            seed: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GenRequest {
    pub prompt: String,
    pub system_prompt: Option<String>,
    pub sampling: SamplingParams,
    pub max_tokens: u32,
    pub stop_sequences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    EndOfText,
    MaxTokens,
    StopSequence(String),
    ContextFull,
    PromptTooLong,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct StreamToken {
    pub text: String,
    pub is_final: bool,
    pub finish_reason: Option<FinishReason>,
    // Liczba REALNIE wygenerowanych tokenów modelu (nie fragmentów tekstu strumienia).
    // Ustawiana wyłącznie na tokenie finalnym (is_final=true); 0 dla fragmentów. Przy
    // speculative jedna iteracja może wyprodukować wiele tokenów scalonych w jeden
    // fragment tekstu, więc to jedyna wiarygodna miara tok/s.
    pub generated_tokens: u32,
    // Liczba tokenów promptu sekwencji (slot zna prompt.len()). Ustawiana wyłącznie
    // na tokenie finalnym; 0 dla fragmentów. Pozwala konsumentowi (core generate)
    // raportować realne prompt_tokens zamiast twardego 0.
    pub prompt_tokens: u32,
    // Przepustowość fazy prefill (tokeny promptu / czas prefillu) oraz fazy dekodowania
    // (tokeny wygenerowane / czas generacji), mierzone w SILNIKU po realnych granicach
    // faz slotu. Ustawiane wyłącznie na tokenie finalnym; 0.0 = brak pomiaru.
    pub prefill_tps: f32,
    pub completion_tps: f32,
    // Czas do pierwszego tokena (ms) mierzony w SILNIKU jako granica faz slotu:
    // od startu requestu (przypisanie slotu) do przejścia w fazę dekodowania.
    // To realny TTFT silnika, niezależny od buforowania kanału między silnikiem a
    // konsumentem. Ustawiany wyłącznie na tokenie finalnym; 0 = brak pomiaru.
    pub ttft_ms: u32,
}

// Strumień wyjściowy jednego requestu. Konsument odbiera tokeny w tempie własnym;
// scheduler nigdy nie blokuje się na wolnym konsumencie (try_send + recovery).
pub struct RequestStream {
    rx: Receiver<StreamToken>,
}

impl RequestStream {
    pub fn recv(&self) -> Option<StreamToken> {
        self.rx.recv().ok()
    }

    pub fn try_recv(&self) -> Result<StreamToken, std::sync::mpsc::TryRecvError> {
        self.rx.try_recv()
    }

    pub fn into_receiver(self) -> Receiver<StreamToken> {
        self.rx
    }
}

// Wynik próby dostarczenia tokena do konsumenta przez `EngineSink`.
//
// `Delivered`  — token oddany konsumentowi.
// `Full(token) — kanał chwilowo pełny; scheduler odkłada `token` do bufora
//                pending tego SLOTU i ponawia później (backpressure per-slot,
//                bez blokowania pozostałych slotów).
// `Closed`     — konsument zniknął; slot zostaje zwolniony.
#[derive(Debug)]
pub enum SinkStatus {
    Delivered,
    Full(StreamToken),
    Closed,
}

// Ujście tokenów jednego requestu. Scheduler woła je BEZPOŚREDNIO ze swojego
// wątku (zero wątku-per-request), więc kontrakt jest twardo nieblokujący:
// `try_send` musi wrócić natychmiast. Implementacja po stronie core owija
// `tokio::sync::mpsc::Sender` (try_send → SinkStatus), dzięki czemu wrappers
// pozostaje BEZ zależności od tokio, a setki równoległych requestów dzielą jeden
// wątek-scheduler bez globalnego locka.
pub trait EngineSink: Send {
    fn try_send(&mut self, token: StreamToken) -> SinkStatus;
}

// Wbudowane ujście oparte o `std::sync::mpsc::SyncSender` — używane przez
// wygodne `LlamaEngine::submit`, które zwraca `RequestStream`. Konsumenci
// bez tokio (np. example smoke, testy) dostają synchroniczny kanał.
#[cfg(feature = "llama")]
struct ChannelSink {
    tx: SyncSender<StreamToken>,
}

#[cfg(feature = "llama")]
impl EngineSink for ChannelSink {
    fn try_send(&mut self, token: StreamToken) -> SinkStatus {
        match self.tx.try_send(token) {
            Ok(()) => SinkStatus::Delivered,
            Err(TrySendError::Full(t)) => SinkStatus::Full(t),
            Err(TrySendError::Disconnected(_)) => SinkStatus::Closed,
        }
    }
}

#[cfg(feature = "llama")]
pub struct LlamaEngine {
    submit_tx: Sender<EngineCommand>,
    join: Option<std::thread::JoinHandle<()>>,
    queue_capacity: usize,
    stream_capacity: usize,
    inflight: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(feature = "llama")]
enum EngineCommand {
    Submit(SlotJob),
    Shutdown,
}

#[cfg(feature = "llama")]
struct SlotJob {
    request: GenRequest,
    sink: Box<dyn EngineSink>,
}

#[cfg(feature = "llama")]
impl LlamaEngine {
    pub fn load(model_path: &Path, mut config: EngineConfig) -> Result<Self, LlamaError> {
        if config.n_seq_max == 0 {
            return Err(LlamaError::ContextFailed);
        }
        match config.speculative {
            SpeculativeMode::NgramSimple { n_max, n_min } => {
                if n_max == 0 || n_min > n_max {
                    return Err(LlamaError::LoadFailed(
                        "ngram speculative: wymagane 1 <= n_min <= n_max".to_string(),
                    ));
                }
            }
            SpeculativeMode::Mtp { n_max } => {
                if n_max == 0 {
                    return Err(LlamaError::LoadFailed(
                        "MTP speculative: wymagane n_max >= 1".to_string(),
                    ));
                }
            }
            SpeculativeMode::Off => {}
        }
        // Rollback rekurencyjny (seq_rm po odrzuceniu draftów) działa tylko gdy
        // kontekst utworzono z n_rs_seq >= długości draftu. Wymuszamy to z config.
        config.n_rs_seq = config.n_rs_seq.max(config.speculative.required_n_rs_seq());

        // Model ładujemy w wątku schedulera, by raw *mut llama_context nigdy nie
        // opuścił wątku-właściciela. Tu przekazujemy tylko ścieżkę + config.
        let model_path = model_path.to_path_buf();
        let queue_capacity = config.queue_capacity.max(1);
        let stream_capacity = config.stream_capacity.max(1);

        let (submit_tx, submit_rx) = std::sync::mpsc::channel::<EngineCommand>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), LlamaError>>();
        let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let thread_config = config.clone();
        let thread_inflight = Arc::clone(&inflight);
        let join = std::thread::Builder::new()
            .name("llama-engine".to_string())
            .spawn(move || {
                scheduler_main(model_path, thread_config, submit_rx, ready_tx, thread_inflight);
            })
            .map_err(|e| LlamaError::LoadFailed(format!("nie udało się uruchomić wątku: {e}")))?;

        // Czekamy aż wątek załaduje model i utworzy kontekst — błąd ładowania
        // musi wrócić do wywołującego, a nie zniknąć w wątku.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                submit_tx,
                join: Some(join),
                queue_capacity,
                stream_capacity,
                inflight,
            }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                let _ = join.join();
                Err(LlamaError::ContextFailed)
            }
        }
    }

    // Wygodny wariant: tworzy wewnętrzny synchroniczny kanał i zwraca
    // `RequestStream`. Konsumenci bez tokio (smoke/test) odbierają przez
    // `RequestStream::recv`. Hot-path core używa `submit_with_sink`, by oddać
    // tokeny wprost do `tokio::mpsc` (zero dodatkowego wątku/kanału po drodze).
    pub fn submit(&self, request: GenRequest) -> Result<RequestStream, LlamaError> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<StreamToken>(self.stream_capacity);
        self.submit_with_sink(request, Box::new(ChannelSink { tx }))?;
        Ok(RequestStream { rx })
    }

    // Główne wejście: oddaje tokeny generacji do podanego `sink`. Scheduler woła
    // `sink.try_send` BEZPOŚREDNIO ze swojego wątku — żaden dodatkowy wątek nie
    // powstaje na request, więc ścieżka skaluje do setek równoległych zapytań
    // dzielących jeden wątek-scheduler.
    pub fn submit_with_sink(
        &self,
        request: GenRequest,
        sink: Box<dyn EngineSink>,
    ) -> Result<(), LlamaError> {
        use std::sync::atomic::Ordering;

        // Backpressure bez blokowania: gdy w locie jest już queue_capacity
        // requestów, odrzucamy zamiast czekać. Scheduler dekrementuje licznik
        // przy każdym zakończeniu requestu (terminal StreamToken).
        let prev = self.inflight.fetch_add(1, Ordering::AcqRel);
        if prev >= self.queue_capacity {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return Err(LlamaError::LoadFailed("kolejka silnika pełna".to_string()));
        }

        let job = SlotJob { request, sink };
        if self.submit_tx.send(EngineCommand::Submit(job)).is_err() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return Err(LlamaError::ContextFailed);
        }
        Ok(())
    }

    pub fn capacity(&self) -> usize {
        self.queue_capacity
    }

    // Liczba requestów aktualnie w locie (zakolejkowanych lub generujących).
    // Dekrementowana przy zwolnieniu slotu (zakończenie, rozłączenie, stall, błąd)
    // oraz przy odrzuceniu (reject_job). Po zakończeniu/zwolnieniu wszystkich
    // requestów wraca do 0 — używane w testach do wykrycia wycieku slotu/KV.
    pub fn inflight(&self) -> usize {
        self.inflight.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(feature = "llama")]
impl Drop for LlamaEngine {
    fn drop(&mut self) {
        let _ = self.submit_tx.send(EngineCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(feature = "llama")]
#[derive(PartialEq, Eq, Clone, Copy)]
enum SlotState {
    Idle,
    Prefill,
    Generating,
}

#[cfg(feature = "llama")]
struct Slot {
    state: SlotState,
    seq_id: i32,
    prompt: Vec<sys::llama_token>,
    prompt_consumed: usize,
    pos: i32,
    generated: u32,
    max_tokens: u32,
    stop_sequences: Vec<String>,
    generated_text: String,
    sent_len: usize,
    decoder: encoding_rs::Decoder,
    sampler: Option<LlamaSamplerGuard>,
    sink: Option<Box<dyn EngineSink>>,
    // Powód zakończenia ustawiony, gdy slot już wyprodukował token finalny i
    // czeka tylko na opróżnienie `pending` (deferred finish). Dopóki Some i
    // pending niepuste, scheduler dalej próbuje dostarczać zaległości BEZ
    // blokowania innych slotów; po opróżnieniu robi realne sprzątanie slotu.
    finishing: Option<FinishReason>,
    // Bufor zaległych tokenów, gdy kanał konsumenta był pełny. Przy speculative
    // jedna iteracja może zaakceptować wiele tokenów, więc pending musi unieść
    // więcej niż jeden token (FIFO zachowuje kolejność wobec konsumenta).
    pending: std::collections::VecDeque<StreamToken>,
    // Token do zdekodowania w następnej iteracji dla slotu Generating.
    next_decode_token: Option<sys::llama_token>,
    // Token id_last zaplanowany do decode w bieżącej iteracji (na batch_logits_index).
    // Po decode dopisujemy go do history jako trwale osadzony w KV.
    id_last_in_flight: Option<sys::llama_token>,
    // Indeks w bieżącym batchu, pod którym leżą logity tego slotu (-1 = brak).
    batch_logits_index: i32,
    // Historia tokenów sekwencji (prompt + zaakceptowane wygenerowane). Służy jako
    // bufor promptu dla draftera ngram oraz baza spójności pos/KV przy rollbacku.
    // Niewykorzystywana gdy speculative=Off (pozostaje pusta).
    history: Vec<sys::llama_token>,
    // Indeksy w bieżącym batchu, pod którymi leżą logity tokenów draftu tego slotu
    // (kolejność = kolejność draftu). Wypełniane tylko przy aktywnym speculative.
    draft_logit_indices: Vec<i32>,
    // Draftowe tokeny zaplanowane w bieżącym batchu (równolegle do indeksów).
    draft_tokens: Vec<sys::llama_token>,
    // Czy w bieżącej turze wywołano spec.draft() z efektywnym budżetem > 0 dla tego
    // slotu. Stan wewnętrzny biblioteki (impl_last/pending_h) MUSI być domknięty
    // dokładnie jednym accept() po KAŻDYM drafcie — ta flaga gwarantuje, że accept
    // pada raz na turę, niezależnie od ścieżki wyjścia commit_generation. Resetowana
    // na początku każdej tury slotu.
    drafted_this_turn: bool,
    // Czy biblioteka ma już ustawiony impl_last[seq_id] dla tej sekwencji. Stan ten
    // jest ustawiany wewnątrz biblioteki dopiero gdy draft() zwróci NIEPUSTY wynik i
    // NIGDY nie jest zerowany aż do końca życia instancji. common_speculative_accept
    // robi GGML_ASSERT(impl_last[seq_id]) → abort, więc accept wolno wołać dopiero
    // gdy pierwszy draft tej sekwencji faktycznie coś wyprodukował. Gdy draft zwróci
    // 0 tokenów zanim impl_last zostanie ustawiony, pending_h i tak jest poprawne
    // (process() zapisuje je z wiersza id_last), więc pominięcie accept jest bezpieczne.
    spec_impl_ready: bool,
    // Znacznik ostatniego POSTĘPU dostarczania tokenów do konsumenta. Ustawiany na
    // starcie slotu i odświeżany WYŁĄCZNIE przy realnym dostarczeniu tokena
    // (SinkStatus::Delivered → zmniejszenie `pending`). NIE jest odświeżany gdy
    // `pending` jest puste — pusty bufor po prostu nie spełnia warunku zastoju w
    // enforce_stall_timeout, więc zdrowy slot bez zaległości i tak nie jest ubijany.
    // Gdy `pending` jest niepuste i `last_progress` jest starsze niż
    // stream_stall_timeout, slot jest siłą zwalniany — patrz CR-001 (anty-hang
    // „żywego ale niemego" konsumenta, który nigdy nie czyta i nie rozłącza się).
    last_progress: std::time::Instant,
    // Granice czasowe faz slotu do pomiaru przepustowości. `prefill_start` ustawiany
    // przy starcie requestu (start fazy prefill), `gen_start` przy przejściu
    // Prefill→Generating (koniec prefillu = start dekodowania). Różnica = czas prefillu;
    // od `gen_start` do finału = czas dekodowania.
    prefill_start: std::time::Instant,
    gen_start: Option<std::time::Instant>,
}

#[cfg(feature = "llama")]
impl Slot {
    fn new(seq_id: i32) -> Self {
        Self {
            state: SlotState::Idle,
            seq_id,
            prompt: Vec::new(),
            prompt_consumed: 0,
            pos: 0,
            generated: 0,
            max_tokens: 0,
            stop_sequences: Vec::new(),
            generated_text: String::new(),
            sent_len: 0,
            decoder: encoding_rs::UTF_8.new_decoder(),
            sampler: None,
            sink: None,
            finishing: None,
            pending: std::collections::VecDeque::new(),
            next_decode_token: None,
            id_last_in_flight: None,
            batch_logits_index: -1,
            history: Vec::new(),
            draft_logit_indices: Vec::new(),
            draft_tokens: Vec::new(),
            drafted_this_turn: false,
            spec_impl_ready: false,
            last_progress: std::time::Instant::now(),
            prefill_start: std::time::Instant::now(),
            gen_start: None,
        }
    }

    fn reset(&mut self) {
        self.state = SlotState::Idle;
        self.prompt.clear();
        self.prompt_consumed = 0;
        self.pos = 0;
        self.generated = 0;
        self.max_tokens = 0;
        self.stop_sequences.clear();
        self.generated_text.clear();
        self.sent_len = 0;
        self.decoder = encoding_rs::UTF_8.new_decoder();
        self.sampler = None;
        self.sink = None;
        self.finishing = None;
        self.pending.clear();
        self.next_decode_token = None;
        self.id_last_in_flight = None;
        self.batch_logits_index = -1;
        self.history.clear();
        self.draft_logit_indices.clear();
        self.draft_tokens.clear();
        self.drafted_this_turn = false;
        self.spec_impl_ready = false;
        self.last_progress = std::time::Instant::now();
        self.prefill_start = std::time::Instant::now();
        self.gen_start = None;
    }
}

// Stan batcha budowanego ręcznie (multi-seq aware, każdy wpis ma własny seq_id).
#[cfg(feature = "llama")]
struct EngineBatch {
    raw: sys::llama_batch,
    capacity: i32,
}

#[cfg(feature = "llama")]
impl EngineBatch {
    fn new(capacity: i32) -> Self {
        Self {
            raw: unsafe { sys::llama_batch_init(capacity, 0, 1) },
            capacity,
        }
    }

    fn clear(&mut self) {
        self.raw.n_tokens = 0;
    }

    fn len(&self) -> i32 {
        self.raw.n_tokens
    }

    fn is_full(&self) -> bool {
        self.raw.n_tokens >= self.capacity
    }

    // Zwraca indeks dodanego tokena w batchu (potrzebny do llama_get_logits_ith).
    fn add(&mut self, token: sys::llama_token, pos: i32, seq_id: i32, logits: bool) -> i32 {
        let idx = self.raw.n_tokens as isize;
        unsafe {
            *self.raw.token.offset(idx) = token;
            *self.raw.pos.offset(idx) = pos;
            *self.raw.n_seq_id.offset(idx) = 1;
            **self.raw.seq_id.offset(idx) = seq_id;
            *self.raw.logits.offset(idx) = if logits { 1 } else { 0 };
        }
        self.raw.n_tokens += 1;
        idx as i32
    }
}

#[cfg(feature = "llama")]
impl Drop for EngineBatch {
    fn drop(&mut self) {
        unsafe { sys::llama_batch_free(self.raw) };
    }
}

// Uchwyt do draftera (shim common_speculative). Instancja NIE jest thread-safe
// (kontrakt z Fazy 1) — żyje wyłącznie w wątku scheduler_main, gdzie jest tworzona
// i zwalniana. Free następuje w Drop (shutdown wątku).
//
// Dla MTP shim trzyma DODATKOWO drugi kontekst draftujący (ctx_dft) utworzony na
// tym samym modelu z ctx_type=MTP. Surowy wskaźnik ctx_dft jest przechowywany w
// bibliotece przez całe życie instancji, więc kolejność zwalniania jest twarda:
// najpierw shim (free), potem ctx_dft. Drop respektuje tę kolejność.
#[cfg(feature = "llama")]
struct SpeculativeEngine {
    raw: *mut sys::llama_rs_speculative,
    // Kontekst draftujący MTP (drugi kontekst na tym samym modelu). None dla ngram.
    // Zwalniany RĘCZNIE w Drop PO llama_rs_speculative_free — patrz niżej. Trzymamy
    // surowy wskaźnik zamiast LlamaContextGuard, by zagwarantować tę kolejność.
    ctx_dft: *mut sys::llama_context,
    // Czy implementacja wymaga przetwarzania batcha po decode (MTP: mirror nextn
    // embeddingów target→draft). Pamiętane raz, bo nie zmienia się w trakcie życia.
    needs_process: bool,
}

#[cfg(feature = "llama")]
impl SpeculativeEngine {
    // Tworzy drafter ngram (bez modelu draftującego, ctx_dft=null).
    fn new_ngram(n_max: u32, n_min: u32, n_seq: u32, n_rs_seq: u32) -> Result<Self, LlamaError> {
        let params = sys::llama_rs_speculative_params {
            type_: sys::LLAMA_RS_SPECULATIVE_TYPE_NGRAM_SIMPLE,
            n_max: n_max as i32,
            n_min: n_min as i32,
        };
        let raw = unsafe {
            sys::llama_rs_speculative_init(
                &params,
                n_seq,
                n_rs_seq,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if raw.is_null() {
            return Err(LlamaError::LoadFailed(
                "nie udało się zainicjalizować draftera ngram".to_string(),
            ));
        }
        Ok(Self {
            raw,
            ctx_dft: std::ptr::null_mut(),
            needs_process: false,
        })
    }

    // Tworzy drafter MTP. `ctx_tgt` to kontekst docelowy, `model` to TEN SAM model
    // (zero duplikacji wag). Tworzy własny kontekst draftujący ctx_dft z
    // ctx_type=LLAMA_CONTEXT_TYPE_MTP i przekazuje oba do shimu. n_rs_seq dotyczy
    // ctx_tgt (rollback target) — ctx_dft tworzymy zawsze z n_rs_seq=0 (wzorzec
    // server-context.cpp:949,965: draft MTP nie potrzebuje snapshotów rollbacku).
    fn new_mtp(
        n_max: u32,
        n_rs_seq: u32,
        model: *mut sys::llama_model,
        ctx_tgt: *mut sys::llama_context,
        config: &EngineConfig,
        dims: ContextDims,
    ) -> Result<Self, LlamaError> {
        let n_seq = dims.n_seq;
        let ctx_dft = create_mtp_draft_context(model, config, dims)?;

        let params = sys::llama_rs_speculative_params {
            type_: sys::LLAMA_RS_SPECULATIVE_TYPE_DRAFT_MTP,
            n_max: n_max as i32,
            n_min: -1,
        };
        let raw = unsafe {
            sys::llama_rs_speculative_init(&params, n_seq, n_rs_seq, ctx_tgt, ctx_dft)
        };
        if raw.is_null() {
            // Posprzątaj już utworzony ctx_dft, zanim zgłosimy błąd.
            unsafe { sys::llama_free(ctx_dft) };
            return Err(LlamaError::LoadFailed(
                "nie udało się zainicjalizować draftera MTP (sprawdź czy model ma głowę MTP / nextn)"
                    .to_string(),
            ));
        }
        // Tani guard: MTP wymaga, by biblioteka zgłosiła zapotrzebowanie na embeddingi
        // nextn (model ma faktycznie głowę MTP / nextn). Jeśli false, init "udał się"
        // strukturalnie, ale model nie potrafi draftować przez MTP — zwalniamy oba
        // konteksty i zgłaszamy błąd ładowania wcześnie (kontrakt no-fallback).
        let has_nextn = unsafe { sys::llama_rs_speculative_need_embd_nextn(raw) };
        if !has_nextn {
            unsafe { sys::llama_rs_speculative_free(raw) };
            unsafe { sys::llama_free(ctx_dft) };
            return Err(LlamaError::LoadFailed(
                "model bez głowy MTP/nextn (need_embd_nextn=false)".to_string(),
            ));
        }
        Ok(Self {
            raw,
            ctx_dft,
            needs_process: true,
        })
    }

    // Mirroruje stan po decode kontekstu docelowego do kontekstu draftującego
    // (MTP: embeddingi nextn target→draft). Zwraca błąd, gdy biblioteczny
    // llama_decode(ctx_dft) zawiedzie. No-op dla ngram (needs_process=false).
    fn process(&mut self, batch: &sys::llama_batch) -> Result<(), LlamaError> {
        if !self.needs_process {
            return Ok(());
        }
        let ok = unsafe { sys::llama_rs_speculative_process(self.raw, batch as *const _) };
        if !ok {
            return Err(LlamaError::LoadFailed(
                "common_speculative_process (MTP) zwrócił błąd".to_string(),
            ));
        }
        Ok(())
    }

    // Czyści wewnętrzny stan KV/rekurencyjny kontekstu draftującego dla sekwencji
    // (przy zwalnianiu slotu). No-op dla ngram. Wzorzec server-context.cpp:166.
    fn clear_seq(&mut self, seq_id: i32) {
        if self.ctx_dft.is_null() {
            return;
        }
        let mem_dft = unsafe { sys::llama_get_memory(self.ctx_dft) };
        unsafe { sys::llama_memory_seq_rm(mem_dft, seq_id, -1, -1) };
    }

    // Rolluje KV kontekstu draftującego sekwencji do pierwszej wolnej pozycji
    // `new_pos` (usuwa [new_pos, koniec)). process() zaawansował ctx_dft o cały
    // batch (id_last + wszystkie drafty); po akceptacji tylko prefiksu reszta musi
    // zniknąć, inaczej następny decode trafi na niespójne pozycje (M-RoPE: X < Y).
    // Lustrza rollback ctx_tgt — wzorzec server-context.cpp:3493-3496. No-op dla
    // ngram. Wewnętrzny stan rekurencyjny ctx_dft sprząta osobno accept().
    fn rollback_dft_seq(&mut self, seq_id: i32, new_pos: i32) {
        if self.ctx_dft.is_null() {
            return;
        }
        let mem_dft = unsafe { sys::llama_get_memory(self.ctx_dft) };
        unsafe { sys::llama_memory_seq_rm(mem_dft, seq_id, new_pos, -1) };
    }

    // Cofa KV ctx_dft do `base_pos` (pozycja id_last) tuż po draft(), kasując
    // autoregresyjne wstępne zaawansowanie draftera, zanim process() zdekoduje
    // batch weryfikacyjny. No-op dla ngram. Wzorzec server-context.cpp:2552-2558.
    fn rollback_dft_after_draft(&mut self, seq_id: i32, base_pos: i32) {
        self.rollback_dft_seq(seq_id, base_pos);
    }

    // Generuje draft dla jednej sekwencji i kopiuje go do `out`. `n_max_eff` to
    // efektywny limit długości draftu w TEJ turze (budżet batcha dla slotu) — musi
    // być >= 1; biblioteka obcina wynik do tej wartości (dp.n_max), więc liczba
    // wydraftowanych == liczba wstawionych do batcha == liczba weryfikowanych
    // (likwiduje rozjazd między długością draftu w bibliotece a verify). Zwraca
    // liczbę tokenów draftu (0 = brak propozycji w tej iteracji).
    fn draft(
        &mut self,
        seq_id: i32,
        n_max_eff: i32,
        n_past: i32,
        id_last: sys::llama_token,
        prompt: &[sys::llama_token],
        out: &mut Vec<sys::llama_token>,
    ) -> usize {
        unsafe {
            sys::llama_rs_speculative_draft(
                self.raw,
                seq_id,
                n_max_eff,
                n_past,
                id_last,
                prompt.as_ptr(),
                prompt.len(),
            );
        }
        out.clear();
        out.resize(n_max_eff.max(0) as usize, 0);
        let n = unsafe {
            sys::llama_rs_speculative_draft_result(self.raw, seq_id, out.as_mut_ptr(), out.len())
        };
        let n = n.min(out.len());
        out.truncate(n);
        n
    }

    // Potwierdza liczbę zaakceptowanych tokenów draftu dla danej sekwencji i domyka
    // wewnętrzny stan biblioteki (impl_last/pending_h). Wolno wołać WYŁĄCZNIE gdy
    // impl_last[seq_id] jest ustawiony, tj. po drafcie, który kiedykolwiek (w tej
    // lub wcześniejszej turze) zwrócił niepusty wynik — inaczej biblioteka robi
    // GGML_ASSERT(impl_last) → abort. Wywołanie z n_accepted=0 jest poprawne i
    // koryguje pending_h do pozycji id_last (domknięcie stanu single-token).
    fn accept(&mut self, seq_id: i32, n_accepted: u16) {
        unsafe { sys::llama_rs_speculative_accept(self.raw, seq_id, n_accepted) };
    }
}

#[cfg(feature = "llama")]
impl Drop for SpeculativeEngine {
    fn drop(&mut self) {
        // Kolejność jest istotna: shim trzyma surowy wskaźnik ctx_dft i w destruktorze
        // implementacji MTP woła na nim llama_set_sampler/llama_set_embeddings_nextn,
        // więc ctx_dft musi żyć aż do zakończenia free. Dopiero potem zwalniamy ctx_dft.
        unsafe { sys::llama_rs_speculative_free(self.raw) };
        if !self.ctx_dft.is_null() {
            unsafe { sys::llama_free(self.ctx_dft) };
            self.ctx_dft = std::ptr::null_mut();
        }
    }
}

#[cfg(feature = "llama")]
fn scheduler_main(
    model_path: std::path::PathBuf,
    config: EngineConfig,
    submit_rx: Receiver<EngineCommand>,
    ready_tx: Sender<Result<(), LlamaError>>,
    inflight: Arc<std::sync::atomic::AtomicUsize>,
) {
    let runtime = match LlamaRuntime::load(
        &model_path,
        crate::llama::LlamaLoadConfig {
            ctx_size: config.ctx_per_seq,
            n_gpu_layers: config.n_gpu_layers,
            batch_size: config.n_batch.max(config.n_seq_max),
            threads: config.threads,
            flash_attn: config.flash_attn,
            main_gpu: config.main_gpu,
            tensor_split: config.tensor_split.clone(),
        },
    ) {
        Ok(rt) => Arc::new(rt),
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let model = runtime.model_ptr();

    // n_ctx jest CAŁKOWITĄ pojemnością kontekstu współdzieloną przez wszystkie
    // sekwencje. Per-sekwencja llama.cpp dzieli ją równo: n_ctx_seq = n_ctx /
    // n_seq_max (gdy kv_unified=false). Aby każdy slot miał ctx_per_seq tokenów,
    // ustawiamy n_ctx = ctx_per_seq * n_seq_max i po utworzeniu weryfikujemy
    // realne llama_n_ctx_seq().
    let n_seq_max = config.n_seq_max;
    let n_ctx_total = config.ctx_per_seq.saturating_mul(n_seq_max).max(1);
    let n_batch = config.n_batch.max(n_seq_max).max(1);
    let n_ubatch = config.n_ubatch.max(1).min(n_batch);

    let ctx = match create_context(model, &config, n_ctx_total, n_batch, n_ubatch, n_seq_max) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };

    let ctx_per_seq = unsafe { sys::llama_n_ctx_seq(ctx.raw) };
    let real_n_batch = unsafe { sys::llama_n_batch(ctx.raw) } as i32;
    // Skonfigurowany górny limit długości draftu (0 = speculative wyłączone). Budżet
    // batcha (CR-003) zawsze przycina go do realnie wolnych slotów batcha per turę.
    let config_spec_n_max = match config.speculative {
        SpeculativeMode::Off => 0,
        SpeculativeMode::NgramSimple { n_max, .. } => n_max as i32,
        SpeculativeMode::Mtp { n_max } => n_max as i32,
    };

    // Drafter tworzony wyłącznie tu (w wątku schedulera). Inicjalizujemy go PRZED
    // ready_tx, by ewentualny błąd dotarł do load() zamiast zniknąć w wątku
    // (kontrakt no-fallback: brak draftera = błąd startu, nie cichy tryb bez draftu).
    // Dla MTP tworzymy drugi kontekst draftujący na tym samym modelu (zero
    // duplikacji wag) — oba konteksty oraz uchwyt speculative żyją WYŁĄCZNIE w tym
    // wątku i nigdy go nie opuszczają (inwariant Send/Sync silnika).
    let mut speculative: Option<SpeculativeEngine> = match config.speculative {
        SpeculativeMode::Off => None,
        SpeculativeMode::NgramSimple { n_max, n_min } => {
            match SpeculativeEngine::new_ngram(n_max, n_min, n_seq_max, config.n_rs_seq) {
                Ok(eng) => Some(eng),
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    drop(ctx);
                    drop(runtime);
                    return;
                }
            }
        }
        SpeculativeMode::Mtp { n_max } => {
            match SpeculativeEngine::new_mtp(
                n_max,
                config.n_rs_seq,
                model,
                ctx.raw,
                &config,
                ContextDims {
                    n_ctx_total,
                    n_batch,
                    n_ubatch,
                    n_seq: n_seq_max,
                },
            ) {
                Ok(eng) => Some(eng),
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    drop(ctx);
                    drop(runtime);
                    return;
                }
            }
        }
    };

    let _ = ready_tx.send(Ok(()));

    let mut slots: Vec<Slot> = (0..n_seq_max as i32).map(Slot::new).collect();
    let memory = unsafe { sys::llama_get_memory(ctx.raw) };
    // Pojemność batcha i limit is_full MUSZĄ równać się real_n_batch — to twardy
    // limit tokenów jednego llama_decode. Speculative dorzuca draftowe tokeny do
    // tego samego batcha, więc zawyżenie limitu (np. max z n_batch) skończyłoby się
    // rc!=0 z llama_decode.
    let mut batch = EngineBatch::new(real_n_batch);

    // Lokalna kolejka oczekujących zadań (gdy brak wolnych slotów).
    let mut waiting: std::collections::VecDeque<SlotJob> = std::collections::VecDeque::new();
    let mut shutdown = false;

    loop {
        // Pobierz komendy bez blokowania, jeśli mamy aktywną pracę. Jeśli wszystko
        // jest puste — blokuj na kanale aż przyjdzie nowy request lub shutdown.
        let any_active = slots.iter().any(|s| s.state != SlotState::Idle);
        let has_waiting = !waiting.is_empty();

        if !any_active && !has_waiting {
            match submit_rx.recv() {
                Ok(EngineCommand::Submit(job)) => waiting.push_back(job),
                Ok(EngineCommand::Shutdown) | Err(_) => break,
            }
        }
        // Drenuj resztę komend nieblokująco.
        loop {
            match submit_rx.try_recv() {
                Ok(EngineCommand::Submit(job)) => waiting.push_back(job),
                Ok(EngineCommand::Shutdown) => {
                    shutdown = true;
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    shutdown = true;
                    break;
                }
            }
        }
        if shutdown {
            break;
        }

        // CR-001: deadline postępu dostarczania egzekwujemy BEZWARUNKOWO raz na
        // iterację głównej pętli — także gdy współistnieją zdrowe, aktywnie
        // generujące sloty. Inaczej zablokowany ("żywy ale niemy" konsument) slot
        // trzymałby KV+inflight aż pozostałe sloty zejdą do Idle. enforce sam
        // bramkuje warunki (ubija tylko slot z niepustym pending po przekroczeniu
        // stream_stall_timeout), więc zdrowy wolno generujący slot z pustym pending
        // pozostaje nietknięty. Wołane dokładnie raz na iterację (gałąź bezczynności
        // już go NIE woła ponownie).
        for slot in slots.iter_mut() {
            enforce_stall_timeout(slot, memory, &inflight, config.stream_stall_timeout);
        }

        // (1) Przypisz oczekujące requesty do wolnych slotów.
        for slot in slots.iter_mut() {
            if slot.state != SlotState::Idle {
                continue;
            }
            let Some(job) = waiting.pop_front() else {
                break;
            };
            // Przy MTP czyścimy stan KV/rekurencyjny kontekstu draftującego dla tej
            // sekwencji PRZED nowym prefillem — ctx_tgt jest czyszczony w finish_slot,
            // ale ctx_dft żyje w shimie, więc resztki poprzedniej generacji
            // zniekształciłyby drafty nowej (wzorzec server-context.cpp:166).
            if let Some(spec) = speculative.as_mut() {
                spec.clear_seq(slot.seq_id);
            }
            start_job(slot, job, model, ctx_per_seq, speculative.is_some(), &inflight);
        }

        // (2) Zbuduj jeden batch: prefill każdego slotu Prefill + po jednym tokenie
        //     decode z każdego slotu Generating (plus draft ngram, gdy włączony).
        //     Konsument-zatkany slot pomijamy.
        batch.clear();
        for slot in slots.iter_mut() {
            slot.batch_logits_index = -1;
            slot.draft_logit_indices.clear();
            slot.draft_tokens.clear();
            // Rozliczenie speculative z poprzedniej tury zamknięte — zerujemy flagę,
            // by accept tej tury padał wyłącznie gdy realnie draftowaliśmy poniżej.
            slot.drafted_this_turn = false;
        }

        let mut scheduled_any = false;

        // Najpierw planujemy "prawdziwy" token (id_last) każdego generującego slotu —
        // po jednym — z poszanowaniem anty-hangu: jeśli kanał slotu pełny, pomijamy.
        // Token id_last NIE jest usuwany ze slotu dopóki nie trafi do batcha.
        for slot in slots.iter_mut() {
            if slot.state != SlotState::Generating {
                continue;
            }
            if batch.is_full() {
                break;
            }
            if !slot_can_accept(slot) {
                continue;
            }
            let Some(token) = slot.next_decode_token.take() else {
                continue;
            };
            let idx = batch.add(token, slot.pos, slot.seq_id, true);
            slot.batch_logits_index = idx;
            slot.id_last_in_flight = Some(token);
            scheduled_any = true;
        }

        // Następnie, gdy speculative włączone, dorzucamy do batcha drafty dla tych
        // slotów, dla których zaplanowano id_last. Strategia budżetu batcha (CR-003):
        // PRZED wywołaniem spec.draft() ustalamy efektywny limit n_max_eff =
        // min(config_n_max, pozostały_budżet_batcha). Biblioteka obcina draft do
        // n_max_eff (dp.n_max), więc liczba wydraftowanych == liczba wstawionych do
        // batcha == liczba weryfikowanych (zero rozjazdu draft↔verify). Gdy budżet
        // batcha = 0, NIE draftujemy wcale dla slotu (zachowanie jak single-token),
        // a flaga drafted_this_turn pozostaje false, więc accept tej tury nie padnie.
        if let Some(spec) = speculative.as_mut() {
            let mut draft_buf: Vec<sys::llama_token> = Vec::new();
            for slot in slots.iter_mut() {
                if slot.batch_logits_index < 0 {
                    continue;
                }
                let budget = real_n_batch - batch.len();
                let n_max_eff = config_spec_n_max.min(budget);
                if n_max_eff <= 0 {
                    continue;
                }
                // id_last to token zaplanowany do decode tej iteracji; history to
                // kontekst PRZED nim (drafter ngram sam dokleja id_last do wzorca).
                let Some(id_last) = slot.id_last_in_flight else {
                    continue;
                };
                let n_draft = spec.draft(
                    slot.seq_id,
                    n_max_eff,
                    slot.pos,
                    id_last,
                    &slot.history,
                    &mut draft_buf,
                );
                // Draftowaliśmy z budżetem > 0 — accept tej tury MUSI domknąć stan
                // biblioteki dla tego slotu (CR-002/CR-004), niezależnie ile tokenów
                // realnie wróciło. impl_last jest ustawiany dopiero przy niepustym
                // drafcie, więc accept stanie się dozwolony dopiero po nim.
                slot.drafted_this_turn = true;
                if n_draft > 0 {
                    slot.spec_impl_ready = true;
                }
                // MTP draft() dekoduje ctx_dft autoregresyjnie (id_last@slot.pos +
                // drafty@slot.pos+1..), zaawansowując jego KV. process() po decode
                // ctx_tgt ponownie zdekoduje cały batch weryfikacyjny na ctx_dft od
                // slot.pos, więc to wstępne zaawansowanie musi zniknąć — inaczej
                // M-RoPE odrzuci nieciągłe pozycje (wzorzec server-context.cpp:2552-2558).
                // No-op dla ngram (draft nie dotyka żadnego kontekstu).
                spec.rollback_dft_after_draft(slot.seq_id, slot.pos);
                if n_draft == 0 {
                    continue;
                }
                // pozycja id_last = slot.pos; drafty idą na pos+1, pos+2, ...
                // n_draft <= n_max_eff <= budżet, więc wszystkie tokeny wejdą do batcha.
                for (k, &dtok) in draft_buf.iter().enumerate() {
                    let dpos = slot.pos + 1 + k as i32;
                    let idx = batch.add(dtok, dpos, slot.seq_id, true);
                    slot.draft_logit_indices.push(idx);
                    slot.draft_tokens.push(dtok);
                }
            }
        }

        // Następnie kawałki prefillu (mogą wypełnić resztę batcha). Prefill nie
        // produkuje wyjścia aż do ostatniego tokena promptu, więc nie bramkujemy
        // go stanem konsumenta.
        for slot in slots.iter_mut() {
            if slot.state != SlotState::Prefill {
                continue;
            }
            while slot.prompt_consumed < slot.prompt.len() && !batch.is_full() {
                let is_last = slot.prompt_consumed + 1 == slot.prompt.len();
                let token = slot.prompt[slot.prompt_consumed];
                let idx = batch.add(token, slot.pos, slot.seq_id, is_last);
                slot.pos += 1;
                slot.prompt_consumed += 1;
                scheduled_any = true;
                if is_last {
                    slot.batch_logits_index = idx;
                }
            }
        }

        if !scheduled_any || batch.len() == 0 {
            // Nic do policzenia w tej iteracji. Jeśli istnieją aktywne sloty, to
            // znaczy że ich konsumenci są zatkani — próbujemy rozładować pending
            // i krótko śpimy (zamiast spinować CPU), aż konsument zwolni miejsce.
            for slot in slots.iter_mut() {
                flush_pending(slot, memory, &inflight);
            }
            // enforce_stall_timeout już zostało wywołane bezwarunkowo na początku
            // tej iteracji (patrz CR-001) — NIE powtarzamy go tutaj, by deadline
            // egzekwować dokładnie raz na turę.
            let still_active = slots.iter().any(|s| s.state != SlotState::Idle);
            if still_active {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            continue;
        }

        // (3) Jeden llama_decode dla całego batcha.
        let rc = unsafe { sys::llama_decode(ctx.raw, batch.raw) };
        if rc != 0 {
            // Twardy błąd dekodowania — domknij stan speculative slotów, które w tej
            // turze draftowały (inaczej kolejna instancja/Drop zostawiłaby otwarty
            // impl_last/pending_h), potem zakończ wszystkie aktywne sloty błędem.
            close_speculative_on_abort(&mut slots, speculative.as_mut());
            fail_active_slots(&mut slots, memory, &inflight, rc);
            continue;
        }

        // (3a) MTP: po decode kontekstu docelowego mirrorujemy embeddingi nextn
        //      target→draft (common_speculative_process). MUSI biec po KAŻDYM
        //      decode (także prefillu), inaczej stan ctx_dft / pending_h rozjedzie
        //      się i drafty będą bezwartościowe. No-op dla ngram. Błąd tu jest
        //      twardy (kontrakt no-fallback) — kończymy aktywne sloty.
        if let Some(spec) = speculative.as_mut() {
            if let Err(e) = spec.process(&batch.raw) {
                let msg = e.to_string();
                close_speculative_on_abort(&mut slots, Some(spec));
                fail_active_slots_msg(&mut slots, memory, &inflight, msg);
                continue;
            }
        }

        // (4) Per slot: pobierz logity, zweryfikuj draft (jeśli był), zatwierdź
        //     zaakceptowane tokeny, wyślij tekst, sprawdź warunki stopu i zrób
        //     rollback KV odrzuconych draftów.
        for slot in slots.iter_mut() {
            if slot.batch_logits_index < 0 {
                continue;
            }

            // Prefill kończy się gdy cały prompt skonsumowany → przejdź do generacji.
            if slot.state == SlotState::Prefill {
                if slot.prompt_consumed < slot.prompt.len() {
                    // Prefill jeszcze trwa (prompt nie zmieścił się w jednym batchu).
                    continue;
                }
                slot.state = SlotState::Generating;
                slot.gen_start = Some(std::time::Instant::now());
            }

            commit_generation(
                slot,
                ctx.raw,
                model,
                memory,
                &inflight,
                speculative.as_mut(),
                ctx_per_seq,
            );
        }

        // (5) Spróbuj rozładować zaległe tokeny zatkanych slotów (po zwolnieniu
        //     miejsca przez konsumenta) — bez blokowania.
        for slot in slots.iter_mut() {
            flush_pending(slot, memory, &inflight);
        }
    }

    // Sprzątanie przy shutdown: zwolnij aktywne sloty natychmiast. Konsumenci
    // zostaną rozłączeni przez drop sink (slots dropuje się na końcu funkcji), co
    // dla `tokio::mpsc` zamyka receiver bez zawieszania core.
    for slot in slots.iter_mut() {
        if slot.state != SlotState::Idle {
            release_slot(
                slot,
                memory,
                &inflight,
                FinishReason::Error("silnik zatrzymany".into()),
            );
        }
    }
    drop(batch);
    drop(ctx);
    drop(runtime);
}

// Zatwierdza wynik jednej iteracji decode dla slotu Generating.
//
// Bez draftu (speculative=None lub pusty draft): sampluje jeden token, zatwierdza
// go (pos+=1) i strumieniuje. Z draftem: weryfikuje sekwencyjnie zaakceptowany
// prefiks (sampled == draft[k]), zatwierdza zaakceptowane tokeny i robi rollback
// KV (seq_rm) odrzuconych draftów, utrzymując spójność pos/history/KV.
//
// Układ KV po decode: id_last na base_pos, draft[k] na base_pos+1+k. Sampler na
// logitach id_last daje token #0 (zawsze prawdziwy). Dla k: jeśli token #k ==
// draft[k], to draft[k] był trafiony i token #(k+1) = sample(logitów draft[k]);
// inaczej stop. Po m trafionych draftach KV jest poprawne na base_pos..base_pos+m
// (id_last + draft[0..m-1]); odrzucone drafty (base_pos+m+1..) usuwamy seq_rm.
// history (kontekst dla ngram) dostaje id_last + draft[0..m-1]; nowy prawdziwy
// token #m czeka jako next_decode_token (jeszcze nie w KV).
//
// Każdy WYPRODUKOWANY token (#0..#m) przechodzi pełne sprawdzenia EOG / stop /
// max_tokens / limitu kontekstu; pierwszy terminalny kończy slot i ucina resztę.
#[cfg(feature = "llama")]
fn commit_generation(
    slot: &mut Slot,
    ctx: *mut sys::llama_context,
    model: *mut sys::llama_model,
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    mut speculative: Option<&mut SpeculativeEngine>,
    ctx_per_seq: u32,
) {
    let spec_on = speculative.is_some();
    let base_pos = slot.pos; // pozycja id_last (Generating) albo pierwszego tokena gen (Prefill)
    let n_draft = slot.draft_tokens.len();
    let id_last = slot.id_last_in_flight.take();
    // Przejście Prefill→Generating nie ma id_last zdekodowanego w tej turze:
    // ostatni token promptu siedzi na base_pos-1, a token #0 zejdzie na base_pos
    // dopiero w następnej turze. Dla Generating id_last zajął base_pos w tej turze,
    // więc kolejne tokeny idą od base_pos+1. To przesunięcie (0 dla Prefill, 1 dla
    // Generating) wchodzi do wyliczenia nowej wolnej pozycji i rollbacku — bez tego
    // M-RoPE w ctx_dft (MTP) widzi nieciągłe pozycje i odrzuca batch.
    let pos_advance = if id_last.is_some() { 1 } else { 0 };

    // FAZA 1 — sampling: pod borrow samplera zbuduj listę wyprodukowanych tokenów
    // (#0..#m) przez sekwencyjną weryfikację draftu. Po tej fazie zwalniamy borrow
    // samplera, by móc modyfikować inne pola slotu.
    let (produced, matched_drafts) = {
        let Some(sampler) = slot.sampler.as_mut() else {
            finish_slot(slot, memory, inflight, FinishReason::Error("brak samplera".into()));
            return;
        };
        let mut produced: Vec<sys::llama_token> = Vec::with_capacity(n_draft + 1);
        let first = sampler.sample(ctx, slot.batch_logits_index);
        let _ = sampler.accept(first);
        produced.push(first);
        for k in 0..n_draft {
            if produced[k] != slot.draft_tokens[k] {
                break;
            }
            let next = sampler.sample(ctx, slot.draft_logit_indices[k]);
            let _ = sampler.accept(next);
            produced.push(next);
        }
        let matched = produced.len() - 1;
        (produced, matched)
    };

    // FAZA 2 — bookkeeping bez borrow samplera. Przetwarzamy wyprodukowane tokeny
    // (#0..#m) po kolei z pełnymi sprawdzeniami terminalnymi. KV osadza id_last na
    // base_pos i trafione drafty na base_pos+1.. ; produced[j] na pozycji logicznej
    // base_pos+1+j.
    for (j, &token) in produced.iter().enumerate() {
        // Liczba trafionych draftów osadzonych w KV gdy kończymy NA tokenie #j:
        // produced[0..j] były draftami (gdy j<=matched), więc min(j, matched).
        let embedded = j.min(matched_drafts);

        if is_eog_with_model(model, token) {
            let end = slot.generated_text.len();
            emit_text_until(slot, end);
            commit_history(slot, spec_on, id_last, &produced, embedded);
            commit_accept(slot, speculative.as_deref_mut(), embedded as u16);
            finish_slot(slot, memory, inflight, FinishReason::EndOfText);
            return;
        }

        let piece = token_to_piece_with_model(model, token, &mut slot.decoder);
        slot.generated += 1;
        slot.generated_text.push_str(&piece);

        if let Some(matched) = check_stop_sequence(&slot.generated_text, &slot.stop_sequences) {
            let matched = matched.to_string();
            let cut = slot.generated_text.len().saturating_sub(matched.len());
            emit_text_until(slot, cut);
            commit_history(slot, spec_on, id_last, &produced, embedded);
            commit_accept(slot, speculative.as_deref_mut(), embedded as u16);
            finish_slot(slot, memory, inflight, FinishReason::StopSequence(matched));
            return;
        }

        if slot.generated >= slot.max_tokens {
            emit_text_until(slot, slot.generated_text.len());
            commit_history(slot, spec_on, id_last, &produced, embedded);
            commit_accept(slot, speculative.as_deref_mut(), embedded as u16);
            finish_slot(slot, memory, inflight, FinishReason::MaxTokens);
            return;
        }

        // Limit kontekstu: następny decode trafi na base_pos+pos_advance+j.
        if base_pos + pos_advance + j as i32 >= ctx_per_seq as i32 {
            emit_text_until(slot, slot.generated_text.len());
            commit_history(slot, spec_on, id_last, &produced, embedded);
            commit_accept(slot, speculative.as_deref_mut(), embedded as u16);
            finish_slot(slot, memory, inflight, FinishReason::ContextFull);
            return;
        }
    }

    // Brak warunku terminalnego — strumieniuj z holdbackiem stop-sekwencji.
    let holdback = stop_holdback_len(&slot.generated_text, &slot.stop_sequences);
    let emittable = slot.generated_text.len().saturating_sub(holdback);
    emit_text_until(slot, emittable);

    // Nowy prawdziwy token #m czeka na decode w kolejnej iteracji (nie jest w KV).
    let last = *produced.last().expect("produced ma zawsze >=1 token");
    slot.next_decode_token = Some(last);

    // Trwale w KV tej tury: id_last + produced[0..m-1] (m trafionych draftów).
    commit_history(slot, spec_on, id_last, &produced, matched_drafts);

    // Rollback odrzuconych draftów: nowa pierwsza wolna pozycja.
    // Generating: base_pos(id_last) + m trafionych draftów + 1. Prefill: base_pos
    // (token #0 zejdzie tu w następnej turze), bo brak id_last i brak draftów.
    let new_pos = base_pos + matched_drafts as i32 + pos_advance;
    if matched_drafts < n_draft {
        unsafe {
            sys::llama_memory_seq_rm(memory, slot.seq_id, new_pos, -1);
        }
    }
    slot.pos = new_pos;
    if let Some(spec) = speculative {
        // Kolejność jak w server-context.cpp: najpierw accept (aktualizuje pending_h
        // MTP do zaakceptowanego tokena i domyka stan biblioteki), potem rollback KV
        // ctx_dft do tej samej pozycji co ctx_tgt. accept padać MUSI gdy slot draftował
        // w tej turze (CR-002/CR-004) — także przy matched_drafts=0 — więc bramkujemy
        // go flagą drafted_this_turn, nie liczbą tokenów. rollback dotyczy ctx_dft i
        // jest potrzebny tylko gdy drafty trafiły do batcha (process zaawansował ctx_dft).
        commit_accept(slot, Some(spec), matched_drafts as u16);
        if n_draft > 0 {
            spec.rollback_dft_seq(slot.seq_id, new_pos);
        }
    }
}

// Domyka stan speculative dla slotu w commit_generation: gdy slot draftował w tej
// turze (drafted_this_turn) i biblioteka ma już ustawiony impl_last (spec_impl_ready),
// woła accept(n_accepted) dokładnie raz i zeruje flagę, by żadna inna ścieżka wyjścia
// nie zaakceptowała ponownie. No-op gdy slot nie draftował lub impl_last nieustawiony
// (wtedy pending_h jest już poprawne z process(), a accept zrobiłby abort).
#[cfg(feature = "llama")]
fn commit_accept(slot: &mut Slot, speculative: Option<&mut SpeculativeEngine>, n_accepted: u16) {
    if !slot.drafted_this_turn {
        return;
    }
    slot.drafted_this_turn = false;
    if !slot.spec_impl_ready {
        return;
    }
    if let Some(spec) = speculative {
        spec.accept(slot.seq_id, n_accepted);
    }
}

// Dopisuje do history tokeny trwale osadzone w KV tej tury: id_last + pierwsze
// `matched` tokenów produced (trafione drafty). produced[matched] (nowy prawdziwy
// token) celowo NIE trafia do history — czeka jako next_decode_token. Gdy
// speculative wyłączone, history nie jest używana i nie rośnie.
#[cfg(feature = "llama")]
fn commit_history(
    slot: &mut Slot,
    spec_on: bool,
    id_last: Option<sys::llama_token>,
    produced: &[sys::llama_token],
    matched: usize,
) {
    if !spec_on {
        return;
    }
    if let Some(id) = id_last {
        slot.history.push(id);
    }
    for &t in produced.iter().take(matched) {
        slot.history.push(t);
    }
}

#[cfg(feature = "llama")]
fn create_context(
    model: *mut sys::llama_model,
    config: &EngineConfig,
    n_ctx_total: u32,
    n_batch: u32,
    n_ubatch: u32,
    n_seq_max: u32,
) -> Result<LlamaContextGuard, LlamaError> {
    let mut params = unsafe { sys::llama_context_default_params() };
    params.n_ctx = n_ctx_total;
    params.n_batch = n_batch;
    params.n_ubatch = n_ubatch;
    params.n_seq_max = n_seq_max;
    params.n_rs_seq = config.n_rs_seq;
    params.kv_unified = config.kv_unified;
    params.flash_attn_type = match config.flash_attn {
        FlashAttentionMode::Auto => sys::LLAMA_FLASH_ATTN_TYPE_AUTO,
        FlashAttentionMode::Off => sys::LLAMA_FLASH_ATTN_TYPE_DISABLED,
        FlashAttentionMode::On => sys::LLAMA_FLASH_ATTN_TYPE_ENABLED,
    };
    if let Some(threads) = config.threads {
        params.n_threads = threads as i32;
        params.n_threads_batch = threads as i32;
    }

    let raw = unsafe { sys::llama_init_from_model(model, params) };
    if raw.is_null() {
        return Err(LlamaError::ContextFailed);
    }
    Ok(LlamaContextGuard { raw })
}

// Wymiary kontekstu współdzielone przez kontekst docelowy i draftujący MTP.
#[cfg(feature = "llama")]
#[derive(Clone, Copy)]
struct ContextDims {
    n_ctx_total: u32,
    n_batch: u32,
    n_ubatch: u32,
    n_seq: u32,
}

// Tworzy kontekst draftujący MTP na TYM SAMYM modelu co kontekst docelowy (zero
// duplikacji wag). Zwraca surowy wskaźnik (nie guard), bo własność przejmuje shim
// speculative i zwolnienie musi nastąpić PO llama_rs_speculative_free.
//
// Parametry wzorowane na server-context.cpp:961-968: te same n_ctx/n_batch/
// n_seq_max co ctx_tgt, ctx_type=MTP, n_rs_seq=0 (draft MTP nie rollbackuje
// rekurencyjnie), n_outputs_max=n_seq (po jednym wyjściu na sekwencję w batchu).
#[cfg(feature = "llama")]
fn create_mtp_draft_context(
    model: *mut sys::llama_model,
    config: &EngineConfig,
    dims: ContextDims,
) -> Result<*mut sys::llama_context, LlamaError> {
    let mut params = unsafe { sys::llama_context_default_params() };
    params.n_ctx = dims.n_ctx_total;
    params.n_batch = dims.n_batch;
    params.n_ubatch = dims.n_ubatch;
    params.n_seq_max = dims.n_seq;
    params.n_rs_seq = 0;
    params.n_outputs_max = dims.n_seq;
    params.ctx_type = sys::LLAMA_CONTEXT_TYPE_MTP;
    params.kv_unified = config.kv_unified;
    params.flash_attn_type = match config.flash_attn {
        FlashAttentionMode::Auto => sys::LLAMA_FLASH_ATTN_TYPE_AUTO,
        FlashAttentionMode::Off => sys::LLAMA_FLASH_ATTN_TYPE_DISABLED,
        FlashAttentionMode::On => sys::LLAMA_FLASH_ATTN_TYPE_ENABLED,
    };
    if let Some(threads) = config.threads {
        params.n_threads = threads as i32;
        params.n_threads_batch = threads as i32;
    }

    let raw = unsafe { sys::llama_init_from_model(model, params) };
    if raw.is_null() {
        return Err(LlamaError::ContextFailed);
    }
    Ok(raw)
}

// Przypisuje zadanie do wolnego slotu. Błędy walidacji/przygotowania są
// zgłaszane jako finalny StreamToken do konsumenta (slot zostaje Idle).
#[cfg(feature = "llama")]
fn start_job(
    slot: &mut Slot,
    job: SlotJob,
    model: *mut sys::llama_model,
    ctx_per_seq: u32,
    speculative_on: bool,
    inflight: &std::sync::atomic::AtomicUsize,
) {
    let SlotJob { request, mut sink } = job;
    let full_prompt = match &request.system_prompt {
        Some(system) if !system.is_empty() => format!("{system}\n\n{}", request.prompt),
        _ => request.prompt.clone(),
    };

    let tokens = match tokenize_with_model(model, &full_prompt, true) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            reject_job(&mut sink, inflight, FinishReason::Error("pusty prompt".into()));
            return;
        }
        Err(e) => {
            reject_job(&mut sink, inflight, FinishReason::Error(e.to_string()));
            return;
        }
    };

    if tokens.len() >= ctx_per_seq as usize {
        reject_job(&mut sink, inflight, FinishReason::PromptTooLong);
        return;
    }

    let sampler = match build_sampler_chain(
        request.sampling.repeat_penalty,
        request.sampling.top_k,
        request.sampling.top_p,
        request.sampling.temperature,
        request.sampling.seed,
    ) {
        Ok(s) => s,
        Err(e) => {
            reject_job(&mut sink, inflight, FinishReason::Error(e.to_string()));
            return;
        }
    };

    slot.reset();
    slot.state = SlotState::Prefill;
    slot.prefill_start = std::time::Instant::now();
    // Drafter ngram potrzebuje pełnego kontekstu sekwencji — seedujemy history
    // promptem. Po prefillu history = prompt, dalej rośnie o zaakceptowane tokeny
    // i pozostaje spójna z zawartością KV.
    if speculative_on {
        slot.history = tokens.clone();
    }
    slot.prompt = tokens;
    slot.prompt_consumed = 0;
    slot.pos = 0;
    slot.max_tokens = request.max_tokens.max(1);
    slot.stop_sequences = request.stop_sequences;
    slot.sampler = Some(sampler);
    slot.sink = Some(sink);
}

#[cfg(feature = "llama")]
fn reject_job(
    sink: &mut Box<dyn EngineSink>,
    inflight: &std::sync::atomic::AtomicUsize,
    reason: FinishReason,
) {
    let _ = sink.try_send(StreamToken {
        text: String::new(),
        is_final: true,
        finish_reason: Some(reason),
        generated_tokens: 0,
        prompt_tokens: 0,
        prefill_tps: 0.0,
        completion_tps: 0.0,
        ttft_ms: 0,
    });
    inflight.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
}

// Wysyła do strumienia jeszcze niewysłany fragment tekstu [sent_len, target).
// target jest przycinany w dół do granicy znaku UTF-8.
#[cfg(feature = "llama")]
fn emit_text_until(slot: &mut Slot, target: usize) {
    let mut target = target.min(slot.generated_text.len());
    while target > slot.sent_len && !slot.generated_text.is_char_boundary(target) {
        target -= 1;
    }
    if target <= slot.sent_len {
        return;
    }
    let chunk = slot.generated_text[slot.sent_len..target].to_string();
    slot.sent_len = target;
    if !chunk.is_empty() {
        enqueue_token(
            slot,
            StreamToken {
                text: chunk,
                is_final: false,
                finish_reason: None,
                generated_tokens: 0,
                prompt_tokens: 0,
                prefill_tps: 0.0,
                completion_tps: 0.0,
                ttft_ms: 0,
            },
        );
    }
}

// Długość końcówki tekstu, którą należy wstrzymać, bo może być prefiksem
// którejś stop-sekwencji (np. częściowe "</" przy stop "</s>").
#[cfg(feature = "llama")]
fn stop_holdback_len(text: &str, stop_sequences: &[String]) -> usize {
    let mut max_hold = 0;
    let text_len = text.len();
    for stop in stop_sequences {
        if stop.is_empty() {
            continue;
        }
        let max_len = stop.len().min(text_len);
        // Iterujemy tylko po końcówkach wyrównanych do granicy znaku UTF-8.
        for len in (1..=max_len).rev() {
            let start = text_len - len;
            if !text.is_char_boundary(start) {
                continue;
            }
            let tail = &text[start..];
            if stop.as_bytes().starts_with(tail.as_bytes()) {
                max_hold = max_hold.max(len);
                break;
            }
        }
    }
    max_hold
}

// Slot generujący może przyjąć kolejny token tylko gdy nie ma zaległych (pending)
// tokenów ani rozłączonego konsumenta — inaczej pominęlibyśmy ordering / blokowali.
#[cfg(feature = "llama")]
fn slot_can_accept(slot: &Slot) -> bool {
    slot.pending.is_empty() && slot.sink.is_some() && slot.finishing.is_none()
}

// Wsadza token do strumienia. Gdy kanał pełny — odkłada na koniec bufora pending
// (nie blokuje). Gdy są już zaległe tokeny, nowy też idzie do pending (FIFO).
// Gdy konsument rozłączony — oznacza slot do zakończenia (tx=None); finalizację
// robi finish/flush.
#[cfg(feature = "llama")]
fn enqueue_token(slot: &mut Slot, token: StreamToken) {
    let Some(sink) = slot.sink.as_mut() else {
        return;
    };
    // Zachowaj kolejność: jeśli coś już czeka, nie wyprzedzaj go nowym tokenem.
    if !slot.pending.is_empty() {
        slot.pending.push_back(token);
        return;
    }
    match sink.try_send(token) {
        SinkStatus::Delivered => {
            slot.last_progress = std::time::Instant::now();
        }
        SinkStatus::Full(t) => {
            slot.pending.push_back(t);
        }
        SinkStatus::Closed => {
            // Konsument zniknął — oznacz do zakończenia przez wyzerowanie sink.
            slot.sink = None;
        }
    }
}

// Próbuje wysłać zaległe tokeny; jeśli kanał znów pełny — zostawia resztę na
// później (bez blokowania innych slotów). Jeśli konsument rozłączony — zwalnia
// slot. Gdy slot jest w trybie deferred-finish (`finishing=Some`) i pending się
// opróżnił, dopiero wtedy domyka slot (seq_rm + reset + dekrement inflight).
#[cfg(feature = "llama")]
fn flush_pending(
    slot: &mut Slot,
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
) {
    if slot.state == SlotState::Idle {
        return;
    }

    // Drenuj bufor pending dopóki kanał przyjmuje (try_send, nigdy blokująco).
    // Każde realne dostarczenie z pending to POSTĘP — odświeża last_progress, więc
    // deadline stall (CR-001) liczy wyłącznie zastój dostarczania, a nie czas
    // konsumenta, który normalnie odbiera w swoim tempie.
    while let Some(pending) = slot.pending.pop_front() {
        let Some(sink) = slot.sink.as_mut() else {
            // Konsument zniknął zanim opróżniliśmy ogon — przestajemy próbować.
            break;
        };
        match sink.try_send(pending) {
            SinkStatus::Delivered => {
                slot.last_progress = std::time::Instant::now();
            }
            SinkStatus::Full(t) => {
                slot.pending.push_front(t);
                return;
            }
            SinkStatus::Closed => {
                slot.sink = None;
                break;
            }
        }
    }

    // Konsument rozłączony (w trakcie generacji) — zwolnij slot natychmiast.
    if slot.sink.is_none() && slot.finishing.is_none() {
        release_slot(
            slot,
            memory,
            inflight,
            FinishReason::Error("konsument rozłączony".into()),
        );
        return;
    }

    // Deferred finish: token finalny i cały ogon dostarczone → realne sprzątanie.
    if slot.finishing.is_some() && slot.pending.is_empty() {
        let reason = slot.finishing.take().unwrap_or(FinishReason::EndOfText);
        release_slot(slot, memory, inflight, reason);
    }
}

// CR-001: deadline postępu dostarczania. Slot, który ma zaległe tokeny (pending),
// żywego konsumenta (sink obecny — rozłączony obsługuje flush_pending) i od
// `timeout` nie zrobił ŻADNEGO postępu w opróżnianiu pending, jest siłą zwalniany.
// To eliminuje „żywego ale niemego" konsumenta, który nigdy nie czyta i nie
// rozłącza się: bez tego trzymałby slot + KV + inflight w nieskończoność i po
// wyczerpaniu queue_capacity silnik odmawiałby WSZYSTKICH przyjęć (cichy hang).
// Pusty pending = brak zastoju (slot normalnie czeka na swoją turę decode), więc
// takiego slotu nigdy nie liczymy jako stall. release_slot zwalnia slot, KV
// (seq_rm) i dekrementuje inflight; pozostałe sloty pracują dalej bez przeszkód.
#[cfg(feature = "llama")]
fn enforce_stall_timeout(
    slot: &mut Slot,
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    timeout: std::time::Duration,
) {
    if slot.state == SlotState::Idle {
        return;
    }
    if slot.pending.is_empty() || slot.sink.is_none() {
        return;
    }
    if slot.last_progress.elapsed() < timeout {
        return;
    }
    release_slot(
        slot,
        memory,
        inflight,
        FinishReason::Error("konsument zablokowany (stall timeout)".into()),
    );
}

// Rozpoczyna zakończenie slotu BEZ blokowania scheduler-a: token finalny trafia
// na koniec bufora pending (za ewentualnym jeszcze niedostarczonym ogonem
// tekstu), slot przechodzi w tryb deferred-finish, po czym próbujemy raz
// opróżnić pending. Jeśli kanał konsumenta jest pełny, zaległości (w tym token
// finalny) dostarczy `flush_pending` w kolejnych iteracjach pętli — nie
// wstrzymując pozostałych slotów (anty-hang). Realne sprzątanie (seq_rm + reset
// + dekrement inflight) robi `release_slot` dopiero gdy pending się opróżni.
#[cfg(feature = "llama")]
fn finish_slot(
    slot: &mut Slot,
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    reason: FinishReason,
) {
    // Idempotencja: gdy slot już kończy, nie dokładaj drugiego tokena finalnego.
    if slot.finishing.is_some() {
        return;
    }
    if slot.sink.is_none() {
        // Konsument zniknął wcześniej — nic nie dostarczamy, tylko zwalniamy.
        release_slot(slot, memory, inflight, reason);
        return;
    }
    slot.finishing = Some(reason.clone());
    // Przepustowość faz mierzona po realnych granicach slotu: prefill = od startu
    // requestu do przejścia w Generating, dekodowanie = od tego przejścia do finału.
    // Dekodowanie liczymy od (generated-1), bo pierwszy token powstaje jeszcze w
    // ramach prefillu — inaczej zaniżalibyśmy tok/s o jeden forward pass.
    let prefill_secs = slot
        .gen_start
        .map(|g| g.duration_since(slot.prefill_start).as_secs_f32())
        .unwrap_or(0.0);
    let decode_secs = slot.gen_start.map(|g| g.elapsed().as_secs_f32()).unwrap_or(0.0);
    let prefill_tps = if prefill_secs > 0.0 {
        slot.prompt.len() as f32 / prefill_secs
    } else {
        0.0
    };
    let completion_tps = if decode_secs > 0.0 && slot.generated > 1 {
        (slot.generated - 1) as f32 / decode_secs
    } else {
        0.0
    };
    // TTFT silnika = czas fazy prefill (start requestu → pierwszy token dekodowania).
    // To ta sama granica co prefill_secs, więc TTFT i prefill_tps są spójne i wolne od
    // zniekształceń zegara ściennego konsumenta (queue, buforowanie kanału).
    let ttft_ms = (prefill_secs * 1000.0).round() as u32;
    slot.pending.push_back(StreamToken {
        text: String::new(),
        is_final: true,
        finish_reason: Some(reason),
        generated_tokens: slot.generated,
        // Slot trzyma prompt aż do reset(), więc to realna liczba tokenów promptu.
        prompt_tokens: slot.prompt.len() as u32,
        prefill_tps,
        completion_tps,
        ttft_ms,
    });
    flush_pending(slot, memory, inflight);
}

// Faktyczne zwolnienie slotu: czyści KV sekwencji, resetuje stan, dekrementuje
// licznik requestów w locie. Wołane gdy ogon + token finalny zostały dostarczone
// albo gdy konsument się rozłączył.
#[cfg(feature = "llama")]
fn release_slot(
    slot: &mut Slot,
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    _reason: FinishReason,
) {
    unsafe {
        sys::llama_memory_seq_rm(memory, slot.seq_id, -1, -1);
    }
    slot.reset();
    inflight.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
}

// Domyka wewnętrzny stan speculative (impl_last/pending_h) dla każdego slotu, który
// w bieżącej turze draftował, gdy tura kończy się błędem PRZED commit_generation
// (decode rc!=0 albo błąd process). Każdy taki slot dostaje accept(0) dokładnie raz;
// zerujemy drafted_this_turn, by żadna późniejsza ścieżka nie zaakceptowała ponownie.
// No-op gdy speculative wyłączone. accept wołamy tylko gdy impl_last jest ustawiony
// (spec_impl_ready), inaczej GGML_ASSERT(impl_last) zrobiłby abort.
#[cfg(feature = "llama")]
fn close_speculative_on_abort(slots: &mut [Slot], speculative: Option<&mut SpeculativeEngine>) {
    let Some(spec) = speculative else {
        return;
    };
    for slot in slots.iter_mut() {
        if slot.drafted_this_turn {
            if slot.spec_impl_ready {
                spec.accept(slot.seq_id, 0);
            }
            slot.drafted_this_turn = false;
        }
    }
}

#[cfg(feature = "llama")]
fn fail_active_slots(
    slots: &mut [Slot],
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    rc: i32,
) {
    fail_active_slots_msg(slots, memory, inflight, format!("llama_decode rc={rc}"));
}

#[cfg(feature = "llama")]
fn fail_active_slots_msg(
    slots: &mut [Slot],
    memory: sys::llama_memory_t,
    inflight: &std::sync::atomic::AtomicUsize,
    msg: String,
) {
    for slot in slots.iter_mut() {
        if slot.state != SlotState::Idle {
            finish_slot(slot, memory, inflight, FinishReason::Error(msg.clone()));
        }
    }
}

#[cfg(not(feature = "llama"))]
pub struct LlamaEngine;

#[cfg(not(feature = "llama"))]
impl LlamaEngine {
    pub fn load(_model_path: &Path, _config: EngineConfig) -> Result<Self, LlamaError> {
        Err(LlamaError::FeatureDisabled)
    }

    pub fn submit(&self, _request: GenRequest) -> Result<RequestStream, LlamaError> {
        Err(LlamaError::FeatureDisabled)
    }
}

// Inwariant bezpieczeństwa: LlamaEngine NIE trzyma żadnego raw wskaźnika do
// modelu/kontekstu/batcha/pamięci ani uchwytu speculative — te żyją wyłącznie w
// wątku `scheduler_main` i nigdy go nie opuszczają. Pola LlamaEngine to wyłącznie
// kanały mpsc, JoinHandle i atomiki, które same są Send+Sync. Dlatego ręczne
// `unsafe impl` jest poprawne. NIE wolno dodawać tu pól z raw ctx/model/spec —
// złamałoby to inwariant i pozwoliło na współdzielenie nie-thread-safe zasobów.
#[cfg(feature = "llama")]
unsafe impl Send for LlamaEngine {}
#[cfg(feature = "llama")]
unsafe impl Sync for LlamaEngine {}
