using TentaFlow.Client;

namespace TentaFlow.Client.Test.Tests;

public static class IngestionTests
{
    /// <summary>
    /// Test Ingestion - indeksowanie tekstu.
    /// </summary>
    public static void RunText(TentaFlowClient client)
    {
        Console.WriteLine("\n─── TEST: Ingestion - Text ───");

        var docId = $"test-doc-{Guid.NewGuid():N}";
        var text = @"
    TentaFlow.AI to zaawansowana platforma sztucznej inteligencji.
    Wykorzystuje modele językowe do przetwarzania dokumentów.
    System obsługuje wiele formatów: PDF, DOCX, XLSX, PPTX, TXT.
    RAG (Retrieval Augmented Generation) pozwala na wyszukiwanie semantyczne.
    Embeddingi są generowane przez modele typu Gemma lub BGE.
    ";

        var result = client.IngestText(
            documentId: docId,
            text: text,
            metadata: new Dictionary<string, string>
            {
                ["source"] = "test",
                ["type"] = "description"
            });

        Console.WriteLine($"✓ Dokument tekstowy zindeksowany");
        Console.WriteLine($"  Document ID: {result.DocumentId}");
        Console.WriteLine($"  Status: {result.Status}");
        Console.WriteLine($"  Chunków: {result.ChunkCount}");
        Console.WriteLine($"  Wektorów: {result.VectorCount}");
        Console.WriteLine($"  Czas: {result.TotalMs} ms");
    }

    /// <summary>
    /// Test Ingestion - indeksowanie pliku TXT.
    /// </summary>
    public static void RunFileTxt(TentaFlowClient client, string exampleFilesDir)
    {
        Console.WriteLine("\n─── TEST: Ingestion - Plik TXT ───");

        var filePath = Path.Combine(exampleFilesDir, "maile.txt");
        if (!File.Exists(filePath))
        {
            Console.WriteLine($"  (Pominięto - brak pliku: {filePath})");
            return;
        }

        var docId = $"test-maile-{Guid.NewGuid():N}";
        var data = File.ReadAllBytes(filePath);

        var result = client.IngestFile(
            documentId: docId,
            filename: "maile.txt",
            data: data,
            metadata: new Dictionary<string, string>
            {
                ["source"] = "example_files",
                ["type"] = "emails"
            });

        Console.WriteLine($"✓ Plik TXT zindeksowany: maile.txt");
        Console.WriteLine($"  Document ID: {result.DocumentId}");
        Console.WriteLine($"  Status: {result.Status}");
        Console.WriteLine($"  Chunków: {result.ChunkCount}");
        Console.WriteLine($"  Wektorów: {result.VectorCount}");
        Console.WriteLine($"  Czas: {result.TotalMs} ms");
    }

    /// <summary>
    /// Test Ingestion - indeksowanie pliku PDF.
    /// </summary>
    public static void RunFilePdf(TentaFlowClient client, string exampleFilesDir)
    {
        Console.WriteLine("\n─── TEST: Ingestion - Plik PDF ───");

        var filePath = Path.Combine(exampleFilesDir, "pdf.pdf");
        if (!File.Exists(filePath))
        {
            Console.WriteLine($"  (Pominięto - brak pliku: {filePath})");
            return;
        }

        var docId = $"test-pdf-{Guid.NewGuid():N}";
        var data = File.ReadAllBytes(filePath);

        var result = client.IngestFile(
            documentId: docId,
            filename: "pdf.pdf",
            data: data,
            metadata: new Dictionary<string, string>
            {
                ["source"] = "example_files",
                ["type"] = "document"
            });

        Console.WriteLine($"✓ Plik PDF zindeksowany: pdf.pdf ({data.Length} bytes)");
        Console.WriteLine($"  Document ID: {result.DocumentId}");
        Console.WriteLine($"  Status: {result.Status}");
        Console.WriteLine($"  Chunków: {result.ChunkCount}");
        Console.WriteLine($"  Wektorów: {result.VectorCount}");
        Console.WriteLine($"  Czas: {result.TotalMs} ms");
    }

    /// <summary>
    /// Test Ingestion - indeksowanie pliku DOCX.
    /// </summary>
    public static void RunFileDocx(TentaFlowClient client, string exampleFilesDir)
    {
        Console.WriteLine("\n─── TEST: Ingestion - Plik DOCX ───");

        var filePath = Path.Combine(exampleFilesDir, "wymagania.docx");
        if (!File.Exists(filePath))
        {
            Console.WriteLine($"  (Pominięto - brak pliku: {filePath})");
            return;
        }

        var docId = $"test-docx-{Guid.NewGuid():N}";
        var data = File.ReadAllBytes(filePath);

        var result = client.IngestFile(
            documentId: docId,
            filename: "wymagania.docx",
            data: data,
            metadata: new Dictionary<string, string>
            {
                ["source"] = "example_files",
                ["type"] = "requirements"
            });

        Console.WriteLine($"✓ Plik DOCX zindeksowany: wymagania.docx ({data.Length} bytes)");
        Console.WriteLine($"  Document ID: {result.DocumentId}");
        Console.WriteLine($"  Status: {result.Status}");
        Console.WriteLine($"  Chunków: {result.ChunkCount}");
        Console.WriteLine($"  Wektorów: {result.VectorCount}");
        Console.WriteLine($"  Czas: {result.TotalMs} ms");
    }

    /// <summary>
    /// Znajduje katalog example_files szukając w górę drzewa katalogów.
    /// </summary>
    public static string FindExampleFilesDir()
    {
        var current = Directory.GetCurrentDirectory();
        for (int i = 0; i < 10; i++)
        {
            var candidate = Path.Combine(current, "TentaFlow.RAG", "example_files");
            if (Directory.Exists(candidate))
                return candidate;
            var parent = Directory.GetParent(current);
            if (parent == null) break;
            current = parent.FullName;
        }
        // Fallback: ścieżka względna
        return Path.Combine(Directory.GetCurrentDirectory(), "..", "TentaFlow.RAG", "example_files");
    }
}
