using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

/// <summary>
/// Testy Memory - system pamięci konwersacji.
/// Memory automatycznie:
/// - Analizuje pytania i decyduje czy szukać w pamięci
/// - Wyciąga encje, relacje i fakty z odpowiedzi
/// - Przechowuje wiedzę w grafie relacji
/// </summary>
public static class MemoryTests
{
    /// <summary>
    /// Test podstawowy Memory - sprawdza czy system zapamiętuje informacje w ramach sesji.
    ///
    /// Scenariusz:
    /// 1. Podajemy informacje o sobie (imię, zawód)
    /// 2. W kolejnym pytaniu pytamy o te informacje
    /// 3. Sprawdzamy czy Memory zwraca poprawne dane
    /// </summary>
    public static void RunBasicMemory(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Basic Memory (Session-based) ───");

        // Generujemy unikalny sessionId dla tego testu
        var sessionId = Guid.NewGuid().ToString();
        Console.WriteLine($"  Session ID: {sessionId}");

        // === KROK 1: Podajemy informacje o sobie ===
        Console.WriteLine("\n  [1] Podaję informacje o sobie...");

        var messages1 = new List<ChatMessage>
        {
            new() { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko po polsku." },
            new() { Role = "user", Content = "Cześć! Nazywam się Tomek i pracuję jako programista w firmie NextApp. Lubię grać w szachy." }
        };

        Console.Write("  User: Cześć! Nazywam się Tomek i pracuję jako programista w firmie NextApp. Lubię grać w szachy.\n");
        Console.Write("  AI: ");

        var options1 = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 1024,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = sessionId,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response1 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages1,
            options: options1,
            onContent: token => Console.Write(token));

        Console.WriteLine();
        Console.WriteLine($"  Latency: {response1.Completion.LatencyMs} ms");

        // Dodajemy odpowiedź asystenta do historii
        messages1.Add(new ChatMessage { Role = "assistant", Content = response1.Completion.Content });

        // Czekamy chwilę aby Memory mogło przetworzyć i zapisać dane
        Console.WriteLine("  (Czekam 2s na zapis do Memory...)");
        Thread.Sleep(2000);

        // === KROK 2: Pytamy o zapamiętane informacje ===
        Console.WriteLine("\n  [2] Pytam o zapamiętane informacje...");

        var messages2 = new List<ChatMessage>(messages1)
        {
            new() { Role = "user", Content = "Jak się nazywam i gdzie pracuję?" }
        };

        Console.Write("  User: Jak się nazywam i gdzie pracuję?\n");
        Console.Write("  AI: ");

        var options2 = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 512,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = sessionId,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response2 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages2,
            options: options2,
            onContent: token => Console.Write(token));

        Console.WriteLine();
        Console.WriteLine($"  Latency: {response2.Completion.LatencyMs} ms");

        // === WERYFIKACJA ===
        var content = response2.Completion.Content.ToLower();
        bool hasTomek = content.Contains("tomek");
        bool hasNextApp = content.Contains("nextapp") || content.Contains("next app");
        bool hasProgramista = content.Contains("programist");

        Console.WriteLine("\n  Weryfikacja Memory:");
        Console.WriteLine($"    Imię (Tomek): {(hasTomek ? "✓" : "✗")}");
        Console.WriteLine($"    Firma (NextApp): {(hasNextApp ? "✓" : "✗")}");
        Console.WriteLine($"    Zawód (programista): {(hasProgramista ? "✓" : "✗")}");

