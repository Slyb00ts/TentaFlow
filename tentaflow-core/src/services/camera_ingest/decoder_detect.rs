// =============================================================================
// Plik: services/camera_ingest/decoder_detect.rs
// Opis: Cross-platformowa detekcja sprzętowego dekodera wideo dla GStreamera.
//       Sprawdza, które elementy dekodujące są zarejestrowane w runtime
//       (przez ElementFactory::find) i dobiera profil sprzętowy (liczba kamer,
//       preferencja HW) automatycznie, bez ręcznej konfiguracji. Fallback na
//       dekodowanie programowe (Software) gdy żaden plugin HW nie jest obecny.
// Przykład:
//   let hw = detect_hw_decoder();           // np. HwDecoder::Vaapi
//   let profile = detect_profile();         // max_cameras, prefer_hw, ...
// =============================================================================

use std::sync::OnceLock;

use gstreamer as gst;
use serde::{Deserialize, Serialize};

/// Rodzina sprzętowego dekodera wideo wykrytego w systemie. `Software` oznacza
/// brak akceleracji — dekodowanie na CPU (zawsze dostępne, fallback).
///
/// Detekcja jest runtime'owa: GStreamer ładuje pluginy z systemu, więc o
/// dostępności decyduje obecność `ElementFactory` danego dekodera, a NIE
/// flagi Cargo. Dlatego ten sam binarny build zachowa się różnie na różnym
/// sprzęcie — dokładnie tego wymaga przenośność (Linux/Windows/macOS/iOS/Android).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HwDecoder {
    /// NVIDIA nvcodec / Jetson nvv4l2 (CUDA / NVDEC).
    Nvidia,
    /// Apple VideoToolbox (macOS / iOS).
    VideoToolbox,
    /// Windows Direct3D11 video decode.
    D3d11,
    /// Linux VA-API (Intel / AMD) — nowy element `vah264dec` lub stary `vaapi*`.
    Vaapi,
    /// Android MediaCodec (amcviddec / OMX / Codec2).
    MediaCodec,
    /// Brak akceleracji — dekodowanie programowe na CPU.
    Software,
}

impl HwDecoder {
    /// Czy to dekoder sprzętowy (cokolwiek poza `Software`).
    pub fn is_hardware(self) -> bool {
        !matches!(self, HwDecoder::Software)
    }

    /// Krótka, stabilna etykieta do logów i telemetrii.
    pub fn label(self) -> &'static str {
        match self {
            HwDecoder::Nvidia => "nvidia",
            HwDecoder::VideoToolbox => "videotoolbox",
            HwDecoder::D3d11 => "d3d11",
            HwDecoder::Vaapi => "vaapi",
            HwDecoder::MediaCodec => "mediacodec",
            HwDecoder::Software => "software",
        }
    }
}

/// Lekki profil sprzętowy dobierany automatycznie na podstawie wykrytego
/// dekodera i liczby rdzeni CPU. To NIE jest pełny tuner — tylko sensowny
/// default, żeby na słabym sprzęcie uruchomić mniej kamer, a na mocnym więcej.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareProfile {
    /// Wykryty dekoder (sprzętowy lub Software).
    pub hw_decoder: HwDecoder,
    /// Sugerowana górna granica liczby równoległych kamer dla tej maszyny.
    pub max_cameras: u32,
    /// Czy preferować ścieżkę sprzętową, gdy dostępna. Przy `Software` zawsze false.
    pub prefer_hw: bool,
}

/// Lista kandydatów na element dekodujący dla każdej rodziny HW. Kolejność =
/// preferencja (najpierw nowsze / wydajniejsze elementy). Pierwszy znaleziony
/// `ElementFactory` wygrywa. `decodebin` i tak autopluguje konkretny element —
/// nam wystarczy wiedzieć, że JAKIŚ dekoder z danej rodziny istnieje.
fn candidates(decoder: HwDecoder) -> &'static [&'static str] {
    match decoder {
        HwDecoder::Nvidia => &["nvv4l2decoder", "nvh264dec", "nvh265dec"],
        HwDecoder::VideoToolbox => &["vtdec_hw", "vtdec"],
        HwDecoder::D3d11 => &["d3d11h264dec", "d3d11h265dec", "mfh264dec", "mfh265dec"],
        HwDecoder::Vaapi => &[
            "vah264dec",
            "vah265dec",
            "vaapih264dec",
            "vaapih265dec",
        ],
        HwDecoder::MediaCodec => &[
            "amcviddec-omxgoogleh264decoder",
            "amcviddec-c2qtiavcdecoder",
            "amcviddec-omxqcomvideodecoderavc",
        ],
        HwDecoder::Software => &[],
    }
}

