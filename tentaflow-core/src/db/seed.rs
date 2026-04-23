// =============================================================================
// Plik: db/seed.rs
// Opis: Domyslne dane - uzytkownik admin, ustawienia, reguly PII, flow, prompty.
// =============================================================================

use anyhow::Result;
use rusqlite::Connection;
use tracing::{debug, info};

use crate::crypto;

/// Seeduje domyslne dane jesli baza jest pusta.
/// Caly seed w jednej transakcji (jedno fsync zamiast wielu).
pub fn seed_defaults(conn: &Connection) -> Result<()> {
    let tx = conn.unchecked_transaction()?;

    // Sprawdz czy jest juz uzytkownik
    let user_count: i64 = tx.query_row("SELECT COUNT(*) FROM users", [], |row| row.get(0))?;

    if user_count == 0 {
        let password_hash = crypto::hash_password("admin")?;
        tx.execute(
            "INSERT INTO users (username, password_hash, role, must_change_password) VALUES ('admin', ?1, 'admin', 1)",
            rusqlite::params![password_hash],
        )?;
        info!("Utworzono domyslnego uzytkownika: admin/admin");
    } else {
        migrate_sha256_passwords(&tx)?;
    }

    // Domyslne ustawienia
    let jwt_secret = generate_jwt_secret();
    let settings: &[(&str, &str)] = &[
        ("dashboard_port", "8090"),
        ("jwt_secret", &jwt_secret),
        ("jwt_expiry_hours", "24"),
        ("metrics_interval_ms", "1000"),
        ("health_check_interval_ms", "5000"),
        ("hf_token", ""),
        ("flow_engine_enabled", "false"),
        ("flow_debug_mode", "false"),
        ("flow_default_timeout_ms", "120000"),
        ("speaker_confidence_high", "0.78"),
        ("speaker_confidence_medium", "0.55"),
        ("speaker_voice_samples_required", "3"),
        ("speaker_enrollment_min_confidence", "0.7"),
        ("oauth_redirect_base_url", "https://localhost:8090"),
    ];

    {
        let mut stmt = tx.prepare("INSERT OR IGNORE INTO settings (key, value) VALUES (?1, ?2)")?;
        for (key, value) in settings {
            let affected = stmt.execute(rusqlite::params![key, value])?;
            if affected == 0 {
                debug!("Ustawienie '{}' juz istnieje, pominieto", key);
            }
        }
    }

    seed_pii_rules(&tx)?;
    seed_flow_node_templates(&tx)?;
    seed_tts_cleaning_rules(&tx)?;
    seed_fast_path_patterns(&tx)?;
    seed_prompts(&tx)?;
    seed_default_flows(&tx)?;
    seed_model_aliases(&tx)?;

    // Seed user_accounts — domyslny admin z hashem argon2
    seed_user_accounts(&tx)?;

    tx.commit()?;
    Ok(())
}

/// Seeduje konto admina w tabeli user_accounts (migracja 14+).
/// Jesli tabela nie istnieje (starsza wersja), pomija.
fn seed_user_accounts(conn: &Connection) -> Result<()> {
    // Sprawdz czy tabela user_accounts istnieje
    let table_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='user_accounts'",
        [],
        |row| row.get(0),
    )?;

    if !table_exists {
        return Ok(());
    }

    let user_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM user_accounts", [], |row| row.get(0))?;

    if user_count == 0 {
        let password_hash = crypto::hash_password("admin")?;
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash, display_name, is_admin, must_change_password) \
             VALUES (1, 'admin', ?1, 'Administrator', 1, 1)",
            rusqlite::params![password_hash],
        )?;
        // Dodaj admina do grupy admins
        conn.execute(
            "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (1, 1)",
            [],
        )?;
        info!("Utworzono domyslne konto admina w user_accounts");
    }

    Ok(())
}

