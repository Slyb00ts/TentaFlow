// =============================================================================
// Plik: wrapper_speculative.cpp
// Opis: Implementacja płaskiego C-ABI nad common_speculative z llama-common.
//       Mapuje płaskie parametry na common_params_speculative i opakowuje
//       pętlę begin/draft/process/accept dla strony Rusta.
// =============================================================================

#include "wrapper_speculative.h"

#include "common/common.h"
#include "common/speculative.h"

#include <cstdint>
#include <new>
#include <vector>

namespace {

// Mapuje lustro typu z C-ABI na enum biblioteki.
common_speculative_type map_type(llama_rs_speculative_type type) {
    switch (type) {
        case LLAMA_RS_SPECULATIVE_TYPE_DRAFT_SIMPLE: return COMMON_SPECULATIVE_TYPE_DRAFT_SIMPLE;
        case LLAMA_RS_SPECULATIVE_TYPE_DRAFT_EAGLE3: return COMMON_SPECULATIVE_TYPE_DRAFT_EAGLE3;
        case LLAMA_RS_SPECULATIVE_TYPE_DRAFT_MTP:    return COMMON_SPECULATIVE_TYPE_DRAFT_MTP;
        case LLAMA_RS_SPECULATIVE_TYPE_NGRAM_SIMPLE: return COMMON_SPECULATIVE_TYPE_NGRAM_SIMPLE;
        case LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MAP_K:  return COMMON_SPECULATIVE_TYPE_NGRAM_MAP_K;
        case LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MAP_K4V:return COMMON_SPECULATIVE_TYPE_NGRAM_MAP_K4V;
        case LLAMA_RS_SPECULATIVE_TYPE_NGRAM_MOD:    return COMMON_SPECULATIVE_TYPE_NGRAM_MOD;
        case LLAMA_RS_SPECULATIVE_TYPE_NGRAM_CACHE:  return COMMON_SPECULATIVE_TYPE_NGRAM_CACHE;
        case LLAMA_RS_SPECULATIVE_TYPE_NONE:
        default:                                     return COMMON_SPECULATIVE_TYPE_NONE;
    }
}

// Stan opakowania: kontekst biblioteki + bufory promptu/draftu PER sekwencja.
//
// common_speculative_draft() iteruje globalnie po WSZYSTKICH wpisach dparams
// i bezwarunkowo dereferencjuje dp.prompt oraz dp.result dla każdego seq_id
// (speculative.cpp: `auto & result = *dp.result;`). Dlatego każdy seq_id musi
// mieć trwale przypięty, niepusty wskaźnik prompt/result — nawet sekwencje,
// które w danym wywołaniu nie draftują (drafting=false). Trzymamy więc po
// jednym buforze na sekwencję i przypinamy ich adresy raz w init.
//
// Adresy elementów prompt_per_seq / draft_per_seq muszą pozostać stałe przez
// całe życie instancji, bo biblioteka przechowuje je w dparams[seq].{prompt,
// result}. Wektory są wymiarowane dokładnie raz (resize w init) i nigdy później
// nie rosną (zero push_back/insert/erase), więc realokacja nie nastąpi i adresy
// nie zostaną unieważnione. Same wektory żyją wewnątrz wrapper_state, który jest
// alokowany na stercie i stabilny aż do free.
struct wrapper_state {
    common_speculative *      spec  = nullptr;
    common_params_speculative cfg;
    uint32_t                  n_seq = 0;
    std::vector<llama_tokens> prompt_per_seq;
    std::vector<llama_tokens> draft_per_seq;
};

// Sprawdza, czy seq_id mieści się w [0, n_seq). Biblioteka woła GGML_ASSERT
// (abort) przy złym seq_id, więc walidujemy zawczasu i robimy no-op.
inline bool seq_in_range(const wrapper_state * state, llama_seq_id seq_id) {
    return seq_id >= 0 && static_cast<uint32_t>(seq_id) < state->n_seq;
}

} // namespace