/// Czy w runtime istnieje którykolwiek z kandydujących elementów dla danej
/// rodziny. `ElementFactory::find` jest tanie i nie panikuje — zwraca None,
/// gdy element nie jest zarejestrowany.
fn family_present(decoder: HwDecoder) -> bool {
    candidates(decoder)
        .iter()
        .any(|name| gst::ElementFactory::find(name).is_some())
}

/// Czy w runtime istnieje konkretny element GStreamera. Cienki helper nad
/// `ElementFactory::find`, używany przez detekcję ścieżki GPU-resident, żeby
/// nie powtarzać `is_some()` przy każdym elemencie łańcucha CUDA.
fn element_present(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

/// Czy dostępna jest ścieżka ingestu GPU-resident dla NVIDIA: dekoder NVDEC,
/// konwersja kolorów na GPU i download do pamięci hosta są zarejestrowane jako
/// elementy GStreamera, a wykryty dekoder to NVIDIA. Spełnienie tego warunku
/// pozwala zbudować łańcuch `nvhXdec → cudaconvert → cudadownload → ... → RGB`,
/// w którym dekoding I konwersja kolorów dzieją się na GPU, a na CPU schodzi
/// dopiero pełna klatka RGB (ten sam kontrakt appsink co ścieżka CPU).
///
/// Wymagamy `cudaconvert` (NV12→RGB na GPU) i `cudadownload` (CUDAMemory→host)
/// oraz przynajmniej jednego dekodera NVDEC (`nvh264dec` lub `nvh265dec`) —
/// konkretny dobierany jest w runtime wg kodeka strumienia. Gdy któregokolwiek
/// brak (inna platforma, niepełny build nvcodec) zwracamy `false` i ingest
/// idzie zawsze działającą ścieżką CPU (decodebin → videoconvert).
pub fn gpu_resident_available() -> bool {
    // `detect_hw_decoder` gwarantuje `gst::init` (idempotentne) — wołamy je
    // PRZED `element_present`, bo `ElementFactory::find` panikuje na
    // niezainicjalizowanym GStreamerze. Gdy to nie NVIDIA, krótkie spięcie:
    // nie odpytujemy elementów CUDA w ogóle.
    let decoder = detect_hw_decoder();
    if decoder != HwDecoder::Nvidia {
        return false;
    }
    let has_decoder = element_present("nvh264dec") || element_present("nvh265dec");
    gpu_resident_path_for(
        decoder,
        has_decoder,
        element_present("cudaconvert"),
        element_present("cudadownload"),
    )
}

/// Czy dostępna jest pośrednia ścieżka NVDEC + konwersja kolorów na CPU dla
/// NVIDIA: dekoder NVDEC (`nvh264dec`/`nvh265dec`) i `cudadownload` są obecne,
/// a wykryty dekoder to NVIDIA — BEZ wymagania `cudaconvert`/`cudascale`. Te
/// dwa elementy pojawiają się dopiero w nvcodec z GStreamer ≥1.26, więc na
/// 1.24 (obecnym na wielu instalacjach) pełny GPU-resident jest niedostępny,
/// ale sam NVDEC + `cudadownload` już są. Pozwala zbudować łańcuch
/// `nvhXdec → cudadownload → videoconvert → RGB`: DEKOD schodzi na GPU (zdejmuje
/// z CPU najdroższy koszt — programowy dekod 4K H.264), a na CPU zostaje jedynie
/// konwersja kolorów NV12→RGB. To wariant pośredni między pełnym GPU-resident
/// (`gpu_resident_available`) a czystym CPU (decodebin + software). Wymaga
/// zainicjalizowanego GStreamera — `detect_hw_decoder` gwarantuje idempotentny
/// `gst::init` przed `element_present`.
pub fn nvdec_decode_available() -> bool {
    let decoder = detect_hw_decoder();
    if decoder != HwDecoder::Nvidia {
        return false;
    }
    let has_decoder = element_present("nvh264dec") || element_present("nvh265dec");
    nvdec_decode_path_for(decoder, has_decoder, element_present("cudadownload"))
}

/// Czysta reguła wyboru ścieżki NVDEC + CPU-convert — wydzielona z
/// `nvdec_decode_available`, by testy mogły sprawdzić logikę bez realnego
/// sprzętu ani rejestru GStreamera. Wymaga jednocześnie: dekodera NVIDIA,
/// obecnego NVDEC i `cudadownload`. W przeciwieństwie do `gpu_resident_path_for`
/// NIE wymaga `cudaconvert` — konwersja kolorów schodzi na CPU (videoconvert).
fn nvdec_decode_path_for(decoder: HwDecoder, has_nvdec: bool, has_cudadownload: bool) -> bool {
    decoder == HwDecoder::Nvidia && has_nvdec && has_cudadownload
}

/// Czy runtime ma komplet elementów CUDA potrzebnych do GPU-owego skalowania
/// klatki detekcji (`cudaupload → cudaconvert → cudascale → cudadownload`).
/// Niezależne od ścieżki dekodowania: skalowanie 4K→560 na GPU ma sens także
/// przy dekodzie CPU/MJPEG (usuwa ~4 ms resize'u pełnej klatki na CPU). Brak
/// któregokolwiek elementu → `false` i gałąź detekcji nie jest dobudowywana
/// (detektor resize'uje na CPU jak dotąd). Wymaga zainicjalizowanego GStreamera
/// — wołamy `detect_hw_decoder` (idempotentny `gst::init`) dla bezpieczeństwa
/// `ElementFactory::find` na innych platformach niż NVIDIA.
pub fn cuda_scale_available() -> bool {
    let _ = detect_hw_decoder();
    element_present("cudaupload")
        && element_present("cudaconvert")
        && element_present("cudascale")
        && element_present("cudadownload")
}

/// Czysta reguła wyboru ścieżki GPU-resident — wydzielona z
/// `gpu_resident_available`, by testy mogły sprawdzić logikę bez realnego
/// sprzętu ani rejestru GStreamera. GPU-resident wymaga jednocześnie: dekodera
/// NVIDIA, obecnego NVDEC, `cudaconvert` i `cudadownload`. Brak któregokolwiek
/// → `false` (ingest idzie ścieżką CPU).
fn gpu_resident_path_for(
    decoder: HwDecoder,
    has_nvdec: bool,
    has_cudaconvert: bool,
    has_cudadownload: bool,
) -> bool {
    decoder == HwDecoder::Nvidia && has_nvdec && has_cudaconvert && has_cudadownload
}

/// Kolejność prób detekcji rodzin HW. Per platforma zawężamy listę przez
/// `cfg`, ale sama detekcja i tak opiera się na `ElementFactory::find`, więc
/// jest odporna na nietypowe konfiguracje (np. NVIDIA na Linuksie z VA-API
/// jako drugą kartą). Pierwsza obecna rodzina wygrywa.
fn detection_order() -> &'static [HwDecoder] {
    #[cfg(target_os = "macos")]
    {
        &[HwDecoder::VideoToolbox]
    }
    #[cfg(target_os = "ios")]
    {
        &[HwDecoder::VideoToolbox]
    }
    #[cfg(target_os = "android")]
    {
        &[HwDecoder::MediaCodec]
    }
    #[cfg(target_os = "windows")]
    {
        // NVIDIA nvcodec działa też na Windows; sprawdzamy go przed D3D11.
        &[HwDecoder::Nvidia, HwDecoder::D3d11]
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android",
        target_os = "windows"
    )))]
    {
        // Linux i pozostałe: NVIDIA (nvcodec/Jetson) przed VA-API (Intel/AMD).
        &[HwDecoder::Nvidia, HwDecoder::Vaapi]
    }
}