/// Seeduje domyslne reguly filtrowania danych osobowych.
fn seed_pii_rules(conn: &Connection) -> Result<()> {
    let rules: &[(&str, &str, &str, &str, i64, &str)] = &[
        (
            "Imie i Nazwisko",
            "name",
            r"[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+",
            "[IMIE_NAZWISKO]",
            100,
            "Wykrywa imie i nazwisko (dwa slowa zaczynajace sie wielka litera)",
        ),
        (
            "NIP",
            "tax_id",
            r"\b\d{3}[-\s]?\d{3}[-\s]?\d{2}[-\s]?\d{2}\b",
            "[NIP]",
            90,
            "Numer Identyfikacji Podatkowej (10 cyfr)",
        ),
        (
            "PESEL",
            "national_id",
            r"\b\d{11}\b",
            "[PESEL]",
            90,
            "Numer PESEL (11 cyfr)",
        ),
        (
            "Email",
            "email",
            r"\b[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\b",
            "[EMAIL]",
            80,
            "Adres email",
        ),
        (
            "Telefon",
            "phone",
            r"(?:\+?48[\s-]?)?(?:\(?\d{2,3}\)?[\s-]?)?\d{3}[\s-]?\d{3}[\s-]?\d{2,3}\b",
            "[TELEFON]",
            80,
            "Numer telefonu (polski format)",
        ),
        (
            "Adres",
            "address",
            r"(?:ul\.|al\.|pl\.|os\.)\s+[A-ZĄĆĘŁŃÓŚŹŻ][a-ząćęłńóśźż]+",
            "[ADRES]",
            70,
            "Adres z prefiksem ulicy/alei/placu/osiedla",
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO pii_rules (name, category, pattern, replacement, priority, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (name, category, pattern, replacement, priority, description) in rules {
        let affected = stmt.execute(rusqlite::params![
            name,
            category,
            pattern,
            replacement,
            priority,
            description
        ])?;
        if affected == 0 {
            debug!("Regula PII '{}' juz istnieje, pominieto", name);
        }
    }

    Ok(())
}

/// Seeduje domyslne szablony wezlow flow (paleta komponentow).
fn seed_flow_node_templates(conn: &Connection) -> Result<()> {
    // (node_type, category, label, description, default_config, icon)
    let templates: &[(&str, &str, &str, &str, &str, &str)] = &[
        (
            "trigger",
            "trigger",
            "Wyzwalacz",
            "Punkt wejścia flow (HTTP, QUIC, webhook)",
            "{}",
            "zap",
        ),
        (
            "llm",
            "service",
            "Model LLM",
            "Wywołanie modelu językowego",
            r#"{"model":"","prompt_id":"","system_prompt":"","temperature":0.7,"max_tokens":4096,"stream":true}"#,
            "brain",
        ),
        (
            "rag",
            "service",
            "RAG",
            "Wyszukiwanie w bazie wiedzy",
            r#"{"collection":"","top_k":5,"min_score":0.7}"#,
            "search",
        ),
        (
            "stt",
            "transform",
            "Rozpoznawanie mowy",
            "Zamiana mowy na tekst (STT)",
            r#"{"language":"pl","model":""}"#,
            "mic",
        ),
        (
            "tts",
            "service",
            "Synteza mowy",
            "Zamiana tekstu na mowę (TTS)",
            r#"{"language":"pl","voice":"","speed":1.0}"#,
            "volume-2",
        ),
        (
            "embeddings",
            "service",
            "Embeddingi",
            "Generowanie embeddingów tekstu",
            r#"{"model":""}"#,
            "hash",
        ),
        (
            "memory",
            "service",
            "Pamięć",
            "Odczyt/zapis pamięci konwersacji",
            r#"{"mode":"query","memory_type":"conversation","max_entries":10,"inject_to_messages":false,"context_prompt_id":"memory_context_template"}"#,
            "database",
        ),
        (
            "template",
            "transform",
            "Szablon",
            "Formatowanie tekstu z podstawianiem zmiennych",
            r#"{"template":""}"#,
            "file-text",
        ),
        (
            "pii_filter",
            "transform",
            "Filtr PII",
            "Usuwanie danych osobowych z tekstu",
            "{}",
            "shield",
        ),
        (
            "tts_clean",
            "transform",
            "Czyszczenie tekstu",
            "Czyszczenie i normalizacja tekstu dla TTS",
            "{}",
            "eraser",
        ),
        (
            "condition",
            "logic",
            "Warunek",
            "Rozgałęzienie warunkowe (if/else)",
            r#"{"field":"","operator":"equals","value":""}"#,
            "git-branch",
        ),
        (
            "switch",
            "logic",
            "Przełącznik",
            "Wielokrotny wybór (switch/case)",
            r#"{"field":"","cases":[]}"#,
            "list",
        ),
        (
            "router",
            "logic",
            "Router",
            "Przekazanie danych dalej",
            "{}",
            "send",
        ),
        (
            "output",
            "output",
            "Wyjście",
            "Punkt wyjścia flow",
            r#"{"format":"text"}"#,
            "send",
        ),
        (
            "conversation_history",
            "transform",
            "Historia rozmowy",
            "Zarządzanie historią konwersacji - wstrzykuje poprzednie wiadomości do kontekstu",
            r#"{"max_messages":20}"#,
            "message-circle",
        ),
        (
            "session_context",
            "transform",
            "Kontekst sesji",
            "Świadomość sesji - informuje LLM czy to początek/kontynuacja/niezrozumiała wiadomość",
            r#"{"first_prompt_id":"session_start","continue_prompt_id":"session_continue","unclear_prompt_id":"session_unclear"}"#,
            "clock",
        ),
        (
            "speaker_context",
            "transform",
            "Rozpoznawanie mówcy",
            "Identyfikacja głosu, personalizacja, obsługa nieznanego użytkownika",
            r#"{"high_threshold":0.85,"medium_threshold":0.60,"personalization_first_prompt":"personalization_first_template","personalization_continue_prompt":"personalization_continue_template","unknown_user_prompt":"unknown_user_strong","medium_confidence_known_prompt":"medium_confidence_known_template","medium_confidence_unknown_prompt":"medium_confidence_unknown","new_voice_prompt":"new_voice_during_conversation","new_speaker_prompt":"new_speaker_introduced_template"}"#,
            "user",
        ),
        (
            "memory_analyzer",
            "transform",
            "Analizator pamięci",
            "Decyzja czy odpytać bazę wiedzy (bielik-1.5b)",
            r#"{"mode":"query_analysis","prompt_id":"query_analysis_system"}"#,
            "cpu",
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO flow_node_templates (node_type, category, label, description, default_config, icon) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (node_type, category, label, description, default_config, icon) in templates {
        stmt.execute(rusqlite::params![
            node_type,
            category,
            label,
            description,
            default_config,
            icon
        ])?;
    }

    info!("Zaladowano szablony wezlow flow (INSERT OR IGNORE)");
    Ok(())
}

/// Seeduje domyslne reguly czyszczenia tekstu dla TTS (skroty polskie).
fn seed_tts_cleaning_rules(conn: &Connection) -> Result<()> {
    let abbreviations: &[(&str, &str, i64)] = &[
        ("np.", "na przykład", 10),
        ("m.in.", "między innymi", 11),
        ("itd.", "i tak dalej", 12),
        ("itp.", "i tym podobne", 13),
        ("tzw.", "tak zwany", 14),
        ("tzn.", "to znaczy", 15),
        ("tj.", "to jest", 16),
        ("dr.", "doktor", 17),
        ("mgr.", "magister", 18),
        ("inż.", "inżynier", 19),
        ("prof.", "profesor", 20),
        ("ul.", "ulica", 21),
        ("al.", "aleja", 22),
        ("pl.", "plac", 23),
        ("os.", "osiedle", 24),
        ("nr.", "numer", 25),
        ("tel.", "telefon", 26),
        ("godz.", "godzina", 27),
        ("min.", "minut", 28),
        ("sek.", "sekund", 29),
        ("pkt.", "punkt", 30),
        ("str.", "strona", 31),
        ("r.", "roku", 32),
        ("ok.", "około", 33),
        ("wg.", "według", 34),
        ("dot.", "dotyczący", 35),
        ("ds.", "do spraw", 36),
        ("ws.", "w sprawie", 37),
        ("zł.", "złotych", 38),
        ("tys.", "tysięcy", 39),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO tts_cleaning_rules (rule_type, pattern, replacement, language, priority) VALUES ('abbreviation', ?1, ?2, 'pl', ?3)",
    )?;
    for (pattern, replacement, priority) in abbreviations {
        let affected = stmt.execute(rusqlite::params![pattern, replacement, priority])?;
        if affected == 0 {
            debug!("Regula TTS '{}' juz istnieje, pominieto", pattern);
        }
    }

    Ok(())
}

/// Seeduje domyslne wzorce fast path (powitania, pozegnania, krotkie wiadomosci).
fn seed_fast_path_patterns(conn: &Connection) -> Result<()> {
    // (module, pattern_type, pattern, match_type, result_json, priority)
    let patterns: &[(&str, &str, &str, &str, &str, i64)] = &[
        // Intent Analyzer - powitania
        (
            "intent_analyzer",
            "greeting",
            "cześć",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "hej",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "hejka",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "siema",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "siemka",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "dzień dobry",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "dobry wieczór",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "witaj",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "witam",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "hello",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "hi",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "greeting",
            "yo",
            "exact",
            r#"{"intent":"greeting","confidence":1.0}"#,
            100,
        ),
        // Intent Analyzer - pozegnania
        (
            "intent_analyzer",
            "farewell",
            "pa",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "papa",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "do widzenia",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "do zobaczenia",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "na razie",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "bye",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "goodbye",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        (
            "intent_analyzer",
            "farewell",
            "dobranoc",
            "exact",
            r#"{"intent":"farewell","confidence":1.0}"#,
            100,
        ),
        // Intent Analyzer - krotkie wiadomosci
        (
            "intent_analyzer",
            "short_message",
            "3",
            "length",
            r#"{"intent":"too_short","confidence":1.0}"#,
            90,
        ),
        // Memory Analyzer - powitania
        (
            "memory_analyzer",
            "greeting",
            "cześć",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "hej",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "hejka",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "siema",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "siemka",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "dzień dobry",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "dobry wieczór",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "witaj",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "witam",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "hello",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "hi",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "yo",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "hej jarvis",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "cześć jarvis",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        (
            "memory_analyzer",
            "greeting",
            "witaj jarvis",
            "exact",
            r#"{"skip_memory":true,"reason":"greeting"}"#,
            100,
        ),
        // Memory Analyzer - pytania do AI
        (
            "memory_analyzer",
            "question_to_ai",
            "jak się masz",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "co słychać",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "co robisz",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "co porabiasz",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "jak tam",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "co u ciebie",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "pomóż mi",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "pomocy",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        (
            "memory_analyzer",
            "question_to_ai",
            "help",
            "exact",
            r#"{"skip_memory":true,"reason":"question_to_ai"}"#,
            90,
        ),
        // Memory Analyzer - przedstawienia
        (
            "memory_analyzer",
            "introduction",
            "jestem",
            "starts_with",
            r#"{"skip_memory":false,"reason":"introduction","extract_name":true}"#,
            80,
        ),
        (
            "memory_analyzer",
            "introduction",
            "mam na imię",
            "starts_with",
            r#"{"skip_memory":false,"reason":"introduction","extract_name":true}"#,
            80,
        ),
        (
            "memory_analyzer",
            "introduction",
            "nazywam się",
            "starts_with",
            r#"{"skip_memory":false,"reason":"introduction","extract_name":true}"#,
            80,
        ),
        (
            "memory_analyzer",
            "introduction",
            "moje imię to",
            "starts_with",
            r#"{"skip_memory":false,"reason":"introduction","extract_name":true}"#,
            80,
        ),
        // Memory Analyzer - krotkie wiadomosci
        (
            "memory_analyzer",
            "short_message",
            "5",
            "length",
            r#"{"skip_memory":true,"reason":"too_short"}"#,
            90,
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO fast_path_patterns (module, pattern_type, pattern, match_type, result_json, priority) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for (module, pattern_type, pattern, match_type, result_json, priority) in patterns {
        let affected = stmt.execute(rusqlite::params![
            module,
            pattern_type,
            pattern,
            match_type,
            result_json,
            priority
        ])?;
        if affected == 0 {
            debug!(
                "Wzorzec fast path '{}/{}' juz istnieje, pominieto",
                module, pattern
            );
        }
    }

    Ok(())
}

/// Seeduje domyslne prompty systemowe do tabeli prompts.
///
/// Od T1.2 seed zawiera wylacznie `transcription_summarization` w 5 jezykach
/// (pl/en/de/es/fr). Wszystkie stare prompty (jarvis_system, session_*,
/// personalization_*, itd.) zostaly usuniete — migracja 52 czysci tabele.
fn seed_prompts(conn: &Connection) -> Result<()> {
    seed_transcription_summarization_prompt(conn)?;
    Ok(())
}

/// Wstawia prompt `transcription_summarization` w pieciu jezykach. Kazdy wiersz
/// ma `is_system=1` (nadpisywalny przy kolejnych uruchomieniach — jesli user
/// nie zmienil recznie, wtedy `is_system` jest nadal 1 i seed moze odswiezyc).
fn seed_transcription_summarization_prompt(conn: &Connection) -> Result<()> {
    // (language, name, description, content)
    let variants: &[(&str, &str, &str, &str)] = &[
        (
            "pl",
            "Podsumowanie transkrypcji",
            "Strukturalne podsumowanie fragmentu transkrypcji spotkania (JSON).",
            PROMPT_TRANSCRIPTION_SUMMARIZATION_PL,
        ),
        (
            "en",
            "Transcription summarization",
            "Structured summary of a meeting transcript fragment (JSON).",
            PROMPT_TRANSCRIPTION_SUMMARIZATION_EN,
        ),
        (
            "de",
            "Zusammenfassung des Transkripts",
            "Strukturierte Zusammenfassung eines Besprechungstranskript-Ausschnitts (JSON).",
            PROMPT_TRANSCRIPTION_SUMMARIZATION_DE,
        ),
        (
            "es",
            "Resumen de transcripción",
            "Resumen estructurado de un fragmento de transcripción de reunión (JSON).",
            PROMPT_TRANSCRIPTION_SUMMARIZATION_ES,
        ),
        (
            "fr",
            "Résumé de la transcription",
            "Résumé structuré d'un extrait de transcription de réunion (JSON).",
            PROMPT_TRANSCRIPTION_SUMMARIZATION_FR,
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT INTO prompts \
             (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version, language, is_system) \
         VALUES ('transcription_summarization', ?1, ?2, ?3, 'system', NULL, NULL, 100, 1, 1, ?4, 1) \
         ON CONFLICT(prompt_id, language) DO UPDATE SET \
             name = excluded.name, \
             description = excluded.description, \
             content = excluded.content, \
             updated_at = datetime('now') \
         WHERE is_system = 1",
    )?;

    for (language, name, description, content) in variants {
        stmt.execute(rusqlite::params![name, description, content, language])?;
    }

    info!("Zaladowano prompty transcription_summarization (5 jezykow)");
    Ok(())
}

// Prompty transcription_summarization — osobne stale zeby nie zasmiecac funkcji.
// Klucze JSON (`decisions`, `action_items`, `owner`, `task`, `deadline`,
// `summary_text`) pozostaja w snake_case po angielsku, bo parser oczekuje
// tych nazw niezaleznie od jezyka instrukcji.

const PROMPT_TRANSCRIPTION_SUMMARIZATION_PL: &str = r#"Jesteś asystentem spotkań biznesowych. Na podstawie poniższego fragmentu transkryptu spotkania wyciągnij strukturalne podsumowanie.

Zwróć wyłącznie JSON w formacie:
{
  "decisions": "Krótki opis kluczowych decyzji podjętych w tym fragmencie (1-3 zdania, zwięźle).",
  "action_items": [
    {
      "owner": "Imię osoby odpowiedzialnej (lub 'Nieokreślone' jeśli brak)",
      "task": "Treść zadania do wykonania",
      "deadline": "Termin w formie jaka padła w rozmowie (np. 'dziś 16:00', 'do piątku', 'po merge'). Wpisz 'brak daty' jeśli nie podano."
    }
  ],
  "summary_text": "Zwięzłe podsumowanie fragmentu (2-4 zdania) obejmujące temat, obecny stan prac i najważniejsze problemy."
}

Format transkryptu wejściowego: każda wypowiedź poprzedzona jest etykietą mówcy w kwadratowych nawiasach, np. `[Jan Kowalski] Treść wypowiedzi.`. Mówcy nierozpoznani mają etykietę `[SPEAKER_00]`, `[SPEAKER_01]` itd.

Nie dodawaj pól których brak w powyższym schemacie. Nie komentuj. Zwróć wyłącznie valid JSON."#;

const PROMPT_TRANSCRIPTION_SUMMARIZATION_EN: &str = r#"You are a business meeting assistant. Based on the following meeting transcript fragment, extract a structured summary.

Return only JSON in the format:
{
  "decisions": "Brief description of key decisions made in this fragment (1-3 sentences, concise).",
  "action_items": [
    {
      "owner": "Name of the responsible person (or 'Unspecified' if missing)",
      "task": "Content of the task to be done",
      "deadline": "Deadline as stated in the conversation (e.g. 'today 4pm', 'by Friday', 'after merge'). Use 'no date' if none was given."
    }
  ],
  "summary_text": "Concise summary of the fragment (2-4 sentences) covering the topic, current state of work, and most important issues."
}

Input transcript format: each utterance is prefixed with a speaker label in square brackets, e.g. `[John Smith] Utterance text.`. Unrecognized speakers are labelled `[SPEAKER_00]`, `[SPEAKER_01]`, etc.

Do not add fields not present in the schema above. Do not comment. Return valid JSON only."#;

const PROMPT_TRANSCRIPTION_SUMMARIZATION_DE: &str = r#"Du bist ein Assistent für Geschäftsbesprechungen. Extrahiere auf Basis des folgenden Besprechungstranskript-Ausschnitts eine strukturierte Zusammenfassung.

Gib ausschließlich JSON im folgenden Format zurück:
{
  "decisions": "Kurze Beschreibung der wichtigsten in diesem Ausschnitt getroffenen Entscheidungen (1-3 Sätze, prägnant).",
  "action_items": [
    {
      "owner": "Name der verantwortlichen Person (oder 'Nicht angegeben', falls nicht genannt)",
      "task": "Inhalt der auszuführenden Aufgabe",
      "deadline": "Termin in der Form wie im Gespräch genannt (z. B. 'heute 16:00', 'bis Freitag', 'nach dem Merge'). Schreibe 'kein Datum', falls keines angegeben wurde."
    }
  ],
  "summary_text": "Prägnante Zusammenfassung des Ausschnitts (2-4 Sätze), die Thema, aktuellen Stand der Arbeit und die wichtigsten Probleme abdeckt."
}

Format des Eingabe-Transkripts: jede Äußerung ist mit einem Sprecher-Label in eckigen Klammern versehen, z. B. `[Max Müller] Inhalt der Äußerung.`. Unerkannte Sprecher erhalten `[SPEAKER_00]`, `[SPEAKER_01]` usw.

Füge keine Felder hinzu, die nicht im obigen Schema stehen. Kommentiere nicht. Gib ausschließlich gültiges JSON zurück."#;

const PROMPT_TRANSCRIPTION_SUMMARIZATION_ES: &str = r#"Eres un asistente de reuniones de negocios. Basándote en el siguiente fragmento de transcripción de la reunión, extrae un resumen estructurado.

Devuelve únicamente JSON con el formato:
{
  "decisions": "Descripción breve de las decisiones clave tomadas en este fragmento (1-3 frases, conciso).",
  "action_items": [
    {
      "owner": "Nombre de la persona responsable (o 'No especificado' si falta)",
      "task": "Contenido de la tarea a realizar",
      "deadline": "Plazo tal como se mencionó en la conversación (p. ej. 'hoy a las 16:00', 'antes del viernes', 'después del merge'). Escribe 'sin fecha' si no se indicó ninguna."
    }
  ],
  "summary_text": "Resumen conciso del fragmento (2-4 frases) que abarque el tema, el estado actual del trabajo y los problemas más importantes."
}

Formato de la transcripción de entrada: cada intervención está precedida por una etiqueta del hablante entre corchetes, p. ej. `[Juan Pérez] Contenido de la intervención.`. Los hablantes no reconocidos llevan la etiqueta `[SPEAKER_00]`, `[SPEAKER_01]`, etc.

No añadas campos que no estén en el esquema anterior. No comentes. Devuelve únicamente JSON válido."#;

const PROMPT_TRANSCRIPTION_SUMMARIZATION_FR: &str = r#"Tu es un assistant de réunions professionnelles. À partir de l'extrait de transcription de réunion ci-dessous, extrais un résumé structuré.

Renvoie uniquement du JSON au format :
{
  "decisions": "Brève description des décisions clés prises dans cet extrait (1 à 3 phrases, concis).",
  "action_items": [
    {
      "owner": "Nom de la personne responsable (ou 'Non précisé' si absent)",
      "task": "Contenu de la tâche à réaliser",
      "deadline": "Échéance telle que mentionnée dans la conversation (par ex. 'aujourd'hui 16h', 'avant vendredi', 'après le merge'). Indique 'pas de date' si aucune n'a été donnée."
    }
  ],
  "summary_text": "Résumé concis de l'extrait (2 à 4 phrases) couvrant le sujet, l'état actuel des travaux et les problèmes les plus importants."
}

Format de la transcription en entrée : chaque intervention est précédée d'une étiquette de locuteur entre crochets, par ex. `[Jean Dupont] Contenu de l'intervention.`. Les locuteurs non identifiés sont étiquetés `[SPEAKER_00]`, `[SPEAKER_01]`, etc.

N'ajoute pas de champs absents du schéma ci-dessus. Ne commente pas. Renvoie uniquement du JSON valide."#;


/// Seeduje domyslne diagramy flow reprezentujace pipeline routera.
fn seed_default_flows(conn: &Connection) -> Result<()> {
    let flows: &[(&str, &str, &str, &str, i64)] = &[
        (
            "Standardowy pipeline LLM",
            "Pipeline rozmowy z historią, kontekstem sesji, rozpoznawaniem mówcy, analizą pamięci i warunkowym odczytem",
            "llm",
            r#"{"nodes":[{"id":"n1","type":"trigger","label":"Wyzwalacz","x":60,"y":280,"config":{}},{"id":"n2","type":"pii_filter","label":"Filtr PII (request)","x":260,"y":280,"config":{}},{"id":"n3","type":"conversation_history","label":"Historia rozmowy","x":460,"y":280,"config":{"max_messages":20}},{"id":"n4","type":"session_context","label":"Kontekst sesji","x":660,"y":280,"config":{"first_prompt_id":"session_start","continue_prompt_id":"session_continue","unclear_prompt_id":"session_unclear"}},{"id":"n5","type":"speaker_context","label":"Rozpoznawanie mowcy","x":860,"y":280,"config":{"high_threshold":0.85,"medium_threshold":0.60,"personalization_first_prompt":"personalization_first_template","personalization_continue_prompt":"personalization_continue_template","unknown_user_prompt":"unknown_user_strong","medium_confidence_known_prompt":"medium_confidence_known_template","medium_confidence_unknown_prompt":"medium_confidence_unknown","new_voice_prompt":"new_voice_during_conversation","new_speaker_prompt":"new_speaker_introduced_template"}},{"id":"n6","type":"memory_analyzer","label":"Analizator pamieci","x":1060,"y":280,"config":{"mode":"query_analysis","prompt_id":"query_analysis_system"}},{"id":"n7","type":"condition","label":"Czy odpytac pamiec?","x":1260,"y":280,"config":{"field":"should_query","operator":"equals","value":true}},{"id":"n8","type":"memory","label":"Pamiec - odczyt","x":1460,"y":200,"config":{"mode":"query","inject_to_messages":true,"context_prompt_id":"memory_context_template"}},{"id":"n9","type":"llm","label":"Model LLM","x":1660,"y":280,"config":{"prompt_id":"jarvis_system","temperature":0.7,"max_tokens":4096,"stream":true,"use_messages_context":true}},{"id":"n10","type":"pii_filter","label":"Filtr PII (response)","x":1860,"y":280,"config":{}},{"id":"n11","type":"tts_clean","label":"Czyszczenie tekstu","x":2060,"y":280,"config":{}},{"id":"n12","type":"output","label":"Wyjscie","x":2260,"y":280,"config":{"format":"text"}}],"edges":[{"id":"e1","from_node":"n1","to_node":"n2","from_port":"default"},{"id":"e2","from_node":"n2","to_node":"n3","from_port":"default"},{"id":"e3","from_node":"n3","to_node":"n4","from_port":"default"},{"id":"e4","from_node":"n4","to_node":"n5","from_port":"default"},{"id":"e5","from_node":"n5","to_node":"n6","from_port":"default"},{"id":"e6","from_node":"n6","to_node":"n7","from_port":"default"},{"id":"e7","from_node":"n7","to_node":"n8","from_port":"true","condition":"true"},{"id":"e8","from_node":"n7","to_node":"n9","from_port":"false","condition":"false"},{"id":"e9","from_node":"n8","to_node":"n9","from_port":"default"},{"id":"e10","from_node":"n9","to_node":"n10","from_port":"default"},{"id":"e11","from_node":"n10","to_node":"n11","from_port":"default"},{"id":"e12","from_node":"n11","to_node":"n12","from_port":"default"}]}"#,
            1,
        ),
        (
            "Standardowy pipeline RAG",
            "Pipeline wyszukiwania w bazie wiedzy: RAG, LLM z kontekstem, filtr PII na odpowiedzi",
            "rag",
            r#"{"nodes":[{"id":"n1","type":"trigger","label":"Wyzwalacz","x":60,"y":280,"config":{}},{"id":"n2","type":"rag","label":"RAG","x":280,"y":280,"config":{"top_k":5,"min_similarity":0.7,"search_modes":["VectorSearch","FullTextSearch"]}},{"id":"n3","type":"llm","label":"Model LLM","x":500,"y":280,"config":{"prompt_id":"rag_system","temperature":0.7,"max_tokens":4096}},{"id":"n4","type":"pii_filter","label":"Filtr PII","x":720,"y":280,"config":{}},{"id":"n5","type":"output","label":"Wyjscie","x":940,"y":280,"config":{"format":"text"}}],"edges":[{"id":"e1","from_node":"n1","to_node":"n2","from_port":"default"},{"id":"e2","from_node":"n2","to_node":"n3","from_port":"default"},{"id":"e3","from_node":"n3","to_node":"n4","from_port":"default"},{"id":"e4","from_node":"n4","to_node":"n5","from_port":"default"}]}"#,
            0,
        ),
        (
            "Standardowy pipeline STT",
            "Pipeline rozpoznawania mowy: STT, czyszczenie tekstu",
            "stt",
            r#"{"nodes":[{"id":"n1","type":"trigger","label":"Wyzwalacz","x":60,"y":280,"config":{}},{"id":"n2","type":"stt","label":"Rozpoznawanie mowy","x":280,"y":280,"config":{}},{"id":"n3","type":"tts_clean","label":"Czyszczenie tekstu","x":500,"y":280,"config":{}},{"id":"n4","type":"output","label":"Wyjscie","x":720,"y":280,"config":{"format":"text"}}],"edges":[{"id":"e1","from_node":"n1","to_node":"n2","from_port":"default"},{"id":"e2","from_node":"n2","to_node":"n3","from_port":"default"},{"id":"e3","from_node":"n3","to_node":"n4","from_port":"default"}]}"#,
            0,
        ),
        (
            "Standardowy pipeline TTS",
            "Pipeline syntezy mowy: czyszczenie tekstu, TTS",
            "tts",
            r#"{"nodes":[{"id":"n1","type":"trigger","label":"Wyzwalacz","x":60,"y":280,"config":{}},{"id":"n2","type":"tts_clean","label":"Czyszczenie tekstu","x":280,"y":280,"config":{}},{"id":"n3","type":"tts","label":"Synteza mowy","x":500,"y":280,"config":{}},{"id":"n4","type":"output","label":"Wyjscie","x":720,"y":280,"config":{"format":"text"}}],"edges":[{"id":"e1","from_node":"n1","to_node":"n2","from_port":"default"},{"id":"e2","from_node":"n2","to_node":"n3","from_port":"default"},{"id":"e3","from_node":"n3","to_node":"n4","from_port":"default"}]}"#,
            0,
        ),
        (
            "teams-flow",
            "Domyslny flow dla teams-bot (trigger -> llm -> output)",
            "agents",
            r#"{"nodes":[{"id":"trigger","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"llm","type":"llm","position":{"x":200,"y":0},"config":{"model_alias":"teams-summarization"}},{"id":"output","type":"output","position":{"x":400,"y":0},"config":{}}],"edges":[{"from":"trigger","to":"llm"},{"from":"llm","to":"output"}]}"#,
            0,
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT INTO flows (name, description, service_type, flow_json, status, is_default) \
         SELECT ?1, ?2, ?3, ?4, 'active', ?5 \
         WHERE NOT EXISTS (SELECT 1 FROM flows WHERE name = ?1)",
    )?;

    for (name, description, service_type, flow_json, is_default) in flows {
        let affected = stmt.execute(rusqlite::params![
            name,
            description,
            service_type,
            flow_json,
            is_default
        ])?;
        if affected > 0 {
            debug!("Utworzono domyslny flow: {}", name);
        }
    }

    Ok(())
}

/// Seeduje domyslne aliasy modeli dla pipeline RAG.
/// Domyslne aliasy — INSERT OR IGNORE nie nadpisze istniejacych wpisow.
fn seed_model_aliases(conn: &Connection) -> Result<()> {
    let aliases: &[(&str, &str)] = &[
        ("rag-embeddings", "embeddings-gemma"),
        ("rag-summarization", "bielik-11b"),
        ("rag-generation", "bielik-11b"),
        ("rag-reranker", "jina-reranker-v3"),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO model_aliases (alias, target_model, is_active) VALUES (?1, ?2, 1)",
    )?;
    for (alias, target) in aliases {
        let affected = stmt.execute(rusqlite::params![alias, target])?;
        if affected == 0 {
            debug!("Alias modelu '{}' juz istnieje, pominieto", alias);
        }
    }

    Ok(())
}

/// Migruje hasla z formatu SHA256 (hex) na argon2 (PHC string).
/// Wykrywa stary format po braku prefiksu "$argon2".
fn migrate_sha256_passwords(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id, username, password_hash FROM users")?;
    let users: Vec<(i64, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, username, hash) in &users {
        if !hash.starts_with("$argon2") {
            let new_hash = crypto::hash_password("admin")?;
            conn.execute(
                "UPDATE users SET password_hash = ?1, must_change_password = 1 WHERE id = ?2",
                rusqlite::params![new_hash, id],
            )?;
            info!(
                "Zmigrowano haslo uzytkownika '{}' z SHA256 na argon2 (wymagana zmiana hasla)",
                username
            );
        }
    }

    Ok(())
}

/// Generuje kryptograficznie losowy JWT secret (32 bajty -> 64 znaki hex)
fn generate_jwt_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("OS RNG fill_bytes");
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    /// T1.2 — swieza baza ma dokladnie 5 promptow transcription_summarization
    /// (po jednym na jezyk pl/en/de/es/fr) i zadnych starych promptow.
    #[test]
    fn fresh_db_has_only_transcription_summarization_prompts() {
        let pool = crate::db::init(Path::new(":memory:")).expect("init db");
        let conn = pool.lock().unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 5, "powinno byc 5 promptow, jest {}", total);

        let langs: Vec<String> = conn
            .prepare("SELECT language FROM prompts WHERE prompt_id = 'transcription_summarization' ORDER BY language")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(langs, vec!["de", "en", "es", "fr", "pl"]);

        let other: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prompts WHERE prompt_id != 'transcription_summarization'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other, 0, "nie powinno byc innych promptow niz transcription_summarization");

        let is_system_all: i64 = conn
            .query_row("SELECT COUNT(*) FROM prompts WHERE is_system = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(is_system_all, 5);
    }

    /// T1.2 — swieza baza ma flow 'teams-flow' w seedzie.
    #[test]
    fn fresh_db_has_teams_flow() {
        let pool = crate::db::init(Path::new(":memory:")).expect("init db");
        let conn = pool.lock().unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM flows WHERE name = 'teams-flow'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "oczekiwany 1 wiersz teams-flow");
    }

    /// find_prompt z fallback na 'pl' gdy dany jezyk nie istnieje.
    #[test]
    fn find_prompt_falls_back_to_pl() {
        let pool = crate::db::init(Path::new(":memory:")).expect("init db");

        let pl = crate::db::repository::find_prompt(&pool, "transcription_summarization", "pl")
            .unwrap()
            .expect("pl wariant istnieje");
        assert_eq!(pl.language, "pl");

        let en = crate::db::repository::find_prompt(&pool, "transcription_summarization", "en")
            .unwrap()
            .expect("en wariant istnieje");
        assert_eq!(en.language, "en");

        // Jezyk nieistniejacy -> fallback na pl
        let fallback = crate::db::repository::find_prompt(&pool, "transcription_summarization", "it")
            .unwrap()
            .expect("fallback na pl");
        assert_eq!(fallback.language, "pl");

        // Nieistniejacy prompt -> None
        let none = crate::db::repository::find_prompt(&pool, "does_not_exist", "pl").unwrap();
        assert!(none.is_none());
    }
}
