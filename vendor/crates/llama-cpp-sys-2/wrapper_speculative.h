// =============================================================================
// Plik: wrapper_speculative.h
// Opis: Płaskie C-ABI nad common_speculative z biblioteki llama-common
//       (MTP / ngram / Eagle3 speculative decoding). Operuje na nieprzezroczystym
//       wskaźniku common_speculative i nie wymaga znajomości typów C++.
//
// KONTRAKT BEZPIECZEŃSTWA (wymagany od strony wywołującej / Rusta):
//
//  (a) Walidacja seq_id: shim odrzuca seq_id spoza [0, n_seq) wczesnym no-op,
//      więc błędny indeks NIE spowoduje aborta przez GGML_ASSERT w bibliotece.
//      Jednak biblioteka używa GGML_ASSERT również w innych miejscach (np.
//      common_speculative_accept robi GGML_ASSERT(impl_last[seq_id])).
//      GGML_ASSERT woła abort() i NIE jest łapane przez wewnętrzny try/catch —
//      catch (std::exception) nie przechwytuje aborta. Strona wywołująca MUSI:
//        - wołać accept() tylko dla sekwencji, dla której draft() faktycznie
//          wygenerował draft (inaczej impl_last[seq_id] == nullptr → abort),
//        - nie wołać draft() dla tej samej sekwencji po raz drugi bez odczytania
//          (draft_result) i akceptacji (accept) poprzedniego draftu.
//
//  (b) Brak thread-safety: jedna instancja llama_rs_speculative NIE jest
//      thread-safe. common_speculative_draft() działa GLOBALNIE — iteruje po
//      wszystkich seq_id i draftuje dla każdego, który ma drafting=true. Stan
//      dparams jest współdzielony w obrębie instancji. Każdy uchwyt wymaga
//      zewnętrznej synchronizacji: jeden wątek na instancję (np. wątek-scheduler
//      continuous batchingu) albo opakowanie w Mutex po stronie Rusta. Wiele
//      sekwencji (n_seq > 1) obsługuje się sekwencyjnie z TEGO SAMEGO wątku.
// =============================================================================
#pragma once

#include "llama.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// Lustro common_speculative_type z common.h dla strony Rusta.
typedef enum llama_rs_speculative_type {
    LLAMA_RS_SPECULATIVE_TYPE_NONE = 0,
    LLAMA_RS_SPECULATIVE_TYPE_DRAFT_SIMPLE = 1,
    LLAMA_RS_SPECULATIVE_TYPE_DRAFT_EAGLE3 = 2,
    LLAMA_RS_SPECULATIVE_TYPE_DRAFT_MTP = 3,
    LLAMA_RS_SPECULATIVE_TYPE_NGRAM_SIMPLE = 4,
    LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MAP_K = 5,
    LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MAP_K4V = 6,
    LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MOD = 7,
    LLAMA_RS_SPECULATIVE_TYPE_NGRAM_CACHE = 8,
} llama_rs_speculative_type;

// Nieprzezroczysty uchwyt do common_speculative.
typedef struct llama_rs_speculative llama_rs_speculative;

// Parametry inicjalizacji jednego typu speculative decoding.
// Tylko ngram-self oraz konfiguracja draftu wymagana w Fazie 1; pełna
// konfiguracja draft-modeli (ścieżki GGUF, konteksty) dojdzie w Fazie 2.
typedef struct llama_rs_speculative_params {
    llama_rs_speculative_type type;
    int32_t n_max; // maks. liczba tokenów draftu (-1 = domyślne z biblioteki)
    int32_t n_min; // min. liczba tokenów draftu (-1 = domyślne z biblioteki)
} llama_rs_speculative_params;

