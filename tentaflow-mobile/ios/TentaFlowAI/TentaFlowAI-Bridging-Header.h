// =============================================================================
// Plik: TentaFlowAI-Bridging-Header.h
// Opis: Deklaracje FFI dla komunikacji Swift <-> Rust w aplikacji iOS.
// =============================================================================

#ifndef TentaFlowAI_Bridging_Header_h
#define TentaFlowAI_Bridging_Header_h

#include <stdbool.h>

// Rust FFI entry points — cykl zycia aplikacji
void tentaflow_mobile_start(void);
void tentaflow_on_pause(void);
void tentaflow_on_resume(void);
void tentaflow_on_memory_warning(void);

// =============================================================================
// LAN discovery — Swift NWBrowser -> Rust iroh mesh
// =============================================================================

// Przekazuje peera znalezionego przez systemowy Bonjour (NWBrowser) do iroh.
// endpoint_id: z-base32 lowercase (52 znaki, format iroh mDNS instance name)
//              albo hex (64 znaki) Ed25519 public key
// ip: string IPv4/IPv6 (bez portu)
// port: port QUIC peera (iroh defaultowo przypisuje dynamicznie)
// Zwraca true jesli zlecono laczenie; false gdy mesh jeszcze nie gotowy
// albo argumenty niepoprawne.
_Bool tentaflow_mobile_add_discovered_peer(const char* endpoint_id,
                                           const char* ip,
                                           unsigned short port);

// =============================================================================
// Czujniki pozycjonowania — Swift CoreMotion/CoreLocation/ARKit -> Rust fuzja
// =============================================================================

// Wpycha jedną KANONICZNĄ próbkę czujnika (little-endian, layout z
// tentaflow-sdk-spec) do hostowej kolejki; addon `phone` opróżnia ją do silnika
// fuzji (ESKF) + wspólnej mapy. kind: 1=IMU, 2=GNSS, 3=BARO, 4=DEPTH(LidarFrame).
_Bool tentaflow_mobile_push_sensor(int kind, const unsigned char* ptr, int len);

// Czyści bufor czujników (rozłączenie / pauza).
void tentaflow_mobile_clear_sensors(void);

// Wpycha jedną jednostkę dostępu H.264 (Annex-B) z natywnego enkodera kamery do
// zarejestrowanej kamery push (ten sam potok co każda kamera).
_Bool tentaflow_mobile_push_camera_h264(const unsigned char* ptr, int len);

// =============================================================================
// Swift MLX bridge — typy callbackow i rejestracja
// =============================================================================

// Callback wolany przez Swift dla kazdego wygenerowanego tokena
typedef void (*tentaflow_token_callback_t)(const char* token_text, _Bool is_final, void* callback_context);

// Callback: zaladuj model z podanej sciezki. Zwraca 0=OK, <0=blad
typedef int (*tentaflow_load_model_fn_t)(const char* model_path, void* context);

// Callback: wyladuj model
typedef void (*tentaflow_unload_model_fn_t)(void* context);

// Callback: generuj tekst z tokenami streamowanymi przez token_callback
typedef int (*tentaflow_generate_fn_t)(
    const char* prompt,
    int max_tokens,
    float temperature,
    float top_p,
    int max_context_tokens,
    int memory_budget_mb,
    tentaflow_token_callback_t token_callback,
    void* callback_context,
    void* context
);

// Callback: pobierz info o modelu jako JSON C string (caller musi zwolnic przez free())
typedef char* (*tentaflow_model_info_fn_t)(void* context);

// Rejestracja callbackow MLX — wywolywane z Swift przy starcie aplikacji
void tentaflow_register_mlx_swift(
    tentaflow_load_model_fn_t load_fn,
    tentaflow_unload_model_fn_t unload_fn,
    tentaflow_generate_fn_t generate_fn,
    tentaflow_model_info_fn_t model_info_fn,
    void* context
);

// =============================================================================
// Apple TTS bridge — AVSpeechSynthesizer (AppleTTSEngine.swift). Na iOS nie ma
// libMLXBridge.dylib, wiec Rust dostaje wskazniki przez rejestracje.
// =============================================================================

// Lista glosow jako JSON C-string (caller zwalnia przez free()).
typedef char* (*apple_tts_list_voices_fn_t)(void);

// Synteza -> malloc'd bufor Float32. out_sample_rate / out_num_samples
// wypelniane przez callee. Zwraca NULL przy bledzie.
typedef float* (*apple_tts_synthesize_fn_t)(
    const char* text,
    const char* voice_id,
    const char* language,
    float rate,
    int* out_sample_rate,
    int* out_num_samples
);

// Zwalnia bufor zwrocony przez apple_tts_synthesize_fn_t.
typedef void (*apple_tts_free_buffer_fn_t)(float* ptr);

// Rejestracja callbackow Apple TTS — wywolywane z Swift przy starcie aplikacji.
void tentaflow_register_apple_tts(
    apple_tts_list_voices_fn_t list_voices,
    apple_tts_synthesize_fn_t synthesize,
    apple_tts_free_buffer_fn_t free_buffer
);

// =============================================================================
// Kokoro MLX bridge — KokoroSwiftLocal (mlalma) przez KokoroBridge package.
// Na iOS brak libKokoroBridge.dylib; Swift rejestruje wskazniki + context.
// =============================================================================

// Zaladuj model z katalogu. Zwraca 0=OK, <0=blad.
typedef int (*kokoro_load_model_fn_t)(const char* model_path, void* context);

// Wyladuj model.
typedef void (*kokoro_unload_model_fn_t)(void* context);

// Synteza -> malloc'd bufor Float32 (24 kHz). out_* wypelniane przez callee.
typedef float* (*kokoro_synthesize_fn_t)(
    const char* text,
    const char* voice,
    const char* language,
    float speed,
    int* out_sample_rate,
    int* out_num_samples,
    void* context
);

// Zwalnia bufor zwrocony przez kokoro_synthesize_fn_t.
typedef void (*kokoro_free_buffer_fn_t)(float* ptr);

// Rejestracja callbackow Kokoro MLX — wywolywane z Swift przy starcie aplikacji.
void tentaflow_register_kokoro(
    kokoro_load_model_fn_t load_fn,
    kokoro_unload_model_fn_t unload_fn,
    kokoro_synthesize_fn_t synthesize_fn,
    kokoro_free_buffer_fn_t free_buffer_fn,
    void* context
);

// =============================================================================
// MLX Whisper bridge — WhisperEngine.swift (MLX). Na iOS brak libMLXBridge.dylib;
// Swift rejestruje wskazniki + context (MLXWhisperEngine.shared) przy starcie.
// =============================================================================

// Zaladuj model z katalogu. Zwraca 0=OK, <0=blad.
typedef int (*mlx_whisper_load_model_fn_t)(const char* model_path, void* context);

// Wyladuj model.
typedef void (*mlx_whisper_unload_model_fn_t)(void* context);

// Transkrypcja PCM Float32 mono 16 kHz -> strdup'd UTF-8 string (Rust zwalnia
// przez libc free). NULL przy bledzie.
typedef char* (*mlx_whisper_transcribe_fn_t)(
    const float* pcm,
    int n_samples,
    const char* language,
    void* context
);

// Rejestracja callbackow MLX Whisper — wywolywane z Swift przy starcie aplikacji.
void tentaflow_register_whisper(
    mlx_whisper_load_model_fn_t load_fn,
    mlx_whisper_unload_model_fn_t unload_fn,
    mlx_whisper_transcribe_fn_t transcribe_fn,
    void* context
);

#endif