/// Wykonuje właściwą detekcję (bez cache). Zakłada, że GStreamer jest już
/// zainicjalizowany — w przeciwnym razie rejestr pluginów jest pusty i wynik
/// zdegraduje do `Software`. `detect_hw_decoder` (cache) gwarantuje init.
fn probe_hw_decoder() -> HwDecoder {
    for &family in detection_order() {
        if family_present(family) {
            tracing::info!(
                decoder = family.label(),
                "camera: wykryto sprzętowy dekoder wideo"
            );
            return family;
        }
    }
    tracing::info!("camera: brak sprzętowego dekodera — używam dekodowania programowego (CPU)");
    HwDecoder::Software
}

/// Wykryty dekoder cache'owany na cały proces. Probing elementów jest tani,
/// ale rejestr pluginów GStreamera nie zmienia się w trakcie życia procesu,
/// więc nie ma sensu sprawdzać go przy każdej kamerze.
static CACHED_DECODER: OnceLock<HwDecoder> = OnceLock::new();

/// Zwraca wykryty (i zcache'owany) sprzętowy dekoder wideo. Inicjalizuje
/// GStreamer przy pierwszym wywołaniu, aby rejestr pluginów był wypełniony —
/// `gst::init` jest idempotentne. Nigdy nie panikuje: gdy init zawiedzie,
/// degradujemy do `Software`.
pub fn detect_hw_decoder() -> HwDecoder {
    *CACHED_DECODER.get_or_init(|| {
        if let Err(e) = gst::init() {
            tracing::warn!(
                error = %e,
                "camera: gst::init nie powiódł się przy detekcji dekodera — fallback Software"
            );
            return HwDecoder::Software;
        }
        probe_hw_decoder()
    })
}

