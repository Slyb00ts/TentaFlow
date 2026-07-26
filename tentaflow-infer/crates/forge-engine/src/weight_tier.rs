// ===== File: weight_tier.rs — rezydencja wag: VRAM, a nadmiar w pamięci hosta =====
//
// Model większy niż VRAM nie może się załadować w całości na kartę, ale nie musi:
// pamięć przypięta hosta (`hipHostMalloc` / `cudaHostAlloc`) jest adresowalna
// przez GPU, więc kernel czyta z niej wprost przez PCIe. Zmierzone na RX 6900 XT
// (PCIe 4.0 x16), warstwa 108 MB, GEMV Q4_0:
//
//   wagi w VRAM                      138 µs   (816 GB/s, część z Infinity Cache)
//   kernel czyta wprost z hosta     4004 µs   ( 28 GB/s — pełne pasmo PCIe)
//   kopia do VRAM + obliczenia      4420 µs   ( 25 GB/s)
//
// Odczyt wprost jest SZYBSZY od kopiowania warstwy do VRAM, bo transfer nakłada
// się na obliczenia sam z siebie — nie trzeba slotów, podwójnego buforowania ani
// prefetcha. Dlatego rezydencja sprowadza się do jednej decyzji przy alokacji.
//
// Polityka jest celowo samodostrajająca: próbujemy VRAM, a gdy pula wag się
// wyczerpie, ta sama waga ląduje w pamięci hosta. Kolejność ładowania (warstwy
// rosnąco) decyduje więc, że w VRAM zostaje początek modelu, a ogon jest
// strumieniowany.
//
// ZNANE OGRANICZENIE — prefill. Odczyt wprost jest optymalny dla dekodowania,
// gdzie każda waga jest czytana RAZ na token. Prefill liczy GEMM kaflami tokenów
// i czyta wagę raz na kafel, więc przez PCIe płaci tyle razy, ile jest kafli.
// Zmierzone na ThinkingCap-27B (1,32 GiB w hoście): prefill 128 tokenów 5889 ms,
// 512 tokenów 23727 ms — dokładnie 4x, przepustowość stoi na 21,7 tok/s zamiast
// rosnąć z długością promptu. Lekarstwem jest zestagowanie warstwy do VRAM raz
// na prefill i policzenie z niej wszystkich kafli; dla dekodowania staging jest
// natomiast wolniejszy od odczytu wprost (zmierzone 4538 vs 4005 us).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use forge_hal::{
    DevBuffer, Device, Event, ExecGraph, KernelHandle, LaunchArgs, LaunchConfig, Module, Pool,
    Stream,
};
use forge_types::{DeviceCaps, MemKind, Result};

/// Ile wag wylądowało gdzie — do raportu po załadowaniu modelu.
#[derive(Debug, Default, Clone, Copy)]
pub struct WeightResidency {
    pub vram_bytes: usize,
    pub host_bytes: usize,
}

impl WeightResidency {
    pub fn total(&self) -> usize {
        self.vram_bytes + self.host_bytes
    }

    /// Ułamek wag czytanych przez PCIe zamiast z VRAM.
    pub fn host_fraction(&self) -> f64 {
        if self.total() == 0 {
            return 0.0;
        }
        self.host_bytes as f64 / self.total() as f64
    }
}

// Sprawdzone i odrzucone pomiarem: proaktywne spychanie DUZYCH tensorow, zeby
// zostawic w VRAM zapas na drobne (skale NVFP4, normy). Wersja z progiem 4 MiB i
// zapasem 768 MiB zeszla z 3,0 do 2,6 tok/s, bo do hosta trafialo 2,05 GiB
// zamiast 1,32 — koszt wiekszego strumieniowania przewyzszyl zysk. Osobny pomiar
// potwierdzil, ze drobne dostepy nie sa tu problemem: GEMV NVFP4 czytajacy
// wprost z hosta osiaga pelne 28 GB/s, tyle samo co Q4_0.

/// Nakładka na urządzenie: przekierowuje alokacje puli wag do pamięci hosta, gdy
/// VRAM się kończy. Pozostałe pule (KV, aktywacje) i pozostałe metody idą bez
/// zmian do urządzenia pod spodem.
pub struct TieredWeightDevice {
    inner: Arc<dyn Device>,
    vram: AtomicUsize,
    host: AtomicUsize,
    /// Twardy limit pamięci hosta; 0 wyłącza strumieniowanie (zachowanie sprzed
    /// tieringu: brak miejsca w VRAM to błąd).
    host_budget: usize,
}

