// =============================================================================
// Plik: tts/clean_cache.rs
// Opis: Cache regul czyszczenia TTS — laduje aktywne reguly z tabeli
//       `tts_cleaning_rules` raz przy pierwszym uzyciu, potem trzyma w pamieci
//       wraz z pre-skompilowanymi regexami. Refresh wywoluje sie po kazdym
//       CRUD na regulach (create/update/delete) zeby zmiana z dashboardu
//       byla widoczna od razu, bez restartu serwera.
//
//       Dodatkowa, zawsze aktywna warstwa: emoji strip — niezalezna od bazy,
//       usuwa wszystkie znaki Unicode z blokow emoji/symbol/dingbat
//       zanim aplikujemy reguly. Dzieki temu TTS nigdy nie probuje
//       wymowic emoji (TTS Sherpa/Apple/Kokoro by zwykle wstawial cisze
//       albo dziwne dzwieki dla "🎉" / "✅" itd.).
// =============================================================================

use parking_lot::RwLock;
use regex::{Regex, RegexBuilder};
use std::sync::OnceLock;

use crate::db::{models::DbTtsCleaningRule, repository, DbPool};

/// Limit pamieci dla skompilowanego regexa (chroni przed pathologicznym
/// patternem z bazy). Wartosc dopasowana do flow_engine::adapters::tts_clean.
const REGEX_SIZE_LIMIT: usize = 1_000_000;

#[derive(Debug)]
struct CompiledRule {
    rule_type: String,
    pattern: String,
    replacement: String,
    /// Pre-skompilowany regex dla rule_type = "regex_remove" / "emoji_range".
    /// None dla "abbreviation" / "phonetic" (uzywaja String::replace).
    regex: Option<Regex>,
}

static CACHE: OnceLock<RwLock<Option<Vec<CompiledRule>>>> = OnceLock::new();

fn cache() -> &'static RwLock<Option<Vec<CompiledRule>>> {
    CACHE.get_or_init(|| RwLock::new(None))
}