// Inicjalizuje kontekst speculative dla `n_seq` sekwencji.
// `n_rs_seq` jest ZAREZERWOWANY (reserved) i ignorowany przez shim: liczbę snapshotów
// stanu rekurencyjnego MTP biblioteka wyprowadza sama z need_n_rs_seq() na podstawie
// typu i draft.n_max. Argument istnieje wyłącznie po to, by strona wywołująca pamiętała
// o wymiarowaniu kontekstu modelu docelowego (ctx_tgt) tą samą wartością; tutaj nie jest
// polem konfiguracji speculative (patrz `(void) n_rs_seq` w wrapper_speculative.cpp).
//
// `ctx_tgt` to kontekst modelu docelowego (zawsze wymagany dla typów draft-model,
// w tym MTP). `ctx_dft` to kontekst draftujący — dla MTP jest to DRUGI kontekst
// utworzony na TYM SAMYM modelu z ctx_type=LLAMA_CONTEXT_TYPE_MTP (zero duplikacji
// wag). Dla ngram oba mogą być NULL (ngram nie używa modelu draftującego).
//
// Konteksty są ustawiane w common_params_speculative.draft.{ctx_tgt,ctx_dft}
// PRZED common_speculative_init — bez tego biblioteka cicho pomija implementację
// MTP (has_mtp wymaga ctx_dft != nullptr). Konteksty muszą żyć dłużej niż zwrócony
// uchwyt (instancja przechowuje surowe wskaźniki). Strona wywołująca odpowiada za
// ich zwolnienie PO llama_rs_speculative_free.
//
// Zwraca NULL przy błędnych argumentach lub niepowodzeniu inicjalizacji.
llama_rs_speculative * llama_rs_speculative_init(
    const llama_rs_speculative_params * params,
    uint32_t n_seq,
    uint32_t n_rs_seq,
    struct llama_context * ctx_tgt,
    struct llama_context * ctx_dft);

// Zwalnia kontekst speculative. NULL jest bezpieczny.
void llama_rs_speculative_free(llama_rs_speculative * spec);

// Maks. liczba tokenów draftu wynikająca z parametrów.
int32_t llama_rs_speculative_n_max(const llama_rs_speculative * spec);

// Rozpoczyna nową generację dla danej sekwencji z promptem `prompt`/`n_prompt`.
void llama_rs_speculative_begin(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    const llama_token * prompt,
    size_t n_prompt);

// Przetwarza batch i aktualizuje wewnętrzny stan speculative.
// Zwraca true, jeśli batch został przetworzony.
bool llama_rs_speculative_process(
    llama_rs_speculative * spec,
    const struct llama_batch * batch);

// true, jeśli któraś implementacja wymaga ekstrakcji embeddingów post-norm.
bool llama_rs_speculative_need_embd(llama_rs_speculative * spec);

// true, jeśli któraś implementacja wymaga ekstrakcji embeddingów nextn (MTP).
bool llama_rs_speculative_need_embd_nextn(llama_rs_speculative * spec);

// Ustawia parametry draftu dla danej sekwencji i generuje draft.
// `n_max` może nadpisać limit (-1 = bez nadpisania). `id_last` to ostatni
// zaakceptowany token, `n_past` to bieżąca pozycja w kontekście, a
// `prompt`/`n_prompt` to dotychczasowy prompt sekwencji.
// Wygenerowane tokeny można odczytać przez llama_rs_speculative_draft_result
// (per seq_id). seq_id spoza [0, n_seq) jest bezpiecznie ignorowane (no-op).
// Uwaga: common_speculative_draft() draftuje globalnie dla wszystkich sekwencji
// z aktywnym drafting; sekwencje bez aktywnego draftu zachowują swój stan.
void llama_rs_speculative_draft(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    int32_t n_max,
    llama_pos n_past,
    llama_token id_last,
    const llama_token * prompt,
    size_t n_prompt);

// Kopiuje ostatni wygenerowany draft dla danej sekwencji do bufora `out`
// o pojemności `out_capacity` tokenów. Zwraca rzeczywistą długość draftu;
// jeśli przekracza `out_capacity`, kopiuje tylko `out_capacity` tokenów,
// a zwrócona wartość pozwala wykryć obcięcie. `out` może być NULL przy
// `out_capacity == 0` (zapytanie tylko o długość). Draft jest buforowany
// per seq_id; seq_id spoza [0, n_seq) zwraca 0 (no-op).
size_t llama_rs_speculative_draft_result(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    llama_token * out,
    size_t out_capacity);

// Informuje kontekst speculative o liczbie tokenów zaakceptowanych przez model
// docelowy dla danej sekwencji. seq_id spoza [0, n_seq) jest ignorowane (no-op).
// UWAGA: wolno wołać wyłącznie dla sekwencji, dla której draft() wygenerował
// draft — inaczej biblioteka robi GGML_ASSERT(impl_last[seq_id]) → abort
// (patrz kontrakt bezpieczeństwa na górze pliku).
void llama_rs_speculative_accept(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    uint16_t n_accepted);

#ifdef __cplusplus
}
#endif