impl TieredWeightDevice {
    pub fn new(inner: Arc<dyn Device>, host_budget: usize) -> Self {
        Self {
            inner,
            vram: AtomicUsize::new(0),
            host: AtomicUsize::new(0),
            host_budget,
        }
    }

    pub fn residency(&self) -> WeightResidency {
        WeightResidency {
            vram_bytes: self.vram.load(Ordering::Relaxed),
            host_bytes: self.host.load(Ordering::Relaxed),
        }
    }
}

impl Device for TieredWeightDevice {
    fn alloc(&self, bytes: usize, kind: MemKind, pool: Pool) -> Result<DevBuffer> {
        if !matches!(pool, Pool::Weights) || !matches!(kind, MemKind::Device) {
            return self.inner.alloc(bytes, kind, pool);
        }
        match self.inner.alloc(bytes, kind, pool) {
            Ok(buf) => {
                self.vram.fetch_add(bytes, Ordering::Relaxed);
                Ok(buf)
            }
            Err(vram_err) => {
                let used = self.host.load(Ordering::Relaxed);
                if used + bytes > self.host_budget {
                    return Err(vram_err);
                }
                let buf = self.inner.alloc(bytes, MemKind::PinnedHost, pool)?;
                self.host.fetch_add(bytes, Ordering::Relaxed);
                Ok(buf)
            }
        }
    }

    fn sub_buffer(&self, parent: &DevBuffer, offset: usize, len: usize) -> Result<DevBuffer> {
        self.inner.sub_buffer(parent, offset, len)
    }

    fn caps(&self) -> &DeviceCaps {
        self.inner.caps()
    }
    fn pool_available(&self, pool: Pool) -> Option<usize> {
        self.inner.pool_available(pool)
    }
    fn create_stream(&self) -> Result<Stream> {
        self.inner.create_stream()
    }
    fn create_event(&self) -> Result<Event> {
        self.inner.create_event()
    }
    fn create_timing_event(&self) -> Result<Event> {
        self.inner.create_timing_event()
    }
    fn record_event(&self, event: &Event, stream: &Stream) -> Result<()> {
        self.inner.record_event(event, stream)
    }
    fn wait_event(&self, stream: &Stream, event: &Event) -> Result<()> {
        self.inner.wait_event(stream, event)
    }
    fn elapsed_event_ms(&self, start: &Event, end: &Event) -> Result<Option<f32>> {
        self.inner.elapsed_event_ms(start, end)
    }
    fn copy(
        &self,
        src: &DevBuffer,
        src_offset: usize,
        dst: &DevBuffer,
        dst_offset: usize,
        bytes: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.inner.copy(src, src_offset, dst, dst_offset, bytes, stream)
    }
    fn write(&self, src: &[u8], dst: &DevBuffer, dst_offset: usize) -> Result<()> {
        self.inner.write(src, dst, dst_offset)
    }
    fn read(&self, src: &DevBuffer, src_offset: usize, dst: &mut [u8]) -> Result<()> {
        self.inner.read(src, src_offset, dst)
    }
    fn load_module(&self, image: &[u8]) -> Result<Module> {
        self.inner.load_module(image)
    }
    fn launch(
        &self,
        kernel: &KernelHandle,
        config: &LaunchConfig,
        args: &LaunchArgs,
        stream: &Stream,
    ) -> Result<()> {
        self.inner.launch(kernel, config, args, stream)
    }
    fn synchronize(&self) -> Result<()> {
        self.inner.synchronize()
    }
    fn begin_capture(&self, stream: &Stream) -> Result<()> {
        self.inner.begin_capture(stream)
    }
    fn end_capture(&self, stream: &Stream) -> Result<ExecGraph> {
        self.inner.end_capture(stream)
    }
    fn launch_graph(&self, graph: &ExecGraph, stream: &Stream) -> Result<()> {
        self.inner.launch_graph(graph, stream)
    }
    fn reset_activations(&self) -> Result<u64> {
        self.inner.reset_activations()
    }
}