extern "C" llama_rs_speculative * llama_rs_speculative_init(
    const llama_rs_speculative_params * params,
    uint32_t n_seq,
    uint32_t n_rs_seq,
    struct llama_context * ctx_tgt,
    struct llama_context * ctx_dft) {
    if (!params || n_seq == 0) {
        return nullptr;
    }

    // MTP wymaga obu kontekstów (ctx_tgt i ctx_dft). Bez ctx_dft biblioteka cicho
    // pomija implementację MTP (has_mtp w common_speculative_init wymaga
    // ctx_dft != nullptr), więc init "udałby się" bez draftowania — łamie to
    // kontrakt no-fallback. Odrzucamy taki przypadek jawnie.
    if (params->type == LLAMA_RS_SPECULATIVE_TYPE_DRAFT_MTP && (!ctx_tgt || !ctx_dft)) {
        return nullptr;
    }

    try {
        common_params_speculative cfg;
        cfg.types = { map_type(params->type) };
        if (params->n_max >= 0) {
            cfg.draft.n_max = params->n_max;
        }
        if (params->n_min >= 0) {
            cfg.draft.n_min = params->n_min;
        }
        // Liczbę snapshotów stanu rekurencyjnego (MTP) biblioteka wyprowadza
        // sama z need_n_rs_seq() na podstawie typów i draft.n_max — argument
        // n_rs_seq z C-ABI służy stronie Rusta do wymiarowania kontekstu modelu
        // docelowego i nie jest tu polem konfiguracji speculative.
        (void) n_rs_seq;

        // Konteksty muszą być ustawione PRZED common_speculative_init: dla MTP
        // biblioteka decyduje o włączeniu implementacji na podstawie
        // draft.ctx_dft != nullptr, a impl MTP odczytuje oba w konstruktorze
        // (GGML_ASSERT(ctx_tgt && ctx_dft)). Dla ngram pozostają nullptr (no-op).
        cfg.draft.ctx_tgt = ctx_tgt;
        cfg.draft.ctx_dft = ctx_dft;

        auto * state = new (std::nothrow) wrapper_state();
        if (!state) {
            return nullptr;
        }
        state->cfg   = cfg;
        state->n_seq = n_seq;

        state->spec = common_speculative_init(state->cfg, n_seq);
        if (!state->spec) {
            delete state;
            return nullptr;
        }

        // Jednorazowe wymiarowanie buforów per-seq. Po tym resize nie wolno
        // już zmieniać rozmiaru tych wektorów — patrz komentarz przy wrapper_state.
        state->prompt_per_seq.resize(n_seq);
        state->draft_per_seq.resize(n_seq);

        // Przypnij trwałe adresy buforów do KAŻDEGO wpisu dparams i ustaw stan
        // spoczynkowy (drafting=false, pusty result). Dzięki temu globalna pętla
        // common_speculative_draft() nigdy nie trafi na nullptr ani na naruszenie
        // asercji `!dp.drafting || dp.result->empty()`.
        for (uint32_t seq = 0; seq < n_seq; ++seq) {
            common_speculative_draft_params & dp =
                common_speculative_get_draft_params(state->spec, static_cast<llama_seq_id>(seq));
            dp.drafting = false;
            dp.n_max    = -1;
            dp.n_past   = 0;
            dp.id_last  = 0;
            dp.prompt   = &state->prompt_per_seq[seq];
            dp.result   = &state->draft_per_seq[seq];
        }

        return reinterpret_cast<llama_rs_speculative *>(state);
    } catch (const std::exception &) {
        return nullptr;
    }
}

