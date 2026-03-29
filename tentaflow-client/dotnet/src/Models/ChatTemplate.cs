// ============================================================================
// CHAT TEMPLATE - Predefiniowane formaty promptów dla modeli LLM
// ============================================================================
//
// CEL:
// Określa format chat template używany przy formatowaniu wiadomości do promptu.
// Pozwala wybrać predefiniowany template lub użyć Auto (serwer formatuje).
//
// UŻYCIE:
// var result = client.ChatCompletion("bielik-1-5b", messages, 0.7f, 256, ChatTemplate.Llama3);
//
// ============================================================================

namespace TentaFlow.Client.Models;

/// <summary>
/// Predefiniowane chat templates dla modeli LLM.
/// </summary>
public enum ChatTemplate
{
    /// <summary>
    /// Automatyczny - serwer użyje template z modelu (vLLM tokenizer).
    /// </summary>
    Auto = 0,

    /// <summary>
    /// Llama 3 Instruct format (dla Bielik, Llama 3, Llama 3.1).
    /// Format: &lt;|start_header_id|&gt;role&lt;|end_header_id|&gt;content&lt;|eot_id|&gt;
    /// </summary>
    Llama3 = 1,

    /// <summary>
    /// ChatML format (dla Qwen, OpenChat, niektóre fine-tuned modele).
    /// Format: &lt;|im_start|&gt;role\ncontent&lt;|im_end|&gt;
    /// </summary>
    ChatML = 2,

    /// <summary>
    /// Alpaca format (dla Stanford Alpaca i pochodne).
    /// Format: ### Instruction:\n### Input:\n### Response:
    /// </summary>
    Alpaca = 3,

    /// <summary>
    /// Vicuna format (dla Vicuna, FastChat modele).
    /// Format: SYSTEM: content\nUSER: content\nASSISTANT:
    /// </summary>
    Vicuna = 4,

    /// <summary>
    /// Mistral Instruct format.
    /// Format: &lt;s&gt;[INST] content [/INST]
    /// </summary>
    Mistral = 5,
}
