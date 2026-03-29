using TentaFlow.Client;
using TentaFlow.Client.Models;

namespace TentaFlow.Client.Test.Tests;

public static class ToolsTests
{
    private static bool _toolsTestPassed = false;
    private static bool _jsonTestPassed = false;

    /// <summary>
    /// Test Bielik 1.5B - Tools/Function Calling.
    /// </summary>
    public static void RunFunctionCalling(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Bielik 1.5B - Tools/Function Calling ───");

        var systemPrompt = @"Jesteś asystentem, który używa narzędzi. Masz dostęp do następujących narzędzi:

NARZĘDZIA:
1. get_weather - Pobiera aktualną pogodę dla lokalizacji
   Parametry:
   - location (string, wymagany): Nazwa miasta
   - units (string, opcjonalny): 'celsius' lub 'fahrenheit', domyślnie 'celsius'

2. calculate - Wykonuje obliczenia matematyczne
   Parametry:
   - expression (string, wymagany): Wyrażenie matematyczne do obliczenia

Gdy użytkownik pyta o coś wymagającego narzędzia, odpowiedz TYLKO w formacie JSON:
{
  ""tool"": ""nazwa_narzędzia"",
  ""parameters"": {
    ""parametr1"": ""wartość1""
  }
}

NIE dodawaj żadnego tekstu przed ani po JSON. Odpowiedz TYLKO JSON-em.";

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = systemPrompt },
            new ChatMessage { Role = "user", Content = "Jaka jest pogoda w Warszawie?" }
        };

        Console.WriteLine("  Model: bielik-1-5b");
        Console.WriteLine("  Pytanie: Jaka jest pogoda w Warszawie?");

        var options = new ChatCompletionOptions
        {
            Temperature = 0.1f,
            MaxTokens = 1024,
            Template = ChatTemplate.Llama3,
            Stream = false
        };

        var result = client.ChatCompletion("bielik-1-5b", messages, options);
        var response = result.Completion.Content.Trim();

        Console.WriteLine($"  Odpowiedź: {response}");
        Console.WriteLine($"  Latency: {result.Completion.LatencyMs} ms");

        // Walidacja odpowiedzi
        _toolsTestPassed = ValidateToolCall(response);
    }

    /// <summary>
    /// Test Bielik 1.5B - JSON Structured Output.
    /// </summary>
    public static void RunJsonOutput(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Bielik 1.5B - JSON Structured Output ───");

        var systemPrompt = @"Jesteś asystentem, który odpowiada TYLKO w formacie JSON.
Twoja odpowiedź musi być poprawnym obiektem JSON bez żadnego dodatkowego tekstu.
NIE dodawaj żadnych wyjaśnień, komentarzy ani markdown. TYLKO czysty JSON.";

        var messages = new[]
        {
            new ChatMessage { Role = "system", Content = systemPrompt },
            new ChatMessage { Role = "user", Content = @"Podaj dane o Polsce w następującym formacie JSON:
{
  ""nazwa"": ""pełna nazwa kraju"",
  ""stolica"": ""nazwa stolicy"",
  ""waluta"": ""nazwa waluty"",
  ""populacja_mln"": liczba_populacji_w_milionach,
  ""kod_iso"": ""dwuliterowy kod ISO""
}" }
        };

        Console.WriteLine("  Model: bielik-1-5b");
        Console.WriteLine("  Pytanie: Podaj dane o Polsce jako JSON");

        var options = new ChatCompletionOptions
        {
            Temperature = 0.1f,
            MaxTokens = 256,
            Template = ChatTemplate.Llama3,
            Stream = false
        };

        var result = client.ChatCompletion("bielik-1-5b", messages, options);
        var response = result.Completion.Content.Trim();

        Console.WriteLine($"  Odpowiedź: {response}");
        Console.WriteLine($"  Latency: {result.Completion.LatencyMs} ms");

        // Walidacja odpowiedzi
        _jsonTestPassed = ValidateJsonOutput(response);
    }

    /// <summary>
    /// Wyświetla podsumowanie testów.
    /// </summary>
    public static void PrintSummary()
    {
        Console.WriteLine($"\n─── Podsumowanie testów Bielik 1.5B ───");
        Console.WriteLine($"  Tools/Function Calling: {(_toolsTestPassed ? "✓ PASSED" : "✗ FAILED")}");
        Console.WriteLine($"  JSON Structured Output: {(_jsonTestPassed ? "✓ PASSED" : "✗ FAILED")}");
    }

    private static bool ValidateToolCall(string response)
    {
        try
        {
            // Usuń markdown code blocks jeśli są
            var jsonContent = response;
            if (jsonContent.StartsWith("```"))
            {
                var lines = jsonContent.Split('\n');
                jsonContent = string.Join('\n', lines.Skip(1).TakeWhile(l => !l.StartsWith("```")));
            }

            // Wyciągnij tylko pierwszy obiekt JSON
            var firstBrace = jsonContent.IndexOf('{');
            if (firstBrace >= 0)
            {
                int braceCount = 0;
                int endIndex = -1;
                for (int i = firstBrace; i < jsonContent.Length; i++)
                {
                    if (jsonContent[i] == '{') braceCount++;
                    else if (jsonContent[i] == '}')
                    {
                        braceCount--;
                        if (braceCount == 0)
                        {
                            endIndex = i;
                            break;
                        }
                    }
                }
                if (endIndex > firstBrace)
                {
                    jsonContent = jsonContent.Substring(firstBrace, endIndex - firstBrace + 1);
                }
            }

            var json = System.Text.Json.JsonDocument.Parse(jsonContent);
            var root = json.RootElement;

            if (root.TryGetProperty("tool", out var toolProp) &&
                root.TryGetProperty("parameters", out var paramsProp))
            {
                var toolName = toolProp.GetString();
                if (toolName == "get_weather" && paramsProp.TryGetProperty("location", out var locationProp))
                {
                    var location = locationProp.GetString();
                    if (!string.IsNullOrEmpty(location) &&
                        (location.Contains("Warszaw", StringComparison.OrdinalIgnoreCase) ||
                         location.Contains("Warsaw", StringComparison.OrdinalIgnoreCase)))
                    {
                        Console.WriteLine($"✓ Test PASSED: Poprawny tool call - {toolName}(location={location})");
                        return true;
                    }
                }
            }

            Console.WriteLine("✗ Test FAILED: JSON nie zawiera poprawnego tool call");
            return false;
        }
        catch (System.Text.Json.JsonException ex)
        {
            Console.WriteLine($"✗ Test FAILED: Odpowiedź nie jest poprawnym JSON: {ex.Message}");
            return false;
        }
    }

    private static bool ValidateJsonOutput(string response)
    {
        try
        {
            // Usuń markdown code blocks jeśli są
            var jsonContent = response;
            if (jsonContent.StartsWith("```"))
            {
                var lines = jsonContent.Split('\n');
                jsonContent = string.Join('\n', lines.Skip(1).TakeWhile(l => !l.StartsWith("```")));
            }

            var json = System.Text.Json.JsonDocument.Parse(jsonContent);
            var root = json.RootElement;

            var hasNazwa = root.TryGetProperty("nazwa", out _);
            var hasStolica = root.TryGetProperty("stolica", out var stolicaProp);
            var hasWaluta = root.TryGetProperty("waluta", out _);

            if (hasNazwa && hasStolica && hasWaluta)
            {
                var stolica = stolicaProp.GetString() ?? "";
                if (stolica.Contains("Warszaw", StringComparison.OrdinalIgnoreCase))
                {
                    Console.WriteLine("✓ Test PASSED: Poprawny JSON z wymaganymi polami i prawidłową stolicą");
                    return true;
                }
                else
                {
                    Console.WriteLine($"✗ Test FAILED: Stolica nieprawidłowa: {stolica}");
                    return false;
                }
            }
            else
            {
                Console.WriteLine($"✗ Test FAILED: Brakujące pola - nazwa:{hasNazwa}, stolica:{hasStolica}, waluta:{hasWaluta}");
                return false;
            }
        }
        catch (System.Text.Json.JsonException ex)
        {
            Console.WriteLine($"✗ Test FAILED: Odpowiedź nie jest poprawnym JSON: {ex.Message}");
            return false;
        }
    }
}