// ============================================================================
// CLIENT CONFIG - Konfiguracja klienta TentaFlow
// ============================================================================
//
// CEL:
// Konfiguracja połączenia QUIC z one-way TLS do TentaFlow.Router.
// Klient NIE wysyła certyfikatu - tylko weryfikuje serwer.
//
// OPCJONALNY CERTYFIKAT CA:
// - CaPath: Certyfikat CA (PEM) - do weryfikacji serwera (opcjonalnie)
//           Jeśli nie podany, używa systemowych certyfikatów CA.
//
// ============================================================================

namespace TentaFlow.Client.Models;

/// <summary>
/// Konfiguracja klienta TentaFlow (one-way TLS).
/// </summary>
public sealed class ClientConfig
{
    /// <summary>
    /// URL Router (np. "quic://localhost:4000").
    /// </summary>
    public required string RouterUrl { get; init; }

    /// <summary>
    /// Ścieżka do certyfikatu CA (.pem) - opcjonalne.
    /// Jeśli nie podane, używa systemowych certyfikatów CA.
    /// </summary>
    public string? CaPath { get; init; }

    /// <summary>
    /// Timeout połączenia w milisekundach (domyślnie: 30000).
    /// </summary>
    public uint TimeoutMs { get; init; } = 30000;
}
