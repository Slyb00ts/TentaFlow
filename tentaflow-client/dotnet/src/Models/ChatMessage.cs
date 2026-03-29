// ============================================================================
// CHAT MESSAGE - Wiadomość w konwersacji chat
// ============================================================================
//
// CEL:
// Reprezentuje pojedynczą wiadomość w konwersacji z modelem LLM.
// Używana w metodach ChatCompletion i ChatCompletionStream.
//
// ROLE:
// - "system": Instrukcje dla modelu (zachowanie, format odpowiedzi)
// - "user": Wiadomość użytkownika (pytanie, polecenie)
// - "assistant": Poprzednia odpowiedź modelu (kontekst)
//
// ============================================================================

namespace TentaFlow.Client.Models;

/// <summary>
/// Wiadomość w konwersacji chat.
/// </summary>
public sealed class ChatMessage
{
    /// <summary>
    /// Rola wiadomości: "system", "user" lub "assistant".
    /// </summary>
    public required string Role { get; init; }

    /// <summary>
    /// Treść wiadomości.
    /// </summary>
    public required string Content { get; init; }

    /// <summary>
    /// Tworzy wiadomość systemową.
    /// </summary>
    public static ChatMessage System(string content) => new() { Role = "system", Content = content };

    /// <summary>
    /// Tworzy wiadomość użytkownika.
    /// </summary>
    public static ChatMessage User(string content) => new() { Role = "user", Content = content };

    /// <summary>
    /// Tworzy wiadomość asystenta.
    /// </summary>
    public static ChatMessage Assistant(string content) => new() { Role = "assistant", Content = content };
}
