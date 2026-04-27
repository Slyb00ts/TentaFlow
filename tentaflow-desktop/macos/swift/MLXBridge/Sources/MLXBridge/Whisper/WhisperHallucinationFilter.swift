// =============================================================================
// Plik: WhisperHallucinationFilter.swift
// Opis: Blacklista znanych Whisper hallucinations. Whisper byl trenowany
//       na podcastach z YT i na ciszy/szumie tla generuje wyuczone outro
//       z wysoka pewnoscia (low noSpeechProb, dobry avgLogprob), wiec
//       progi probabilistyczne ich nie lapia. Lista zebrana z openai/whisper
//       issue tracker + observacji w produkcji.
//
//       Strategia: porownujemy znormalizowany input (lowercase, bez interpunkcji,
//       trimmed) z lista znanych fraz. Jesli match jest exact lub prefix —
//       caly segment uznajemy za halucynacje i zwracamy "".
//       NIE porownujemy `contains` zeby nie wycinac prawdziwej mowy ktora
//       *zawiera* wzorzec (np. ktos faktycznie powiedzial "dziekuje bardzo").
// =============================================================================

import Foundation

public enum WhisperHallucinationFilter {
    /// Lista znormalizowanych fraz halucynacji. Dodawaj nowe gdy zaobserwujesz
    /// w produkcji — Whisper ma bias do konkretnego subsetu z treningu.
    private static let knownHallucinations: Set<String> = [
        // PL — outra polskich podcastow / napisy z YT
        "dziekuje bardzo",
        "dziekuje za uwage",
        "dziekuje za obejrzenie",
        "napisy stworzone przez spolecznosc amaraorg",
        "napisy stworzone przez spolecznosc amara org",
        "napisy stworzone przez spoleczenstwo amaraorg",
        "napisy stworzone przez spoleczenstwo amara org",
        "napisy zrobione przez spolecznosc amaraorg",
        "napisy autorzy",
        "subtitles by the amaraorg community",
        "subtitles by the amara org community",
        "do zobaczenia",
        "do zobaczenia w nastepnym odcinku",
        "do zobaczenia nastepnym razem",
        "zapraszam do subskrypcji",

        // EN — outra YT/podcastow
        "thank you",
        "thank you very much",
        "thank you for watching",
        "thanks for watching",
        "subscribe to my channel",
        "please subscribe",
        "like and subscribe",
        "see you next time",
        "see you in the next video",
        "bye",
        "goodbye",

        // Inne — instrumental / muzyka detekcja Whispera (klasyczne false positives)
        "music",
        "applause",
        "laughter",
        "muzyka",
        "smiech",
        "brawa",
    ]

    /// Zwraca true gdy `text` jest jedna z typowych halucynacji Whispera.
    /// Normalizacja: lowercase, usuwa interpunkcje, multi-spaces -> single space, trim.
    public static func isHallucination(_ text: String) -> Bool {
        let normalized = normalize(text)
        if normalized.isEmpty { return false }
        return knownHallucinations.contains(normalized)
    }

    private static func normalize(_ text: String) -> String {
        var s = text.lowercased()
        // Diakrytyki -> ASCII (zeby "dziękuję" matchowalo "dziekuje")
        if let folded = s.applyingTransform(.stripDiacritics, reverse: false) {
            s = folded
        }
        // Usun interpunkcje + cyfry (poza spacja). Whisper czesto dodaje
        // koncowa kropke albo emotki — chcemy te same fingerprinty.
        let allowed = CharacterSet.lowercaseLetters.union(.whitespaces)
        s = String(s.unicodeScalars.filter { allowed.contains($0) })
        // Multi-space -> single
        let parts = s.split(whereSeparator: { $0.isWhitespace })
        return parts.joined(separator: " ").trimmingCharacters(in: .whitespaces)
    }
}