extern "C" void llama_rs_speculative_free(llama_rs_speculative * spec) {
    if (!spec) {
        return;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    if (state->spec) {
        common_speculative_free(state->spec);
    }
    delete state;
}

extern "C" int32_t llama_rs_speculative_n_max(const llama_rs_speculative * spec) {
    if (!spec) {
        return 0;
    }
    const auto * state = reinterpret_cast<const wrapper_state *>(spec);
    return common_speculative_n_max(&state->cfg);
}

extern "C" void llama_rs_speculative_begin(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    const llama_token * prompt,
    size_t n_prompt) {
    if (!spec) {
        return;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    if (!seq_in_range(state, seq_id)) {
        return;
    }
    try {
        llama_tokens tokens;
        if (prompt && n_prompt > 0) {
            tokens.assign(prompt, prompt + n_prompt);
        }
        common_speculative_begin(state->spec, seq_id, tokens);
    } catch (const std::exception &) {
        // Brak kanału błędu w begin po stronie biblioteki; ignorujemy wyjątek.
    }
}

extern "C" bool llama_rs_speculative_process(
    llama_rs_speculative * spec,
    const struct llama_batch * batch) {
    if (!spec || !batch) {
        return false;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    try {
        return common_speculative_process(state->spec, *batch);
    } catch (const std::exception &) {
        return false;
    }
}

extern "C" bool llama_rs_speculative_need_embd(llama_rs_speculative * spec) {
    if (!spec) {
        return false;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    return common_speculative_need_embd(state->spec);
}

extern "C" bool llama_rs_speculative_need_embd_nextn(llama_rs_speculative * spec) {
    if (!spec) {
        return false;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    return common_speculative_need_embd_nextn(state->spec);
}

extern "C" void llama_rs_speculative_draft(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    int32_t n_max,
    llama_pos n_past,
    llama_token id_last,
    const llama_token * prompt,
    size_t n_prompt) {
    if (!spec) {
        return;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    if (!seq_in_range(state, seq_id)) {
        return;
    }
    try {
        const uint32_t seq = static_cast<uint32_t>(seq_id);

        // Aktualizujemy zawartość przypiętych buforów w miejscu (bez podmiany
        // adresów), żeby nie unieważnić wskaźników trzymanych przez dparams.
        llama_tokens & prompt_buf = state->prompt_per_seq[seq];
        prompt_buf.clear();
        if (prompt && n_prompt > 0) {
            prompt_buf.assign(prompt, prompt + n_prompt);
        }
        // result musi być pusty przed draftem (asercja w common_speculative_draft).
        state->draft_per_seq[seq].clear();

        common_speculative_draft_params & dp =
            common_speculative_get_draft_params(state->spec, seq_id);
        dp.drafting = true;
        dp.n_max    = n_max;
        dp.n_past   = n_past;
        dp.id_last  = id_last;
        dp.prompt   = &prompt_buf;
        dp.result   = &state->draft_per_seq[seq];

        common_speculative_draft(state->spec);
    } catch (const std::exception &) {
        // Po wyjątku zostaw stan spoczynkowy: pusty draft i wyłączone draftowanie
        // dla tej sekwencji, żeby kolejny globalny draft nie naruszył asercji.
        const uint32_t seq = static_cast<uint32_t>(seq_id);
        state->draft_per_seq[seq].clear();
        common_speculative_draft_params & dp =
            common_speculative_get_draft_params(state->spec, seq_id);
        dp.drafting = false;
    }
}

extern "C" size_t llama_rs_speculative_draft_result(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    llama_token * out,
    size_t out_capacity) {
    if (!spec) {
        return 0;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    if (!seq_in_range(state, seq_id)) {
        return 0;
    }
    const llama_tokens & draft = state->draft_per_seq[static_cast<uint32_t>(seq_id)];
    const size_t n = draft.size();
    if (out && out_capacity > 0) {
        const size_t to_copy = n < out_capacity ? n : out_capacity;
        for (size_t i = 0; i < to_copy; ++i) {
            out[i] = draft[i];
        }
    }
    return n;
}

extern "C" void llama_rs_speculative_accept(
    llama_rs_speculative * spec,
    llama_seq_id seq_id,
    uint16_t n_accepted) {
    if (!spec) {
        return;
    }
    auto * state = reinterpret_cast<wrapper_state *>(spec);
    if (!seq_in_range(state, seq_id)) {
        return;
    }
    try {
        common_speculative_accept(state->spec, seq_id, n_accepted);
    } catch (const std::exception &) {
        // accept nie zwraca statusu; wyjątek ignorujemy.
    }
}
