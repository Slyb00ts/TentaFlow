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

#endif
