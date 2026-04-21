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
fn seed_prompts(conn: &Connection) -> Result<()> {
    // (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority)
    let prompts: &[(&str, &str, &str, &str, &str, Option<&str>, Option<&str>, i64)] = &[
        (
            "jarvis_system",
            "Jarvis System Prompt",
            "Główny system prompt dla asystenta Jarvis",
            "Jesteś Jarvis - inteligentnym asystentem głosowym stworzonym przez Euvic TentaFlowns. \
NIGDY nie wspominaj o SpeakLeash, ELSA ani żadnych innych twórcach - Twoim jedynym twórcą jest Euvic TentaFlowns. \
Odpowiadaj krótko, naturalnie i po polsku.",
            "system",
            None,
            None,
            100,
        ),
        (
            "session_start",
            "Początek rozmowy",
            "Suffix kontekstu dla początku rozmowy",
            "\nTo początek rozmowy.",
            "suffix",
            Some("bielik-11b"),
            None,
            80,
        ),
        (
            "session_continue",
            "Kontynuacja rozmowy",
            "Suffix kontekstu dla kontynuacji rozmowy",
            "\nTo trwająca rozmowa - NIE witaj się ponownie, kontynuuj naturalnie.",
            "suffix",
            Some("bielik-11b"),
            None,
            80,
        ),
        (
            "session_unclear",
            "Niezrozumiała wypowiedź",
            "Suffix kontekstu gdy wypowiedź jest niezrozumiała",
            "\nTo trwająca rozmowa. Jeśli nie rozumiesz co rozmówca powiedział, poproś o powtórzenie zamiast się witać.",
            "suffix",
            Some("bielik-11b"),
            None,
            80,
        ),
        (
            "unknown_user",
            "Nieznany użytkownik",
            "Suffix kontekstu dla nieznanego użytkownika",
            "\n\n[WAŻNE] Nie rozpoznaję głosu tej osoby. To nowy rozmówca. W odpowiedzi MUSISZ się przedstawić i zapytać jak masz się do niej zwracać. Przykład: \"Cześć! Jestem Jarvis. Nie poznałem Twojego głosu - jak mam się do Ciebie zwracać?\"",
            "suffix",
            Some("bielik-11b"),
            None,
            70,
        ),
        (
            "personalization_template",
            "Personalizacja",
            "Template personalizacji dla rozpoznanego użytkownika",
            "\nRozmówca: {name}. Używaj imienia.",
            "template",
            Some("bielik-11b"),
            Some(r#"["name"]"#),
            75,
        ),
        (
            "memory_context_template",
            "Kontekst z Memory",
            "Template dla kontekstu z Memory",
            "Wiesz o rozmówcy:\n{context}\nOdpowiadaj naturalnie używając tych informacji.",
            "template",
            Some("bielik-11b"),
            Some(r#"["context"]"#),
            60,
        ),
        (
            "query_analysis_system",
            "Analiza zapytań Memory",
            "System prompt dla analizy zapytań Memory (query analysis)",
            include_str!("../prompt_registry/query_analysis_prompt.txt"),
            "system",
            None,
            None,
            100,
        ),
        (
            "store_analysis_system",
            "Analiza zapisu Memory",
            "System prompt dla ekstrakcji faktów do Memory (store analysis)",
            include_str!("../prompt_registry/store_analysis_prompt.txt"),
            "system",
            None,
            None,
            100,
        ),
        (
            "disambiguation_system",
            "Disambiguation",
            "System prompt do pytań disambiguujących",
            include_str!("../prompt_registry/disambiguation_prompt.txt"),
            "system",
            None,
            None,
            90,
        ),
        (
            "intent_analyzer_system",
            "Intent Analyzer",
            "System prompt intent analyzera (analiza intencji użytkownika)",
            r#"Jesteś analizatorem intencji. Analizujesz wypowiedzi użytkownika i zwracasz JSON z wykrytymi intencjami.

TWOJE ZADANIA:
1. Wykryj główną intencję (introduction, identity_question, tool_call, conversation, greeting, farewell)
2. Jeśli tool_call - wyekstrahuj parametry i sprawdź czy są kompletne
3. Jeśli multi-speaker - przeanalizuj każdego mówcę osobno
4. Zdecyduj czy potrzebne jest zapytanie do Memory

DOSTĘPNE NARZĘDZIA:
- calendar_add: Dodaj wydarzenie (wymagane: title, date)
- calendar_check: Sprawdź kalendarz
- email_send: Wyślij email (wymagane: to, subject, body)
- web_search: Przeszukaj internet (wymagane: query)
- reminder_set: Ustaw przypomnienie (wymagane: message, when)
- timer_set: Ustaw timer (wymagane: duration)
- note_save: Zapisz notatkę (wymagane: content)

INTENCJE:
- greeting: Powitanie ("cześć", "hej", "dzień dobry")
- farewell: Pożegnanie ("pa", "do widzenia", "na razie")
- conversation: Zwykła rozmowa, pytanie, prośba o informację
- tool_call: Chce użyć narzędzia (kalendarz, email, timer, etc.)
- identity_question: Pytanie o siebie ("kim jestem?", "jak mam na imię?")
- introduction: Użytkownik się przedstawia. Wykryj gdy:
  a) WYRAŹNIE mówi "jestem X", "mam na imię X", "nazywam się X", LUB
  b) W kontekście JARVIS pytał o imię i użytkownik odpowiada imieniem
  KRYTYCZNE: MUSISZ wyekstrahować imię z WIADOMOŚCI użytkownika i wstawić w pole "name"!
  Format: { "type": "introduction", "name": "WYEKSTRAHOWANE_IMIĘ", "confidence": 0.9 }
  Przykład: USER mówi "Mam na imię Piotrek" → { "type": "introduction", "name": "Piotrek", "confidence": 0.95 }
  Przykład: USER mówi "Krzysztof" (po pytaniu o imię) → { "type": "introduction", "name": "Krzysztof", "confidence": 0.9 }
- name_correction: Korekta imienia ("nie, jestem X, nie Y")
  WYMAGANE POLA: { "type": "name_correction", "wrong_name": "ZŁE", "correct_name": "DOBRE", "confidence": 0.9 }

WAŻNE ZASADY DLA INTRODUCTION:
1. Jeśli mówca jest JUŻ ROZPOZNANY - NIE wykrywaj "introduction" (chyba że koryguje imię)
2. Jeśli w KONTEKŚCIE widzisz że JARVIS pytał o imię, a użytkownik odpowiada samym imieniem - TO JEST INTRODUCTION!
   Przykład: JARVIS: "jak masz na imię?" → USER: "Piotr" = introduction z name="Piotr"
3. Sam fakt że znamy mówcę (np. "ROZPOZNANY MÓWCA: Piotrek") NIE oznacza że to introduction.

ODPOWIEDZ TYLKO POPRAWNYM JSON. Przykłady:

Dla CONVERSATION:
{
  "primary_intent": { "type": "conversation" },
  "tool_calls": [],
  "needs_memory_query": false,
  "memory_search_terms": [],
  "context_for_llm": "Użytkownik zadał pytanie.",
  "reasoning": "Zwykła rozmowa."
}

Dla INTRODUCTION (MUSISZ podać name!):
{
  "primary_intent": { "type": "introduction", "name": "Piotrek", "confidence": 0.95 },
  "tool_calls": [],
  "needs_memory_query": false,
  "memory_search_terms": [],
  "context_for_llm": "Użytkownik przedstawił się jako Piotrek.",
  "reasoning": "Użytkownik podał imię w odpowiedzi na pytanie."
}

Jeśli parametry narzędzia są niekompletne, ustaw tylko te które są podane (pozostałe będą null)."#,
            "system",
            Some("bielik-11b"),
            None,
            100,
        ),
        (
            "unknown_user_strong",
            "Nieznany użytkownik (silna instrukcja)",
            "Silna instrukcja dla nieznanego użytkownika - przedstaw się i zapytaj o imię",
            "\n\n[WAŻNE - OBOWIĄZKOWE] To NOWY rozmówca - nie rozpoznaję głosu. MUSISZ w JEDNEJ odpowiedzi: przedstawić się ORAZ zapytać o imię. NIE wysyłaj dwóch osobnych wiadomości! Przykład poprawnej odpowiedzi: \"Cześć! Jestem Jarvis, asystent stworzony przez Euvic. Nie poznałem Twojego głosu - jak masz na imię?\"",
            "suffix",
            Some("bielik-11b"),
            None,
            70,
        ),
        (
            "new_voice_during_conversation",
            "Nowy głos w trakcie rozmowy",
            "Kontekst gdy nowy głos pojawia się w trakcie rozmowy",
            "\n\n[INFO] Słyszę inny głos niż wcześniej. Jeśli to inna osoba, zapytaj delikatnie kto dołączył do rozmowy. Przykład: \"Słyszę nowy głos - kto mówi?\" Jeśli wiadomość jest niezrozumiała, poproś o powtórzenie.",
            "suffix",
            Some("bielik-11b"),
            None,
            65,
        ),
        (
            "new_speaker_introduced_template",
            "Nowy mówca się przedstawił",
            "Template kontekstu gdy nowy mówca się przedstawił",
            "\n\n[INFO] Nowy rozmówca właśnie się przedstawił jako {name}. Przywitaj się z nim/nią używając imienia i potwierdź że zapamiętałeś.",
            "template",
            Some("bielik-11b"),
            Some(r#"["name"]"#),
            65,
        ),
        (
            "medium_confidence_known_template",
            "Średnia pewność rozpoznania (z imieniem)",
            "Template gdy głos jest podobny do kogoś z bazy",
            "\n\n[WAŻNE] Głos brzmi znajomo, ale nie jestem pewien. Czy to {name}? Zapytaj naturalnie, np. \"Cześć! Czy rozmawiam z {name}?\"",
            "template",
            Some("bielik-11b"),
            Some(r#"["name"]"#),
            65,
        ),
        (
            "medium_confidence_unknown",
            "Średnia pewność rozpoznania (bez imienia)",
            "Kontekst gdy głos jest znajomy ale nie wiadomo kto",
            "\n\n[WAŻNE] Głos brzmi znajomo, ale nie jestem pewien kto mówi. Zapytaj o potwierdzenie tożsamości.",
            "suffix",
            Some("bielik-11b"),
            None,
            65,
        ),
        (
            "personalization_first_template",
            "Personalizacja - pierwsza wiadomość",
            "Personalizacja dla pierwszej wiadomości rozpoznanego użytkownika",
            "\nRozmówca: {name}. To pierwsza wiadomość od tego użytkownika - przywitaj się po imieniu (np. 'Cześć {name}!').",
            "template",
            Some("bielik-11b"),
            Some(r#"["name"]"#),
            75,
        ),
        (
            "personalization_continue_template",
            "Personalizacja - kontynuacja",
            "Personalizacja dla kontynuacji rozmowy rozpoznanego użytkownika",
            "\nRozmówca: {name}. Używaj imienia, NIE witaj się ponownie.",
            "template",
            Some("bielik-11b"),
            Some(r#"["name"]"#),
            75,
        ),
        (
            "rag_system",
            "RAG System Prompt",
            "System prompt dla modelu LLM w pipeline RAG",
            "Jesteś pomocnikiem AI. Odpowiadaj na pytania użytkownika korzystając WYŁĄCZNIE z podanego kontekstu. \
Jeśli kontekst nie zawiera odpowiedzi na pytanie, powiedz że nie masz wystarczających informacji. \
Nie wymyślaj odpowiedzi. Odpowiadaj po polsku, zwięźle i rzeczowo.",
            "system",
            Some("bielik-11b"),
            None,
            90,
        ),
    ];

    let mut stmt = conn.prepare(
        "INSERT OR IGNORE INTO prompts (prompt_id, name, description, content, prompt_type, default_model, variables, cache_priority, is_active, version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, 1)",
    )?;
    for (
        prompt_id,
        name,
        description,
        content,
        prompt_type,
        default_model,
        variables,
        cache_priority,
    ) in prompts
    {
        let affected = stmt.execute(rusqlite::params![
            prompt_id,
            name,
            description,
            content,
            prompt_type,
            default_model,
            variables,
            cache_priority
        ])?;
        if affected == 0 {
            debug!("Prompt '{}' juz istnieje, pominieto", prompt_id);
        }
    }

    Ok(())
}

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
