// =============================================================================
// Plik: WhisperTokenizer.swift
// Opis: Wrapper na `swift-transformers` AutoTokenizer dostosowany do
//       konwencji specjalnych tokenow Whispera. Whisper trzyma w slowniku
//       tokeny sterujace dekoderem:
//         - <|startoftranscript|>     -> 50258 (large-v3)
//         - <|<lang_code>|>           -> 50259..50358 (np. <|en|>=50259, <|pl|>=50293)
//         - <|transcribe|>            -> 50360
//         - <|translate|>             -> 50359
//         - <|notimestamps|>          -> 50364
//         - <|nospeech|>              -> 50363
//         - <|endoftext|>             -> 50257
//         - <|0.00|>..<|30.00|>       -> 50365..51865 (1500 tokenow timestampow co 20ms)
//
//       Konkretne ID-y wczytujemy z tokenizera (lookup po stringach), zeby
//       byc niezalezni od wersji modelu.
// =============================================================================

import Foundation
import Tokenizers

public enum WhisperTokenizerError: Error, CustomStringConvertible {
    case missingTokenizerJson(URL)
    case missingSpecialToken(String)

    public var description: String {
        switch self {
        case .missingTokenizerJson(let url):
            return "Brak tokenizer.json w \(url.path)"
        case .missingSpecialToken(let token):
            return "Brak specjalnego tokena '\(token)' w tokenizerze"
        }
    }
}

/// Lekki wrapper trzymajacy wczesniej zlokalizowane ID specjalnych tokenow,
/// zeby decoder loop nie placil za string lookup w kazdej iteracji.
public final class WhisperTokenizer {
    public let inner: any Tokenizer

    public let sotToken: Int
    public let eotToken: Int
    public let transcribeToken: Int
    public let translateToken: Int
    public let noTimestampsToken: Int
    public let noSpeechToken: Int
    /// Pierwszy token timestampa <|0.00|>; kolejne to +1, +2, ... az do
    /// `timestampBase + 1500` (`<|30.00|>`). Whisper sprawdza `tokenId >= timestampBase`
    /// zeby wykryc czy ostatni token to znacznik czasu.
    public let timestampBase: Int
    /// Mapa "<|en|>" / "<|pl|>" / ... -> token ID. Klucze trzymamy bez prefixu
    /// "<|" i suffixu "|>", zeby caller mogl podac po prostu "en" / "pl".
    public let languageTokens: [String: Int]

    public init(folder: URL) async throws {
        let tokenizerJson = folder.appendingPathComponent("tokenizer.json")
        guard FileManager.default.fileExists(atPath: tokenizerJson.path) else {
            throw WhisperTokenizerError.missingTokenizerJson(tokenizerJson)
        }
        // AutoTokenizer.from(modelFolder:) wczytuje tokenizer.json + opcjonalny
        // tokenizer_config.json. Wewnetrznie buduje BPE z merges + dodaje
        // wszystkie added_tokens.json jako specjalne.
        let tk = try await AutoTokenizer.from(modelFolder: folder)

        func mustFind(_ token: String) throws -> Int {
            let ids = tk.encode(text: token, addSpecialTokens: false)
            // Specjalny token musi byc dokladnie jeden w wyniku — gdy tokenizer
            // potraktowal go jako zwykly tekst, dostalibysmy kilka tokenow BPE.
            guard ids.count == 1 else {
                throw WhisperTokenizerError.missingSpecialToken(token)
            }
            return ids[0]
        }

        let sot = try mustFind("<|startoftranscript|>")
        let eot = try mustFind("<|endoftext|>")
        let transcribe = try mustFind("<|transcribe|>")
        let translate = try mustFind("<|translate|>")
        let noTs = try mustFind("<|notimestamps|>")
        let noSp = try mustFind("<|nospeech|>")
        let tsBase = try mustFind("<|0.00|>")

        // Whisper wspiera 99 jezykow; budujemy mape leniwie — sprawdzamy
        // tylko te ktore sa w tokenizerze (gdy nie ma, pomijamy bez bledu).
        var langs: [String: Int] = [:]
        let codes = [
            "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr",
            "pl", "ca", "nl", "ar", "sv", "it", "id", "hi", "fi", "vi",
            "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no",
            "th", "ur", "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk",
            "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk",
            "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw",
            "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc",
            "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
            "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl",
            "mg", "as", "tt", "haw", "ln", "ha", "ba", "jw", "su", "yue",
        ]
        for code in codes {
            let ids = tk.encode(text: "<|\(code)|>", addSpecialTokens: false)
            if ids.count == 1 {
                langs[code] = ids[0]
            }
        }

        self.inner = tk
        self.sotToken = sot
        self.eotToken = eot
        self.transcribeToken = transcribe
        self.translateToken = translate
        self.noTimestampsToken = noTs
        self.noSpeechToken = noSp
        self.timestampBase = tsBase
        self.languageTokens = langs
    }

    /// Dekoduje liste token ID na tekst, pomijajac specjalne tokeny
    /// (timestampy + sterujace) — to jest wartosc czytelna dla uzytkownika.
    /// `swift-transformers` nie traktuje timestamp tokenow jako "specjalne"
    /// (dla niego to po prostu added_tokens), wiec filtrujemy je sami.
    public func decode(tokens: [Int]) -> String {
        let filtered = tokens.filter { $0 < timestampBase }
        return inner.decode(tokens: filtered, skipSpecialTokens: true)
    }

    /// Dekoduje surowo, zachowujac specjalne tokeny — uzywane do debugowania
    /// stanu dekodera (gdy potrzeba zobaczyc znaczniki <|0.00|>).
    public func decodeRaw(tokens: [Int]) -> String {
        return inner.decode(tokens: tokens, skipSpecialTokens: false)
    }
}
