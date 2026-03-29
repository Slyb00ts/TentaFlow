// ============================================================================
// TENTAFLOW EXCEPTION - Wyjątki operacji TentaFlow
// ============================================================================
//
// CEL:
// Dedykowany typ wyjątku dla błędów operacji TentaFlow.
// Pozwala na selektywne przechwytywanie błędów z biblioteki.
//
// TYPOWE PRZYCZYNY:
// - Błąd połączenia z Router (timeout, certyfikaty)
// - Błąd modelu (model niedostępny, błąd inference)
// - Błąd RAG (brak wyników, błąd indeksowania)
// - Błąd argumentów (null, puste kolekcje)
//
// ============================================================================

namespace TentaFlow.Client;

/// <summary>
/// Wyjątek rzucany gdy operacja TentaFlow się nie powiedzie.
/// </summary>
public class TentaFlowException : Exception
{
    /// <summary>
    /// Tworzy nowy TentaFlowException z podaną wiadomością.
    /// </summary>
    public TentaFlowException(string message) : base(message)
    {
    }

    /// <summary>
    /// Tworzy nowy TentaFlowException z podaną wiadomością i wewnętrznym wyjątkiem.
    /// </summary>
    public TentaFlowException(string message, Exception innerException) : base(message, innerException)
    {
    }
}
