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
    seed_prompts(&tx)?;
    seed_default_flows(&tx)?;

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
            r#"{"mode":"query","memory_type":"conversation","max_entries":10,"inject_to_messages":false,"context_prompt_id":""}"#,
            "database",
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
            r#"{"first_prompt_id":"","continue_prompt_id":"","unclear_prompt_id":""}"#,
            "clock",
        ),
        (
            "speaker_context",
            "transform",
            "Rozpoznawanie mówcy",
            "Identyfikacja głosu, personalizacja, obsługa nieznanego użytkownika",
            r#"{"high_threshold":0.85,"medium_threshold":0.60,"personalization_first_prompt":"","personalization_continue_prompt":"","unknown_user_prompt":"","medium_confidence_known_prompt":"","medium_confidence_unknown_prompt":"","new_voice_prompt":"","new_speaker_prompt":""}"#,
            "user",
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
    // R3d (plan v7): zestaw default flows do wystawienia z fresh DB.
    // - "Standardowy pipeline LLM" (Safe Chat) — z filtrem PII, default=1
    // - "Default Chat" — bez PII, opt-in dla scenariuszy ktore nie chca filtru
    // - "Standardowy pipeline TTS" — czyszczenie tekstu + TTS, default=1
    // - "Audio Chat" — STT (z diarization) → LLM → output, dla `audio_input`
    //   requestow (D.18: graph z STT na trigger -> input_modalities=[Audio]
    //   auto-inferred przez catalog provider)
    // - "teams-flow" — flow dla teams-bot (non-default)
    //
    // RAG flows usuniete razem z RAG path; nowa implementacja RAG bedzie
    // miala wlasne default flows.
    // Wszystkie chat/agents flowy seedowane jako STREAMING (LLM -> pii_filter
    // -> output z mode=stream, edges od LLM dalej z from_port=stream).
    // Bez tego try_dispatch_streaming wpada na is_streaming=false ->
    // wrap_blocking_as_stream -> single chunk z całością odpowiedzi
    // (klient widzi calosc po EOF zamiast token-by-token).
    let flows: &[(&str, &str, &str, &str, i64)] = &[
        (
            "Standardowy pipeline LLM",
            "Streaming pipeline LLM z filtrem PII na odpowiedzi.",
            "chat",
            r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"l1","type":"llm","position":{"x":200,"y":0},"config":{}},{"id":"p1","type":"pii_filter","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"l1"},{"from_node":"l1","to_node":"p1","from_port":"stream"},{"from_node":"p1","to_node":"o1","from_port":"stream"}]}"#,
            1,
        ),
        (
            "Default Chat",
            "Streaming chat pipeline: trigger -> LLM -> pii_filter -> output(stream).",
            "chat",
            r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"l1","type":"llm","position":{"x":200,"y":0},"config":{}},{"id":"p1","type":"pii_filter","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"l1"},{"from_node":"l1","to_node":"p1","from_port":"stream"},{"from_node":"p1","to_node":"o1","from_port":"stream"}]}"#,
            0,
        ),
        (
            "Standardowy pipeline TTS",
            "Prosty pipeline syntezy mowy: czyszczenie tekstu i TTS.",
            "tts",
            r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"c1","type":"tts_clean","position":{"x":200,"y":0},"config":{}},{"id":"s1","type":"tts","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{}}],"edges":[{"from_node":"t1","to_node":"c1"},{"from_node":"c1","to_node":"s1"},{"from_node":"s1","to_node":"o1"}]}"#,
            1,
        ),
        (
            "Audio Chat",
            "Voice conversation streaming: STT (z diarization) -> LLM -> pii_filter -> output(stream). Wlaczany dla audio_input.",
            "chat",
            r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"s1","type":"stt","position":{"x":200,"y":0},"config":{"diarization":true}},{"id":"l1","type":"llm","position":{"x":400,"y":0},"config":{}},{"id":"p1","type":"pii_filter","position":{"x":600,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":800,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"s1"},{"from_node":"s1","to_node":"l1"},{"from_node":"l1","to_node":"p1","from_port":"stream"},{"from_node":"p1","to_node":"o1","from_port":"stream"}]}"#,
            0,
        ),
        (
            "teams-flow",
            "Streaming flow dla teams-bot: trigger -> llm -> pii_filter -> output(stream).",
            "agents",
            r#"{"nodes":[{"id":"t1","type":"trigger","position":{"x":0,"y":0},"config":{}},{"id":"l1","type":"llm","position":{"x":200,"y":0},"config":{"model_alias":"teams-summarization"}},{"id":"p1","type":"pii_filter","position":{"x":400,"y":0},"config":{}},{"id":"o1","type":"output","position":{"x":600,"y":0},"config":{"mode":"stream"}}],"edges":[{"from_node":"t1","to_node":"l1"},{"from_node":"l1","to_node":"p1","from_port":"stream"},{"from_node":"p1","to_node":"o1","from_port":"stream"}]}"#,
            0,
        ),
    ];

    // Migracja seedów: gdy istniejący flow_json NIE zawiera `from_port":"stream"`
    // (czyli to stary blocking seed sprzed Krok 6/7), nadpisz go aktualnym
    // streamingowym wariantem. Custom flows (admin zmienił JSON) zostają
    // nietknięte. Brak rekordu → INSERT.
    let mut update_stmt = conn.prepare(
        "UPDATE flows SET description = ?2, service_type = ?3, flow_json = ?4, \
         is_default = ?5, status = 'active' \
         WHERE name = ?1 AND flow_json NOT LIKE '%\"from_port\":\"stream\"%'",
    )?;
    let mut insert_stmt = conn.prepare(
        "INSERT INTO flows (name, description, service_type, flow_json, status, is_default) \
         SELECT ?1, ?2, ?3, ?4, 'active', ?5 \
         WHERE NOT EXISTS (SELECT 1 FROM flows WHERE name = ?1)",
    )?;

    for (name, description, service_type, flow_json, is_default) in flows {
        // Streaming-aware seedy (chat + agents) — UPDATE legacy blocking
        // wariantu, INSERT jeśli rekord nie istnieje. TTS pozostaje
        // blocking, więc UPDATE nic nie zmieni (LIKE nie matchuje), INSERT
        // wstawi przy fresh DB.
        let migrated = update_stmt.execute(rusqlite::params![
            name,
            description,
            service_type,
            flow_json,
            is_default
        ])?;
        if migrated > 0 {
            tracing::info!(
                "seed: zmigrowano flow '{}' z blocking na streaming variant",
                name
            );
            continue;
        }
        let inserted = insert_stmt.execute(rusqlite::params![
            name,
            description,
            service_type,
            flow_json,
            is_default
        ])?;
        if inserted > 0 {
            debug!("Utworzono domyslny flow: {}", name);
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
        assert_eq!(
            other, 0,
            "nie powinno byc innych promptow niz transcription_summarization"
        );

        let is_system_all: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM prompts WHERE is_system = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(is_system_all, 5);
    }

    /// Swieza baza ma dokladnie 3 domyslne flows: LLM, TTS, teams-flow.
    /// Kazdy ma zdefiniowany DAG trigger -> ... -> output z odpowiednimi nodami.
    #[test]
    fn fresh_db_has_expected_default_flows() {
        let pool = crate::db::init(Path::new(":memory:")).expect("init db");
        let conn = pool.lock().unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM flows", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 5, "oczekiwane 5 domyslnych flows, jest {}", total);

        let names: Vec<String> = conn
            .prepare("SELECT name FROM flows ORDER BY name")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert_eq!(
            names,
            vec![
                "Audio Chat".to_string(),
                "Default Chat".to_string(),
                "Standardowy pipeline LLM".to_string(),
                "Standardowy pipeline TTS".to_string(),
                "teams-flow".to_string(),
            ]
        );

        // Sprawdz kazdy flow strukturalnie.
        let assert_dag = |name: &str, expected_types: &[&str], expected_edges: usize| {
            let (flow_json, service_type, is_default): (String, String, i64) = conn
                .query_row(
                    "SELECT flow_json, service_type, is_default FROM flows WHERE name = ?1",
                    rusqlite::params![name],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            let parsed: serde_json::Value = serde_json::from_str(&flow_json).unwrap();
            let nodes = parsed["nodes"].as_array().unwrap();
            let edges = parsed["edges"].as_array().unwrap();
            assert_eq!(nodes.len(), expected_types.len(), "{}: node count", name);
            assert_eq!(edges.len(), expected_edges, "{}: edge count", name);
            let types: Vec<&str> = nodes.iter().map(|n| n["type"].as_str().unwrap()).collect();
            assert_eq!(types, expected_types, "{}: node types", name);
            (service_type, is_default)
        };

        let (st, def) = assert_dag(
            "Standardowy pipeline LLM",
            &["trigger", "llm", "pii_filter", "output"],
            3,
        );
        // R0a: service_type "llm" zostalo przemianowane na "chat" (chat
        // router dispatchuje z service_type='chat'); seed odzwierciedla
        // ten kontrakt.
        assert_eq!(st, "chat");
        assert_eq!(def, 1);

        let (st, def) = assert_dag(
            "Standardowy pipeline TTS",
            &["trigger", "tts_clean", "tts", "output"],
            3,
        );
        assert_eq!(st, "tts");
        assert_eq!(def, 1);

        let (st, def) = assert_dag(
            "Default Chat",
            &["trigger", "llm", "pii_filter", "output"],
            3,
        );
        assert_eq!(st, "chat");
        assert_eq!(def, 0, "Default Chat nie jest default w db (Standardowy pipeline LLM jest)");

        let (st, def) = assert_dag(
            "Audio Chat",
            &["trigger", "stt", "llm", "pii_filter", "output"],
            4,
        );
        assert_eq!(st, "chat");
        assert_eq!(def, 0, "Audio Chat jest opt-in (wymaga audio_input)");

        let (st, _) = assert_dag("teams-flow", &["trigger", "llm", "pii_filter", "output"], 3);
        assert_eq!(st, "agents");

        // teams-flow: llm node musi miec model_alias = teams-summarization.
        let teams_json: String = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE name = 'teams-flow'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let teams_parsed: serde_json::Value = serde_json::from_str(&teams_json).unwrap();
        let llm_node = teams_parsed["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "llm")
            .unwrap();
        assert_eq!(llm_node["config"]["model_alias"], "teams-summarization");
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
        let fallback =
            crate::db::repository::find_prompt(&pool, "transcription_summarization", "it")
                .unwrap()
                .expect("fallback na pl");
        assert_eq!(fallback.language, "pl");

        // Nieistniejacy prompt -> None
        let none = crate::db::repository::find_prompt(&pool, "does_not_exist", "pl").unwrap();
        assert!(none.is_none());
    }

    /// Kazdy seedowany flow musi przechodzic walidacje AdapterRegistry
    /// (zbudowanej z tym samym zestawem adapterow co FlowDispatcher). Chroni
    /// przed regresja: dodanie node_type do seed'a bez adaptera w dispatcherze
    /// blokowaloby zapis flow przez walidacje dispatch/handlers.rs.
    #[test]
    fn seeded_flows_pass_adapter_validation() {
        use crate::flow_engine::node_adapter::AdapterRegistry;
        use crate::flow_engine::node_adapters::{
            ConditionNodeAdapter, ConversationHistoryNodeAdapter, EmbeddingsNodeAdapter,
            LlmNodeAdapter, MemoryNodeAdapter, OutputNodeAdapter, PiiFilterNodeAdapter,
            SessionContextNodeAdapter, SpeakerContextNodeAdapter, SttNodeAdapter,
            TriggerNodeAdapter, TtsCleanNodeAdapter, TtsNodeAdapter,
        };
        use crate::flow_engine::types::FlowDefinition;
        use crate::flow_engine::validation::validate;
        use std::sync::Arc;

        let pool = crate::db::init(Path::new(":memory:")).expect("init db");

        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(TriggerNodeAdapter::new()));
        registry.register(Arc::new(OutputNodeAdapter::new()));
        registry.register(Arc::new(ConditionNodeAdapter::new()));
        registry.register(Arc::new(PiiFilterNodeAdapter::new()));
        registry.register(Arc::new(TtsCleanNodeAdapter::new()));
        registry.register(Arc::new(SttNodeAdapter::new()));
        registry.register(Arc::new(TtsNodeAdapter::new()));
        registry.register(Arc::new(EmbeddingsNodeAdapter::new()));
        registry.register(Arc::new(MemoryNodeAdapter::new()));
        registry.register(Arc::new(ConversationHistoryNodeAdapter::new()));
        registry.register(Arc::new(SessionContextNodeAdapter::new()));
        registry.register(Arc::new(SpeakerContextNodeAdapter::new()));
        registry.register_llm(Arc::new(LlmNodeAdapter::new()));

        let flow_jsons: Vec<(String, String)> = {
            let conn = pool.lock().unwrap();
            let mut stmt = conn.prepare("SELECT name, flow_json FROM flows").unwrap();
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            rows
        };

        assert!(!flow_jsons.is_empty(), "seed nie wyprodukowal flows");
        for (name, json) in &flow_jsons {
            let parsed: FlowDefinition = serde_json::from_str(json)
                .unwrap_or_else(|e| panic!("flow '{}': nie parsuje: {}", name, e));
            validate(
                &parsed,
                &registry,
                crate::flow_engine::validation::ValidationSource::UserDefined,
            )
                .unwrap_or_else(|e| panic!("flow '{}': walidacja nie przechodzi: {}", name, e));
        }
    }
}