fn compile(rules: Vec<DbTtsCleaningRule>) -> Vec<CompiledRule> {
    rules
        .into_iter()
        .map(|r| {
            let regex = match r.rule_type.as_str() {
                "regex_remove" | "emoji_range" => RegexBuilder::new(&r.pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .build()
                    .map_err(|e| {
                        tracing::warn!(
                            rule_id = r.id,
                            pattern = %r.pattern,
                            error = %e,
                            "TTS clean cache: niepoprawny regex — regula pomijana"
                        );
                    })
                    .ok(),
                _ => None,
            };
            CompiledRule {
                rule_type: r.rule_type,
                pattern: r.pattern,
                replacement: r.replacement.unwrap_or_default(),
                regex,
            }
        })
        .collect()
}

/// Wymusza ponowne zaladowanie regul z DB. Wolaj po kazdym CRUD na
/// `tts_cleaning_rules` (Create/Update/Delete) zeby cache byl spojny z DB.
/// Bezpiecznie wywolywalna z wielu watkow — bierze write lock raz na refresh.
pub fn refresh(db: &DbPool) {
    match repository::list_tts_cleaning_rules_active(db) {
        Ok(rules) => {
            let count = rules.len();
            let compiled = compile(rules);
            *cache().write() = Some(compiled);
            tracing::debug!(rule_count = count, "TTS clean cache odswiezony");
        }
        Err(e) => {
            tracing::warn!(error = %e, "TTS clean cache: refresh failed — zostawiam stary stan");
        }
    }
}

/// Inwalidacja cache bez przeladowania. Kolejne `clean()` zaladuje swieze
/// reguly. Lzejsza alternatywa do `refresh()` gdy nie chcemy odpalac
/// query natychmiast (np. w pisanym handlerze, gdzie DbPool moze byc na
/// innym watku tokio).
pub fn invalidate() {
    *cache().write() = None;
}

/// Czysci tekst pod TTS. Operacje:
///   1. Strip emoji (Unicode pictographs/symbols/dingbats/regional/ZWJ).
///   2. Apply regul z DB w kolejnosci priority (compiled cache).
///
/// Lazy-load: pierwsze wywolanie po starcie/inwalidacji laduje reguly z DB.
/// Idempotentne: dla identycznego inputu zwraca identyczny output.
pub fn clean(text: &str, db: &DbPool) -> String {
    let mut out = strip_emoji(text);

    // Lazy load — bez tego zadne reguly nie byloby aplikowane przy pierwszym
    // strzale. Refresh ma write lock, wiec sprawdzamy stan w read lock i
    // uwalniamy przed wywolaniem refresh.
    if cache().read().is_none() {
        refresh(db);
    }

    let guard = cache().read();
    let Some(rules) = guard.as_ref() else {
        return out;
    };

    for rule in rules {
        match rule.rule_type.as_str() {
            "abbreviation" | "phonetic" => {
                out = out.replace(&rule.pattern, &rule.replacement);
            }
            "regex_remove" | "emoji_range" => {
                if let Some(ref re) = rule.regex {
                    out = re.replace_all(&out, rule.replacement.as_str()).to_string();
                }
            }
            other => {
                tracing::trace!(rule_type = other, "TTS clean: nieznany typ — pomijam");
            }
        }
    }

    out
}

/// Usuwa znaki Unicode z blokow emoji / symbol / dingbat — niezaleznie od
/// regul z bazy. To wspolczesny TTS (Sherpa, Apple AVSpeech, Kokoro) zwykle
/// nie umie z nimi nic sensownego zrobic; bez stripu wstawia cisze albo
/// rozjeżdża sie prozodyjnie.
///
/// Pokrywa:
///   - U+1F300..U+1FAFF — Misc Pictographs, Emoticons, Transport, Symbols
///   - U+2600..U+27BF   — Misc Symbols + Dingbats
///   - U+1F1E6..U+1F1FF — Regional Indicator Symbols (flagi)
///   - U+FE00..U+FE0F   — Variation Selectors (modyfikatory emoji)
///   - U+E0100..U+E01EF — Variation Selectors Supplement
///   - U+200D            — Zero Width Joiner (klejenie ZWJ-emoji sequences)
fn strip_emoji(s: &str) -> String {
    s.chars()
        .filter(|c| {
            let n = *c as u32;
            !((0x1F300..=0x1FAFF).contains(&n)
                || (0x2600..=0x27BF).contains(&n)
                || (0x1F1E6..=0x1F1FF).contains(&n)
                || (0xFE00..=0xFE0F).contains(&n)
                || (0xE0100..=0xE01EF).contains(&n)
                || n == 0x200D)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_emoji_usuwa_pictographs() {
        assert_eq!(strip_emoji("Witaj 👋 świecie 🌍!"), "Witaj  świecie !");
    }

    #[test]
    fn strip_emoji_zostawia_polskie_znaki() {
        let in_text = "Cześć, jak się masz? Żółć, ążźć.";
        assert_eq!(strip_emoji(in_text), in_text);
    }

    #[test]
    fn strip_emoji_usuwa_zwj_sequences() {
        // 👨‍👩‍👧 (family) = 👨 ZWJ 👩 ZWJ 👧 — wszystkie 5 znakow w blokach emoji/ZWJ
        let in_text = "Rodzina: 👨‍👩‍👧 dziecko";
        let out = strip_emoji(in_text);
        // Po stripie zostaje "Rodzina:  dziecko" (jednak ze spacjami w srodku)
        assert!(!out.contains('👨'));
        assert!(!out.contains('👩'));
        assert!(!out.contains('👧'));
        assert!(out.contains("dziecko"));
        assert!(out.contains("Rodzina"));
    }

    #[test]
    fn strip_emoji_dingbats_i_symbols() {
        assert_eq!(strip_emoji("OK ✓ done ✅"), "OK  done ");
    }
}