/// Heurystyka liczby kamer na podstawie dekodera i liczby rdzeni CPU.
/// Dekoder sprzętowy odciąża CPU, więc utrzymamy więcej strumieni; dekodowanie
/// programowe 1080p H.264 kosztuje ~kilka % rdzenia na kamerę, więc skalujemy
/// ostrożnie i trzymamy minimum 1, by „zawsze działało" nawet na słabym sprzęcie.
fn suggest_max_cameras(decoder: HwDecoder, cpus: usize) -> u32 {
    let cpus = cpus.max(1) as u32;
    if decoder.is_hardware() {
        // HW dekoder: wąskim gardłem jest przepustowość dekodera/IO, nie CPU.
        // ~4 kamery na rdzeń, sufit 64 by nie obiecywać absurdów.
        (cpus.saturating_mul(4)).clamp(2, 64)
    } else {
        // Software: ~1 kamera na rdzeń, ale zostaw rdzeń na resztę systemu.
        cpus.saturating_sub(1).clamp(1, 16)
    }
}

/// Buduje profil sprzętowy automatycznie z wykrytego dekodera i `num_cpus`.
/// Pure poza odczytem cache'owanego dekodera i liczby rdzeni — łatwe do
/// testowania przez `profile_from` poniżej.
pub fn detect_profile() -> HardwareProfile {
    let decoder = detect_hw_decoder();
    profile_from(decoder, num_cpus::get())
}

