// =============================================================================
// Plik: whisper.rs
// Opis: Typy i punkty wejścia wrappera TentaFlow dla whisper.cpp.
// Przykład: let config = WhisperTranscribeConfig::default();
// =============================================================================

#[cfg(feature = "whisper")]
use std::ffi::{CStr, CString};
#[cfg(feature = "whisper")]
use std::os::raw::c_char;
#[cfg(feature = "whisper")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(feature = "whisper")]
use std::ptr;

use serde::{Deserialize, Serialize};

use crate::native::{NativeError, NativeLayout};

#[cfg(feature = "whisper")]
pub use whisper_rs_sys as sys;

pub const ENGINE_ID: &str = "whisper-cpp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperArtifacts {
    pub include_dir: PathBuf,
    pub static_dir: PathBuf,
    pub dynamic_dir: PathBuf,
}

impl WhisperArtifacts {
    pub fn discover() -> Result<Self, NativeError> {
        Self::from_layout(&NativeLayout::discover()?, WhisperVariant::Multi)
    }

    pub fn from_layout(
        layout: &NativeLayout,
        variant: WhisperVariant,
    ) -> Result<Self, NativeError> {
        let include_dir = layout.include_dir().join("whisper");
        layout.require_file(include_dir.join("whisper.h"))?;
        layout.require_file(include_dir.join("ggml.h"))?;

        let static_dir = layout
            .static_dir()
            .join("whisper-cpp")
            .join(variant.as_dir_name());
        layout.require_file(static_dir.join(static_library_name("whisper")))?;

        Ok(Self {
            include_dir,
            static_dir,
            dynamic_dir: layout
                .dynamic_dir()
                .join("whisper-cpp")
                .join(variant.as_dir_name()),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WhisperVariant {
    Multi,
    Cpu,
    Cuda,
    Vulkan,
    Rocm,
    Metal,
}

impl WhisperVariant {
    pub fn as_dir_name(self) -> &'static str {
        match self {
            Self::Multi => "multi",
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Rocm => "rocm",
            Self::Metal => "metal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperLoadConfig {
    pub use_gpu: bool,
    pub flash_attn: bool,
    pub gpu_device: i32,
}

impl Default for WhisperLoadConfig {
    fn default() -> Self {
        Self {
            use_gpu: true,
            flash_attn: false,
            gpu_device: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WhisperTranscribeConfig {
    pub language: Option<String>,
    pub translate: bool,
    pub temperature: f32,
    pub max_segment_len: Option<u32>,
    pub word_timestamps: bool,
    pub initial_prompt: Option<String>,
    pub no_speech_threshold: f32,
    pub n_threads: i32,
    pub beam_size: i32,
}

impl Default for WhisperTranscribeConfig {
    fn default() -> Self {
        Self {
            language: None,
            translate: false,
            temperature: 0.0,
            max_segment_len: None,
            word_timestamps: false,
            initial_prompt: None,
            no_speech_threshold: 0.6,
            n_threads: std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(4),
            beam_size: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperTranscribeOutput {
    pub text: String,
    pub duration_seconds: f64,
    pub segments: Vec<WhisperSegment>,
    /// ISO-639-1 code of the language whisper actually used (explicit or
    /// auto-detected), from `whisper_full_lang_id`. `None` when unknown.
    pub detected_language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperSegment {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub no_speech_prob: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum WhisperError {
    #[error("model path is not valid UTF-8: {0}")]
    InvalidPath(PathBuf),
    #[error("failed to load whisper model: {0}")]
    LoadFailed(PathBuf),
    #[error("audio sample count exceeds whisper.cpp API limit: {0}")]
    AudioTooLong(usize),
    #[error("whisper transcription failed with code {0}")]
    TranscribeFailed(i32),
    #[error("whisper.cpp returned invalid segment count: {0}")]
    InvalidSegmentCount(i32),
}

#[cfg(feature = "whisper")]
pub struct WhisperRuntime {
    ctx: WhisperContext,
}

#[cfg(feature = "whisper")]
unsafe impl Send for WhisperRuntime {}

#[cfg(feature = "whisper")]
impl WhisperRuntime {
    pub fn load(
        model_path: impl AsRef<Path>,
        config: WhisperLoadConfig,
    ) -> Result<Self, WhisperError> {
        let model_path = model_path.as_ref();
        let path = path_to_cstring(model_path)?;
        let mut params = unsafe { sys::whisper_context_default_params() };
        params.use_gpu = config.use_gpu;
        params.flash_attn = config.flash_attn;
        params.gpu_device = config.gpu_device;

        let ctx = unsafe { sys::whisper_init_from_file_with_params(path.as_ptr(), params) };
        if ctx.is_null() {
            return Err(WhisperError::LoadFailed(model_path.to_path_buf()));
        }

        Ok(Self {
            ctx: WhisperContext { ptr: ctx },
        })
    }

    pub fn transcribe(
        &self,
        config: &WhisperTranscribeConfig,
        pcm: &[f32],
    ) -> Result<WhisperTranscribeOutput, WhisperError> {
        let n_samples =
            i32::try_from(pcm.len()).map_err(|_| WhisperError::AudioTooLong(pcm.len()))?;
        let mut params = build_full_params(config);
        let language = config
            .language
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| WhisperError::InvalidPath(PathBuf::from("language")))?;
        let initial_prompt = config
            .initial_prompt
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| WhisperError::InvalidPath(PathBuf::from("initial_prompt")))?;

        params.language = language
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr());
        // NULL language already means "detect, then transcribe" inside
        // whisper_full. `detect_language = true` would make whisper.cpp STOP
        // after the detection pass and return zero segments.
        params.detect_language = false;
        params.initial_prompt = initial_prompt
            .as_ref()
            .map_or(ptr::null(), |value| value.as_ptr());

        let code = unsafe {
            sys::whisper_full(
                self.ctx.ptr,
                params,
                if pcm.is_empty() {
                    ptr::null()
                } else {
                    pcm.as_ptr()
                },
                n_samples,
            )
        };
        if code != 0 {
            return Err(WhisperError::TranscribeFailed(code));
        }

        let n_segments = unsafe { sys::whisper_full_n_segments(self.ctx.ptr) };
        if n_segments < 0 {
            return Err(WhisperError::InvalidSegmentCount(n_segments));
        }

        let mut segments = Vec::with_capacity(n_segments as usize);
        let mut full_text = String::new();

        for i in 0..n_segments {
            let text_ptr = unsafe { sys::whisper_full_get_segment_text(self.ctx.ptr, i) };
            let text = cstr_to_string(text_ptr);
            let trimmed = text.trim();
            if !full_text.is_empty() && !trimmed.is_empty() {
                full_text.push(' ');
            }
            full_text.push_str(trimmed);

            let start = unsafe { sys::whisper_full_get_segment_t0(self.ctx.ptr, i) } as f64 * 0.01;
            let end = unsafe { sys::whisper_full_get_segment_t1(self.ctx.ptr, i) } as f64 * 0.01;
            let no_speech_prob =
                unsafe { sys::whisper_full_get_segment_no_speech_prob(self.ctx.ptr, i) };

            segments.push(WhisperSegment {
                id: i as u32,
                start,
                end,
                text: trimmed.to_string(),
                no_speech_prob,
            });
        }

        // Language whisper actually decoded with — explicit or auto-detected
        // (whisper.cpp sets `state->lang_id` on both paths).
        let detected_language = {
            let lang_id = unsafe { sys::whisper_full_lang_id(self.ctx.ptr) };
            if lang_id >= 0 {
                let lang = cstr_to_string(unsafe { sys::whisper_lang_str(lang_id) });
                if lang.is_empty() { None } else { Some(lang) }
            } else {
                None
            }
        };

        Ok(WhisperTranscribeOutput {
            text: full_text,
            duration_seconds: pcm.len() as f64 / 16000.0,
            segments,
            detected_language,
        })
    }
}

#[cfg(feature = "whisper")]
struct WhisperContext {
    ptr: *mut sys::whisper_context,
}

#[cfg(feature = "whisper")]
impl Drop for WhisperContext {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { sys::whisper_free(self.ptr) };
            self.ptr = ptr::null_mut();
        }
    }
}

#[cfg(feature = "whisper")]
fn build_full_params(config: &WhisperTranscribeConfig) -> sys::whisper_full_params {
    let beam_size = config.beam_size.max(1);
    let strategy = if beam_size > 1 {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_BEAM_SEARCH
    } else {
        sys::whisper_sampling_strategy_WHISPER_SAMPLING_GREEDY
    };
    let mut params = unsafe { sys::whisper_full_default_params(strategy) };
    params.n_threads = config.n_threads.max(1);
    params.translate = config.translate;
    params.print_special = false;
    params.print_progress = false;
    params.print_realtime = false;
    params.print_timestamps = false;
    params.token_timestamps = config.word_timestamps;
    params.temperature = config.temperature;
    params.no_speech_thold = config.no_speech_threshold;
    if let Some(max_len) = config.max_segment_len {
        params.max_len = i32::try_from(max_len).unwrap_or(i32::MAX);
    }
    params.greedy.best_of = 1;
    params.beam_search.beam_size = beam_size;
    params.beam_search.patience = 1.0;
    params
}

#[cfg(feature = "whisper")]
fn path_to_cstring(path: &Path) -> Result<CString, WhisperError> {
    let value = path
        .to_str()
        .ok_or_else(|| WhisperError::InvalidPath(path.to_path_buf()))?;
    CString::new(value).map_err(|_| WhisperError::InvalidPath(path.to_path_buf()))
}

#[cfg(feature = "whisper")]
fn cstr_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn static_library_name(name: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{name}.lib")
    } else {
        format!("lib{name}.a")
    }
}
