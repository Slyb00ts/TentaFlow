# =============================================================================
# Plik: vllm-spec-prep.sh
# Opis: Wspoldzielony przez entrypointy vllm i vllm-spark. Funkcja
#       `vllm_provision_and_spec` czyta env VLLM_* i: (1) opcjonalnie kwantyzuje
#       model glowny do NVFP4 (przepisuje $MODEL na lokalna sciezke), (2) sklada
#       $SPEC_ARGS = "--speculative-config <json>" dla ngram / mtp / draft.
#       JSON jest kompaktowy (bez spacji), wiec przezywa word-splitting w
#       `vllm serve ... $VLLM_ARGS $SPEC_ARGS`.
# =============================================================================

# Ustawia globalnie: MODEL (moze przepisac) oraz SPEC_ARGS.
vllm_provision_and_spec() {
  SPEC_ARGS=""

  if [[ -n "${VLLM_MODEL_QUANTIZE:-}" ]]; then
    echo "[entrypoint] kwantyzacja modelu glownego -> $VLLM_MODEL_QUANTIZE" >&2
    MODEL="$(/app/prepare-vllm-model.sh "$MODEL" "$VLLM_MODEL_QUANTIZE")"
    echo "[entrypoint] model glowny (NVFP4): $MODEL" >&2
  fi

  local ntok="${VLLM_SPEC_NUM_TOKENS:-4}"
  case "${VLLM_SPEC_METHOD:-}" in
    ngram)
      SPEC_ARGS="--speculative-config {\"method\":\"ngram\",\"num_speculative_tokens\":${ntok},\"prompt_lookup_max\":4,\"prompt_lookup_min\":2}"
      ;;
    mtp)
      # Glowice MTP wbudowane w model — vLLM uzywa ich bez osobnego draftu.
      SPEC_ARGS="--speculative-config {\"method\":\"mtp\",\"num_speculative_tokens\":${ntok}}"
      ;;
    draft)
      local draft="${VLLM_SPEC_REPO:?VLLM_SPEC_REPO wymagane dla metody draft}"
      if [[ -n "${VLLM_SPEC_DRAFT_QUANTIZE:-}" ]]; then
        echo "[entrypoint] kwantyzacja draftu -> $VLLM_SPEC_DRAFT_QUANTIZE" >&2
        draft="$(/app/prepare-vllm-model.sh "$draft" "$VLLM_SPEC_DRAFT_QUANTIZE")"
      fi
      SPEC_ARGS="--speculative-config {\"model\":\"${draft}\",\"num_speculative_tokens\":${ntok}}"
      ;;
    "") : ;;
    *) echo "[entrypoint] nieznana VLLM_SPEC_METHOD='${VLLM_SPEC_METHOD}', pomijam spekulacje" >&2 ;;
  esac

  [[ -n "$SPEC_ARGS" ]] && echo "[entrypoint] speculative: $SPEC_ARGS" >&2
}