/// Czysta funkcja budująca profil — wydzielona, by testy mogły wstrzyknąć
/// dowolny dekoder i liczbę rdzeni bez realnego sprzętu.
fn profile_from(decoder: HwDecoder, cpus: usize) -> HardwareProfile {
    HardwareProfile {
        hw_decoder: decoder,
        max_cameras: suggest_max_cameras(decoder, cpus),
        prefer_hw: decoder.is_hardware(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Na maszynie bez HW dekodera detekcja MUSI zwrócić Software i nie
    /// panikować. `detect_hw_decoder` sam inicjalizuje GStreamer, więc test
    /// nie potrzebuje realnej kamery ani ręcznego `gst::init`.
    #[test]
    fn detect_does_not_panic_and_returns_some_variant() {
        let d = detect_hw_decoder();
        // Każdy wariant jest poprawny — zależy od maszyny CI. Kluczowe: brak
        // paniki i deterministyczny wynik (cache) przy powtórnym wywołaniu.
        assert_eq!(d, detect_hw_decoder(), "wynik detekcji musi być cache'owany");
    }

    #[test]
    fn family_present_software_is_false() {
        // Rodzina Software nie ma kandydatów — nigdy nie jest „obecna" jako HW.
        assert!(!family_present(HwDecoder::Software));
    }

    #[test]
    fn software_profile_keeps_at_least_one_camera() {
        // Nawet na 1-rdzeniowym, słabym sprzęcie profil Software gwarantuje
        // co najmniej jedną kamerę — wymóg „musi działać na każdym sprzęcie".
        let p = profile_from(HwDecoder::Software, 1);
        assert_eq!(p.hw_decoder, HwDecoder::Software);
        assert!(!p.prefer_hw);
        assert!(p.max_cameras >= 1);
    }

    #[test]
    fn hardware_profile_scales_up_and_prefers_hw() {
        let p = profile_from(HwDecoder::Vaapi, 8);
        assert!(p.prefer_hw);
        assert!(p.max_cameras > profile_from(HwDecoder::Software, 8).max_cameras);
        assert!(p.max_cameras <= 64);
    }

    #[test]
    fn max_cameras_bounds_are_clamped() {
        // Absurdalnie dużo rdzeni nie może przekroczyć sufitów.
        assert!(suggest_max_cameras(HwDecoder::Nvidia, 1024) <= 64);
        assert!(suggest_max_cameras(HwDecoder::Software, 1024) <= 16);
        // Zero rdzeni (teoretycznie) nadal daje ≥1.
        assert!(suggest_max_cameras(HwDecoder::Software, 0) >= 1);
    }

    #[test]
    fn gpu_resident_requires_nvidia_and_full_cuda_chain() {
        // NVIDIA z kompletnym łańcuchem CUDA → ścieżka GPU-resident.
        assert!(gpu_resident_path_for(HwDecoder::Nvidia, true, true, true));
        // Software nigdy nie wybiera GPU-resident, choćby elementy były obecne.
        assert!(!gpu_resident_path_for(HwDecoder::Software, true, true, true));
        // Inna rodzina HW (np. VA-API) też nie — ścieżka jest tylko NVIDIA.
        assert!(!gpu_resident_path_for(HwDecoder::Vaapi, true, true, true));
        // NVIDIA, ale brakuje któregoś ogniwa łańcucha CUDA → fallback CPU.
        assert!(!gpu_resident_path_for(HwDecoder::Nvidia, false, true, true));
        assert!(!gpu_resident_path_for(HwDecoder::Nvidia, true, false, true));
        assert!(!gpu_resident_path_for(HwDecoder::Nvidia, true, true, false));
    }

    #[test]
    fn nvdec_decode_requires_nvidia_nvdec_and_cudadownload_only() {
        // NVIDIA + NVDEC + cudadownload → ścieżka NVDEC + CPU-convert. Nie
        // wymaga cudaconvert/cudascale (kontrast z gpu_resident_path_for).
        assert!(nvdec_decode_path_for(HwDecoder::Nvidia, true, true));
        // Software / inna rodzina HW nigdy nie wybiera NVDEC — jest tylko NVIDIA.
        assert!(!nvdec_decode_path_for(HwDecoder::Software, true, true));
        assert!(!nvdec_decode_path_for(HwDecoder::Vaapi, true, true));
        // NVIDIA, ale brak NVDEC albo cudadownload → fallback CPU.
        assert!(!nvdec_decode_path_for(HwDecoder::Nvidia, false, true));
        assert!(!nvdec_decode_path_for(HwDecoder::Nvidia, true, false));
    }

    #[test]
    fn labels_are_stable_and_distinct() {
        let all = [
            HwDecoder::Nvidia,
            HwDecoder::VideoToolbox,
            HwDecoder::D3d11,
            HwDecoder::Vaapi,
            HwDecoder::MediaCodec,
            HwDecoder::Software,
        ];
        let mut labels: Vec<&str> = all.iter().map(|d| d.label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), all.len(), "etykiety muszą być unikalne");
    }
}