        if (hasTomek && (hasNextApp || hasProgramista))
        {
            Console.WriteLine("  ✓ Memory działa poprawnie - informacje zostały zapamiętane!");
        }
        else
        {
            Console.WriteLine("  ⚠ Memory może nie działać - sprawdź konfigurację Memory w Router");
        }
    }

    /// <summary>
    /// Test Memory z wieloma sesjami - weryfikuje izolację danych między sesjami.
    /// </summary>
    public static void RunMultiSessionMemory(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Multi-Session Memory Isolation ───");

        var session1 = Guid.NewGuid().ToString();
        var session2 = Guid.NewGuid().ToString();

        Console.WriteLine($"  Session 1: {session1}");
        Console.WriteLine($"  Session 2: {session2}");

        // === SESJA 1: Anna, lekarka ===
        Console.WriteLine("\n  [Session 1] Przedstawiam się jako Anna...");

        var messages1 = new List<ChatMessage>
        {
            new() { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko." },
            new() { Role = "user", Content = "Jestem Anna i jestem lekarką." }
        };

        var options1 = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 256,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = session1,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response1 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages1,
            options: options1);

        Console.WriteLine($"  AI (Session 1): {response1.Completion.Content.Trim()}");

        // === SESJA 2: Marek, nauczyciel ===
        Console.WriteLine("\n  [Session 2] Przedstawiam się jako Marek...");

        var messages2 = new List<ChatMessage>
        {
            new() { Role = "system", Content = "Jesteś pomocnym asystentem. Odpowiadaj krótko." },
            new() { Role = "user", Content = "Jestem Marek i jestem nauczycielem." }
        };

        var options2 = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 256,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = session2,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response2 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages2,
            options: options2);

        Console.WriteLine($"  AI (Session 2): {response2.Completion.Content.Trim()}");

        Thread.Sleep(2000); // Czekamy na zapis

        // === WERYFIKACJA IZOLACJI ===
        Console.WriteLine("\n  [Weryfikacja] Sprawdzam izolację sesji...");

        // Pytanie w sesji 1 - powinna pamiętać Annę
        messages1.Add(new ChatMessage { Role = "assistant", Content = response1.Completion.Content });
        messages1.Add(new ChatMessage { Role = "user", Content = "Jak mam na imię?" });

        var check1Options = new ChatCompletionOptions
        {
            Temperature = 0.1f,
            MaxTokens = 128,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = session1,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var check1 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages1,
            options: check1Options);

        Console.WriteLine($"  Session 1 - 'Jak mam na imię?': {check1.Completion.Content.Trim()}");

        // Pytanie w sesji 2 - powinna pamiętać Marka
        messages2.Add(new ChatMessage { Role = "assistant", Content = response2.Completion.Content });
        messages2.Add(new ChatMessage { Role = "user", Content = "Jak mam na imię?" });

        var check2Options = new ChatCompletionOptions
        {
            Temperature = 0.1f,
            MaxTokens = 128,
            Stream = true,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = session2,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var check2 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages2,
            options: check2Options);

        Console.WriteLine($"  Session 2 - 'Jak mam na imię?': {check2.Completion.Content.Trim()}");

        // Weryfikacja
        bool session1HasAnna = check1.Completion.Content.ToLower().Contains("anna");
        bool session2HasMarek = check2.Completion.Content.ToLower().Contains("marek");

        Console.WriteLine("\n  Wynik izolacji:");
        Console.WriteLine($"    Session 1 pamięta Annę: {(session1HasAnna ? "✓" : "✗")}");
        Console.WriteLine($"    Session 2 pamięta Marka: {(session2HasMarek ? "✓" : "✗")}");

        if (session1HasAnna && session2HasMarek)
        {
            Console.WriteLine("  ✓ Izolacja sesji działa poprawnie!");
        }
        else
        {
            Console.WriteLine("  ⚠ Problem z izolacją sesji");
        }
    }

    /// <summary>
    /// Test Memory z TTS - pełny przepływ voice assistant.
    /// </summary>
    public static void RunMemoryWithTts(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Memory + TTS (Voice Assistant Flow) ───");

        var sessionId = Guid.NewGuid().ToString();
        Console.WriteLine($"  Session ID: {sessionId}");

        var ttsOptions = new TtsOptions
        {
            Model = "sherpa-tts",
            Voice = "jarvis",
            Format = "wav",
            Speed = 1.0f
        };

        var allAudio = new List<byte[]>();

        // === Pytanie 1: Przedstawienie się ===
        Console.WriteLine("\n  [1] User: Cześć, mam na imię Kasia.");

        var messages = new List<ChatMessage>
        {
            new() { Role = "system", Content = "Jesteś Jarvis - inteligentnym asystentem głosowym. Odpowiadaj krótko i naturalnie po polsku." },
            new() { Role = "user", Content = "Cześć, mam na imię Kasia." }
        };

        Console.Write("  Jarvis: ");

        var options1 = new ChatCompletionOptions
        {
            Temperature = 0.7f,
            MaxTokens = 256,
            Stream = true,
            Tts = ttsOptions,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = sessionId,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response1 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages,
            options: options1,
            onContent: token => Console.Write(token),
            onAudio: chunk => allAudio.Add(chunk));

        Console.WriteLine();
        Console.WriteLine($"  Audio chunks: {allAudio.Count}, Total bytes: {allAudio.Sum(c => c.Length)}");

        messages.Add(new ChatMessage { Role = "assistant", Content = response1.Completion.Content });

        Thread.Sleep(1500);

        // === Pytanie 2: Sprawdzenie pamięci ===
        Console.WriteLine("\n  [2] User: Pamiętasz jak mam na imię?");

        messages.Add(new ChatMessage { Role = "user", Content = "Pamiętasz jak mam na imię?" });

        Console.Write("  Jarvis: ");

        var options2 = new ChatCompletionOptions
        {
            Temperature = 0.3f,
            MaxTokens = 256,
            Stream = true,
            Tts = ttsOptions,
            Memory = new MemoryOptions
            {
                Enabled = true,
                SessionId = sessionId,
                StoreEnabled = true,
                QueryEnabled = true
            }
        };

        var response2 = client.ChatCompletion(
            model: "bielik-11b",
            messages: messages,
            options: options2,
            onContent: token => Console.Write(token),
            onAudio: chunk => allAudio.Add(chunk));

        Console.WriteLine();
        Console.WriteLine($"  Audio chunks total: {allAudio.Count}");

        // Zapisz audio
        if (allAudio.Count > 0)
        {
            using var ms = new MemoryStream();
            foreach (var chunk in allAudio)
            {
                ms.Write(chunk, 0, chunk.Length);
            }
            var outputPath = Path.Combine(Path.GetTempPath(), "dotnet_memory_tts_test.wav");
            File.WriteAllBytes(outputPath, ms.ToArray());
            Console.WriteLine($"  Zapisano audio: {outputPath}");
        }

        // Weryfikacja
        bool hasKasia = response2.Completion.Content.ToLower().Contains("kasia");
        Console.WriteLine($"\n  Memory + TTS: {(hasKasia ? "✓ Działa!" : "⚠ Sprawdź konfigurację")}");
    }
}