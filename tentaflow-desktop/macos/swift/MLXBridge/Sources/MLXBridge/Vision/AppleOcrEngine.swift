// =============================================================================
// Plik: AppleOcrEngine.swift
// Opis: Wrapper na `VNRecognizeTextRequest` (Vision framework) — natywny OCR
//       Apple, dziala na macOS/iOS bez zaleznosci i bez modelu na dysku.
//
//       Przyjmuje bajty obrazu (PNG/JPG/dowolny format ktory `CGImageSource`
//       potrafi zdekodowac), uruchamia rozpoznawanie tekstu i zwraca JSON z
//       polaczonym tekstem oraz per-linia bounding boxami i pewnoscia. Wybor
//       jezykow (`recognitionLanguages`) i korekta jezykowa
//       (`usesLanguageCorrection`) sa konfigurowalne przez parametry FFI.
//
//       Cdecl `MLXAppleOCR_*` jest niezalezny od MLXBridge LLM/Whisper/TTS —
//       Vision dziala bez MLX, ale dzielimy ten sam dylib zeby Rust mial jeden
//       punkt dlopen.
// =============================================================================

import Foundation
import Vision

#if canImport(CoreGraphics)
import CoreGraphics
import ImageIO
#endif

/// Jedna rozpoznana linia: tekst, pewnosc (0..1) i bbox w znormalizowanych
/// wspolrzednych Vision (origin lewy-dolny, [0..1] x/y/width/height).
public struct AppleOCRLine {
    public let text: String
    public let confidence: Float
    public let x: Float
    public let y: Float
    public let width: Float
    public let height: Float
}

/// Wynik OCR: polaczony tekst (linie zlaczone `\n`) + lista linii z metadanymi.
public struct AppleOCRResult {
    public let text: String
    public let lines: [AppleOCRLine]
}

public enum AppleOCREngine {
    /// Dekoduje bajty obrazu do `CGImage`. `CGImageSource` ogarnia PNG/JPG/HEIC
    /// i inne formaty rejestrowane w ImageIO bez recznego parsowania naglowkow.
    private static func decodeImage(_ bytes: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(bytes as CFData, nil) else {
            return nil
        }
        return CGImageSourceCreateImageAtIndex(source, 0, nil)
    }

    /// Synchroniczne rozpoznawanie tekstu uzywane przez cdecl FFI.
    /// `languages` — lista kodow jezykow (np. `["pl-PL", "en-US"]`); pusta lista
    /// zostawia domyslne jezyki Vision. `useLanguageCorrection` wlacza slownikowa
    /// korekte (lepsza dla prozy, gorsza dla kodow/tablic).
    ///
    /// `VNImageRequestHandler.perform` jest synchroniczne — wynik czytamy z
    /// requestu po jego zakonczeniu, bez run-loopa i bez Taska (caller to Rust
    /// `spawn_blocking`, wiec blokowanie watku jest pozadane).
    public static func recognizeSync(
        imageBytes: Data,
        languages: [String],
        useLanguageCorrection: Bool
    ) -> AppleOCRResult? {
        guard let cgImage = decodeImage(imageBytes) else {
            return nil
        }

        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.usesLanguageCorrection = useLanguageCorrection
        if !languages.isEmpty {
            request.recognitionLanguages = languages
        }

        let handler = VNImageRequestHandler(cgImage: cgImage, options: [:])
        do {
            try handler.perform([request])
        } catch {
            print("[AppleOCR] perform blad: \(error)")
            return nil
        }

        guard let observations = request.results else {
            return AppleOCRResult(text: "", lines: [])
        }

        var lines: [AppleOCRLine] = []
        var joined: [String] = []
        for obs in observations {
            // `topCandidates(1)` zwraca najpewniejszy odczyt danej linii.
            guard let candidate = obs.topCandidates(1).first else { continue }
            let box = obs.boundingBox
            lines.append(AppleOCRLine(
                text: candidate.string,
                confidence: candidate.confidence,
                x: Float(box.origin.x),
                y: Float(box.origin.y),
                width: Float(box.size.width),
                height: Float(box.size.height)
            ))
            joined.append(candidate.string)
        }

        return AppleOCRResult(text: joined.joined(separator: "\n"), lines: lines)
    }

    /// Serializuje wynik do JSON: `{ "text": "...", "lines": [ { text,
    /// confidence, x, y, width, height } ] }`. Rust czyta tylko `text`, ale
    /// boxy/pewnosc sa dostepne dla bogatszych konsumentow (np. layout flow).
    public static func resultToJson(_ result: AppleOCRResult) -> String? {
        let linesJson: [[String: Any]] = result.lines.map { line in
            [
                "text": line.text,
                "confidence": line.confidence,
                "x": line.x,
                "y": line.y,
                "width": line.width,
                "height": line.height,
            ]
        }
        let obj: [String: Any] = [
            "text": result.text,
            "lines": linesJson,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: obj),
              let s = String(data: data, encoding: .utf8) else { return nil }
        return s
    }
}

// =============================================================================
// C-ABI exports — niezalezne od MLX bridge.
// =============================================================================

/// Rozpoznaje tekst z bajtow obrazu (PNG/JPG/...). Zwraca alokowany przez
/// `strdup` JSON string (`{text, lines:[...]}`); caller zwalnia go przez
/// `MLXAppleOCR_freeString`. `langs` to przecinkami rozdzielona lista kodow
/// jezykow (np. "pl-PL,en-US"); pusty/NULL = domyslne jezyki Vision.
/// `useLanguageCorrection != 0` wlacza slownikowa korekte. `outLen` (gdy != NULL)
/// dostaje dlugosc JSON w bajtach UTF-8 bez NUL.
@_cdecl("MLXAppleOCR_recognize")
public func MLXAppleOCR_recognize(
    bytes: UnsafePointer<UInt8>?,
    len: Int32,
    langs: UnsafePointer<CChar>?,
    useLanguageCorrection: Int32,
    outLen: UnsafeMutablePointer<Int32>?
) -> UnsafeMutablePointer<CChar>? {
    guard let bytes, len > 0 else { return nil }
    let data = Data(bytes: bytes, count: Int(len))

    // "pl-PL,en-US" -> ["pl-PL", "en-US"]; puste fragmenty odrzucone.
    let languages: [String] = langs
        .map { String(cString: $0) }?
        .split(separator: ",")
        .map { $0.trimmingCharacters(in: .whitespaces) }
        .filter { !$0.isEmpty }
        ?? []

    let result = AppleOCREngine.recognizeSync(
        imageBytes: data,
        languages: languages,
        useLanguageCorrection: useLanguageCorrection != 0
    )
    guard let r = result, let json = AppleOCREngine.resultToJson(r) else { return nil }

    if let outLen {
        outLen.pointee = Int32(json.utf8.count)
    }
    // strdup() alokuje przez malloc — Rust zwolni przez `MLXAppleOCR_freeString`.
    return strdup(json)
}

/// Zwalnia string zwrocony przez `MLXAppleOCR_recognize`. Bezpieczny dla NULL.
/// `strdup` alokuje przez malloc, wiec zwalniamy przez `free`.
@_cdecl("MLXAppleOCR_freeString")
public func MLXAppleOCR_freeString(ptr: UnsafeMutablePointer<CChar>?) {
    guard let ptr else { return }
    free(ptr)
}
