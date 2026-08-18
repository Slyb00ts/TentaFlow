// =============================================================================
// File: modules/flows-builder/model-modalities.js
// Description: What each model can actually take in and give back, so the
//              canvas can dim the ports a chosen model cannot serve.
//
//              A port's TYPE never changes — an `image` port carries images
//              whatever model is selected, and the backend validates edges
//              against that. What changes with the model is whether the port
//              can be USED, and that is a UI affordance: dimming it stops
//              someone wiring a picture into a text-only model and waiting for
//              a run to explain why.
//
//              The source is the unified catalog (`CatalogEntryWire`), which
//              already carries `input_modalities` / `output_modalities` per
//              model. Nothing here infers capability from a model's NAME:
//              "vision" in a repo id is marketing, the catalog is the record.
// Example:
//   await ModelModalities.load();
//   ModelModalities.accepts('qwen3-vl', 'image');   // -> true | false | null
//   ModelModalities.reasoningLevels('gpt-5');       // -> ['low','high'] | [] | null
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

/// model id (lower-cased) -> { input: Set, output: Set }
let index = null;
let loading = null;

function normalise(id) {
  return String(id || '').trim().toLowerCase();
}

function build(entries) {
  const map = new Map();
  for (const e of entries || []) {
    const id = normalise(e.id ?? e.model ?? '');
    if (!id) continue;
    map.set(id, {
      input: new Set((e.inputModalities ?? e.input_modalities ?? []).map(normalise)),
      output: new Set((e.outputModalities ?? e.output_modalities ?? []).map(normalise)),
      // Kolejność z katalogu jest znacząca (low → high), więc lista, nie zbiór.
      reasoning: (e.reasoningLevels ?? e.reasoning_levels ?? []).map(normalise),
    });
  }
  return map;
}

export const ModelModalities = {
  /// Loads the catalog once. Concurrent callers share the same request.
  async load({ force = false } = {}) {
    if (index && !force) return index;
    if (loading && !force) return loading;
    loading = (async () => {
      try {
        const body = await ApiBinary.action('catalogListRequest', {});
        const entries = body?.entries ?? body?.models ?? [];
        index = build(entries);
      } catch (_) {
        // No catalog: every port stays enabled. Dimming is help, not a gate —
        // guessing "unsupported" from a failed request would block valid work.
        index = new Map();
      }
      return index;
    })();
    return loading;
  },

  /// `true` / `false` when the catalog knows the model, `null` when it does
  /// not — an unknown model must not be treated as incapable.
  accepts(modelId, modality) {
    const e = index?.get(normalise(modelId));
    if (!e) return null;
    // An entry that declares nothing is text-only by the resolver's own rule
    // (`satisfies` in runtime/resolver.rs), so mirror that instead of guessing.
    if (e.input.size === 0) return normalise(modality) === 'text';
    return e.input.has(normalise(modality));
  },

  emits(modelId, modality) {
    const e = index?.get(normalise(modelId));
    if (!e) return null;
    if (e.output.size === 0) return normalise(modality) === 'text';
    return e.output.has(normalise(modality));
  },

  /// Poziomy rozumowania modelu w kolejności podanej przez katalog. Pusta lista =
  /// model NIE wspiera sterowania rozumowaniem, więc formularz nie ma czego
  /// pokazać. `null` = model nieznany katalogowi — tak jak przy modalnościach nie
  /// wyciągamy z tego wniosku, że czegoś nie potrafi.
  reasoningLevels(modelId) {
    const e = index?.get(normalise(modelId));
    if (!e) return null;
    return e.reasoning.slice();
  },

  known(modelId) {
    return Boolean(index?.get(normalise(modelId)));
  },
};
