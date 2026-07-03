// ============ File: legal/rodo_generator.rs — F2 P8.b RODO PDF generator =====
//
// End-to-end generator for a single RODO/GDPR document:
//   1. validate caller membership in the target org (org isolation invariant),
//   2. load org-scoped metadata (name, address, contact, optional DPO, retention),
//   3. render the matching Handlebars template under strict mode,
//   4. typeset the rendered text into a multi-page A4 PDF (genpdf + DejaVu),
//   5. write the PDF under `<legal_root>/<org_id>/<doc_id>.pdf` with the
//      parent directory containment-checked via canonicalize() + starts_with(),
//   6. blake3-hash the on-disk PDF and persist the row via
//      `db::legal_documents::insert` (UUIDv4 id minted in the repo),
//   7. emit a B-class audit_log row (`legal.generate`).
//
// Error reason strings are static and carry no caller / org input — a denied
// request from org A cannot leak the existence (or non-existence) of resources
// belonging to org B through the error surface.

use std::path::{Component, Path, PathBuf};

use blake3::Hasher;
use handlebars::Handlebars;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::legal_documents::{insert as insert_legal_document, NewLegalDocument};
use crate::services::legal::types::RodoVariant;

// Default retention windows used when the org row does not carry a parsed
// retention policy. Kept as named constants so an ops change is one edit, not
// a hunt through the renderer.
const DEFAULT_RETENTION_FRAMES_DAYS: u32 = 7;
const DEFAULT_RETENTION_RECORDINGS_DAYS: u32 = 30;

// Default data-category list. Polish wording — matches the legal templates.
const DEFAULT_DATA_CATEGORIES: &[&str] = &[
    "wizerunek (obraz twarzy zarejestrowany przez kamerę monitoringu)",
    "sylwetka oraz cechy postury widoczne na obrazie",
    "metadane techniczne: znacznik czasu, identyfikator strumienia, identyfikator kamery",
    "dane kontekstowe zdarzenia: klasa detekcji, lokalizacja kamery, kierunek ruchu",
];

// Default recipient list. `recordings_days` placeholder is rendered into the
// template, so the recipient list itself stays static.
const DEFAULT_RECIPIENTS: &[(&str, &str)] = &[
    (
        "Upoważnieni pracownicy administratora",
        "obsługa systemu monitoringu w zakresie ochrony osób i mienia",
    ),
    (
        "Dostawca usługi hostingu / kolokacji",
        "przechowywanie nagrań na podstawie umowy powierzenia (art. 28 RODO)",
    ),
    (
        "Organy uprawnione na podstawie przepisów prawa",
        "realizacja obowiązków prawnych ciążących na administratorze",
    ),
];

// Embedded DejaVu Sans family — vendored under `assets/fonts/dejavu/` so PDF
// generation does not depend on system fonts (deterministic in CI, in Docker,
// and on operator laptops without DejaVu installed).
const FONT_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/dejavu/DejaVuSans.ttf");
const FONT_BOLD: &[u8] = include_bytes!("../../../assets/fonts/dejavu/DejaVuSans-Bold.ttf");
const FONT_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/dejavu/DejaVuSans-Oblique.ttf");
const FONT_BOLD_ITALIC: &[u8] =
    include_bytes!("../../../assets/fonts/dejavu/DejaVuSans-BoldOblique.ttf");

// Handlebars sources — embedded so the binary is self-contained and the
// strict-mode renderer can never fail because of a missing file on disk.
const TPL_SHORT: &str = include_str!("../../../templates/legal/rodo_short.hbs");
const TPL_STANDARD: &str = include_str!("../../../templates/legal/rodo_standard.hbs");
const TPL_FULL: &str = include_str!("../../../templates/legal/rodo_full.hbs");

#[derive(Debug, Clone)]
pub struct RodoGenerationInput {
    pub org_id: String,
    pub variant: RodoVariant,
    pub generated_by_user_id: String,
}

#[derive(Debug, Clone)]
pub struct RodoGenerationOutput {
    pub doc_id: String,
    pub pdf_path: PathBuf,
    pub content_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RodoGenerationError {
    /// Returned both when the org row does not exist and when the caller is
    /// not a member of the org. The two cases are collapsed on purpose so a
    /// remote caller cannot enumerate org existence via error-shape probing.
    /// The server-side distinction is preserved in a `tracing::warn` line.
    #[error("user not a member of organization")]
    UserNotMember,
    #[error("template render failed")]
    TemplateRender(#[from] handlebars::RenderError),
    #[error("pdf generation failed: {0}")]
    PdfGeneration(String),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("database error")]
    Db(#[from] rusqlite::Error),
    #[error("path traversal blocked")]
    PathTraversal,
}

// --- internal rendering context ---------------------------------------------

#[derive(Debug, Serialize)]
struct RenderContext {
    org: OrgCtx,
    generation: GenerationCtx,
    retention: RetentionCtx,
    data_categories: Vec<String>,
    recipients: Vec<RecipientCtx>,
}

#[derive(Debug, Serialize)]
struct OrgCtx {
    name: String,
    address: String,
    email: String,
    // Optional: empty string is the "absent" sentinel the templates branch on
    // with `{{#if org.dpo_email}}`.
    dpo_email: String,
}

#[derive(Debug, Serialize)]
struct GenerationCtx {
    date: String,
}

#[derive(Debug, Serialize)]
struct RetentionCtx {
    frames_days: u32,
    recordings_days: u32,
}

#[derive(Debug, Serialize)]
struct RecipientCtx {
    name: String,
    purpose: String,
}

#[derive(Debug, Clone)]
struct OrgRow {
    name: String,
    contact_email: Option<String>,
    dpo_contact: Option<String>,
    retention_policy_json: Option<String>,
}

// --- public entrypoint -------------------------------------------------------

pub fn generate(
    conn: &Connection,
    legal_root: &Path,
    input: &RodoGenerationInput,
    now_ms: i64,
) -> Result<RodoGenerationOutput, RodoGenerationError> {
    // a) + c) Load org row AND verify caller membership. Both failures collapse
    // to the same external error (UserNotMember) so a probe cannot tell apart
    // "org does not exist" from "org exists but you are not in it". The
    // server-side `tracing::warn` keeps the operator-visible distinction.
    let org = match load_org(conn, &input.org_id)? {
        Some(row) => row,
        None => {
            tracing::warn!(
                target: "tentaflow::legal::rodo",
                org_id = %input.org_id,
                user_id = %input.generated_by_user_id,
                "rodo generate denied: org not found"
            );
            return Err(RodoGenerationError::UserNotMember);
        }
    };
    if !is_member(conn, &input.org_id, &input.generated_by_user_id)? {
        tracing::warn!(
            target: "tentaflow::legal::rodo",
            org_id = %input.org_id,
            user_id = %input.generated_by_user_id,
            "rodo generate denied: user not member of org"
        );
        return Err(RodoGenerationError::UserNotMember);
    }

    // b) Retention values: parse the optional JSON blob if present, otherwise
    // fall back to the module-level defaults.
    let (frames_days, recordings_days) = parse_retention(org.retention_policy_json.as_deref());

    // Build the render context. Address comes out of the org row name field
    // for the moment — the table does not carry a separate address column yet,
    // so we render the org name twice (header + address) until the schema
    // grows one. dpo_email is empty string when absent so the {{#if}} branch
    // in the full template stays falsey.
    let ctx = RenderContext {
        org: OrgCtx {
            name: org.name.clone(),
            address: org.name.clone(),
            email: org.contact_email.clone().unwrap_or_default(),
            dpo_email: org.dpo_contact.clone().unwrap_or_default(),
        },
        generation: GenerationCtx {
            date: format_iso_date(now_ms),
        },
        retention: RetentionCtx {
            frames_days,
            recordings_days,
        },
        data_categories: DEFAULT_DATA_CATEGORIES
            .iter()
            .map(|s| (*s).to_string())
            .collect(),
        recipients: DEFAULT_RECIPIENTS
            .iter()
            .map(|(n, p)| RecipientCtx {
                name: (*n).to_string(),
                purpose: (*p).to_string(),
            })
            .collect(),
    };

    // d) Pick the template body. e/f) Strict mode rejects missing placeholders
    // — a typo in the template surfaces as a render error, not as silent "".
    let template = match input.variant {
        RodoVariant::Short => TPL_SHORT,
        RodoVariant::Standard => TPL_STANDARD,
        RodoVariant::Full => TPL_FULL,
    };
    let mut hb = Handlebars::new();
    hb.set_strict_mode(true);
    // PDF output is a binary document, not HTML. The default HTML escape
    // would turn legitimate punctuation in org names (e.g. `ACME <Co> & Sons`)
    // into entity references like `&lt;Co&gt; &amp; Sons` baked into the PDF.
    hb.register_escape_fn(handlebars::no_escape);
    let rendered = hb.render_template(template, &ctx)?;

    // g) Lay out the rendered text into an A4 PDF in memory.
    let pdf_bytes = render_pdf(&rendered, input.variant)?;

    // h) Containment-check the output directory BEFORE creating any
    // intermediate dir. `build_pdf_path` validates that `org_id` is a
    // UUIDv4 and that the resolved path is contained under the canonicalized
    // `legal_root`. Only after that check do we materialize the org subdir.
    let pdf_path = build_pdf_path(legal_root, &input.org_id, now_ms)?;
    if let Some(parent) = pdf_path.parent() {
        std::fs::create_dir_all(parent)?;
        secure_dir_permissions(parent)?;
    }
    std::fs::write(&pdf_path, &pdf_bytes)?;

    // i) Hash the on-disk artefact (not the in-memory buffer) so the row
    // commits to the bytes that downloaders will actually receive.
    let content_hash = blake3_hex_of_file(&pdf_path)?;

    // j) Persist. The repo mints UUIDv4 and enforces CHECK constraints on
    // content_hash length + variant string.
    let new_doc = NewLegalDocument {
        org_id: input.org_id.clone(),
        variant: input.variant,
        generated_at: now_ms,
        generated_by_user_id: input.generated_by_user_id.clone(),
        content_hash: content_hash.clone(),
        pdf_path: pdf_path.to_string_lossy().to_string(),
        signed_url_ref: None,
    };
    let doc_id = insert_legal_document(conn, &new_doc).map_err(|e| {
        // Map the anyhow error from the repo onto our typed surface. CHECK /
        // FK violations come back as rusqlite::Error nested in anyhow; we
        // best-effort downcast, otherwise log and surface a generic Db().
        if let Some(rs) = e.downcast_ref::<rusqlite::Error>() {
            return RodoGenerationError::Db(rusqlite::Error::SqliteFailure(
                match rs {
                    rusqlite::Error::SqliteFailure(code, _) => *code,
                    _ => rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                },
                Some(rs.to_string()),
            ));
        }
        RodoGenerationError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(e.to_string()),
        ))
    })?;

    // k) Best-effort audit emit. A failed audit insert must not break the
    // generate flow — the document is already persisted at this point.
    let _ = audit_emit_generate(
        conn,
        &input.org_id,
        &input.generated_by_user_id,
        &doc_id,
        input.variant,
        &content_hash,
        frames_days,
        recordings_days,
    );

    Ok(RodoGenerationOutput {
        doc_id,
        pdf_path,
        content_hash,
    })
}

/// Async wrapper around [`generate`] for use from Tokio handlers.
///
/// The sync version blocks on Handlebars rendering, PDF layout (genpdf), and
/// blocking file I/O — all of which would stall a Tokio worker thread if
/// awaited from `async` code. This wrapper offloads the whole pipeline to a
/// blocking pool via `tokio::task::spawn_blocking`.
///
/// Use [`generate`] directly from CLI / sync entry points where the calling
/// thread is already a blocking-friendly context.
pub async fn generate_async(
    db: crate::db::DbPool,
    legal_root: PathBuf,
    input: RodoGenerationInput,
    now_ms: i64,
) -> Result<RodoGenerationOutput, RodoGenerationError> {
    tokio::task::spawn_blocking(move || {
        let conn = db.write().map_err(|_| {
            RodoGenerationError::Db(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("db pool mutex poisoned".to_string()),
            ))
        })?;
        generate(&conn, &legal_root, &input, now_ms)
    })
    .await
    .map_err(|join_err| {
        RodoGenerationError::PdfGeneration(format!("blocking task join: {join_err}"))
    })?
}

// --- helpers -----------------------------------------------------------------

fn load_org(conn: &Connection, org_id: &str) -> Result<Option<OrgRow>, RodoGenerationError> {
    let row = conn
        .query_row(
            "SELECT name, contact_email, dpo_contact, retention_policy_json \
             FROM organizations WHERE org_id = ?1 AND status != 'deleted'",
            params![org_id],
            |row| {
                Ok(OrgRow {
                    name: row.get(0)?,
                    contact_email: row.get(1)?,
                    dpo_contact: row.get(2)?,
                    retention_policy_json: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn is_member(conn: &Connection, org_id: &str, user_id: &str) -> Result<bool, RodoGenerationError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

fn parse_retention(json_blob: Option<&str>) -> (u32, u32) {
    fn pick(v: &serde_json::Value, key: &str, default: u32) -> u32 {
        let Some(raw) = v.get(key).and_then(|x| x.as_u64()) else {
            return default;
        };
        match u32::try_from(raw) {
            Ok(n) => n,
            Err(_) => {
                tracing::warn!(
                    target: "tentaflow::legal::rodo",
                    key = key,
                    value = raw,
                    "retention value exceeds u32::MAX, falling back to default"
                );
                default
            }
        }
    }
    if let Some(raw) = json_blob {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
            let frames = pick(&v, "frames_days", DEFAULT_RETENTION_FRAMES_DAYS);
            let recordings = pick(&v, "recordings_days", DEFAULT_RETENTION_RECORDINGS_DAYS);
            return (frames, recordings);
        }
    }
    (
        DEFAULT_RETENTION_FRAMES_DAYS,
        DEFAULT_RETENTION_RECORDINGS_DAYS,
    )
}

fn format_iso_date(now_ms: i64) -> String {
    // YYYY-MM-DD only — keep the doc body locale-neutral and stable.
    let secs = now_ms / 1000;
    let dt =
        chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0).unwrap_or_else(chrono::Utc::now);
    dt.format("%Y-%m-%d").to_string()
}

fn build_pdf_path(
    legal_root: &Path,
    org_id: &str,
    now_ms: i64,
) -> Result<PathBuf, RodoGenerationError> {
    // Defence-in-depth: reject any `org_id` that is not a safe slug. The real
    // containment proof is the component walk + `starts_with` below; this is a
    // cheap first gate that rejects anything with a path separator, `.`, or
    // other funny business before we build a path from it. It intentionally
    // accepts BOTH UUIDs and the seeded default org id (`org-default`) — the
    // old UUIDv4-only check hard-failed every non-UUID org, so RODO generation
    // was impossible on a default install.
    if !is_safe_org_id(org_id) {
        return Err(RodoGenerationError::PathTraversal);
    }

    // Ensure the root exists before canonicalize() — canonicalize() errors on
    // a missing path. The org-scoped subdir is intentionally NOT created here:
    // if the containment proof below were to fail (it cannot once `org_id` is
    // UUIDv4, but the check is still made), no stray directory must remain
    // outside the root.
    std::fs::create_dir_all(legal_root)?;
    let root_canon = legal_root.canonicalize()?;

    // Build the candidate path manually and verify it stays under `root_canon`
    // WITHOUT touching the filesystem. We walk every component of the joined
    // path and reject any non-Normal segment (Component::ParentDir / RootDir /
    // Prefix). This proves containment without needing canonicalize() on a
    // path that has not been created yet.
    let nonce = uuid::Uuid::new_v4();
    let relative = PathBuf::from(org_id).join(format!("{}-{}.pdf", now_ms, nonce));
    for c in relative.components() {
        match c {
            Component::Normal(_) => {}
            _ => return Err(RodoGenerationError::PathTraversal),
        }
    }
    let candidate = root_canon.join(&relative);
    if !candidate.starts_with(&root_canon) {
        return Err(RodoGenerationError::PathTraversal);
    }
    Ok(candidate)
}

/// A safe org id for path building: non-empty, bounded, and made only of
/// ASCII alphanumerics plus `-`/`_`. This rejects path separators (`/`, `\`),
/// `.`/`..`, NUL and any other traversal vector, while accepting both UUID org
/// ids and human slugs like `org-default`.
fn is_safe_org_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(unix)]
fn secure_dir_permissions(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(dir)?.permissions();
    perms.set_mode(0o700);
    std::fs::set_permissions(dir, perms)
}

#[cfg(not(unix))]
fn secure_dir_permissions(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

fn blake3_hex_of_file(path: &Path) -> Result<String, RodoGenerationError> {
    let bytes = std::fs::read(path)?;
    let mut hasher = Hasher::new();
    hasher.update(&bytes);
    Ok(hasher.finalize().to_hex().to_string())
}

fn render_pdf(text: &str, _variant: RodoVariant) -> Result<Vec<u8>, RodoGenerationError> {
    use genpdf::fonts::{FontData, FontFamily};
    use genpdf::{elements, style, Document, SimplePageDecorator};

    let family = FontFamily {
        regular: FontData::new(FONT_REGULAR.to_vec(), None)
            .map_err(|e| RodoGenerationError::PdfGeneration(format!("font regular: {e}")))?,
        bold: FontData::new(FONT_BOLD.to_vec(), None)
            .map_err(|e| RodoGenerationError::PdfGeneration(format!("font bold: {e}")))?,
        italic: FontData::new(FONT_ITALIC.to_vec(), None)
            .map_err(|e| RodoGenerationError::PdfGeneration(format!("font italic: {e}")))?,
        bold_italic: FontData::new(FONT_BOLD_ITALIC.to_vec(), None)
            .map_err(|e| RodoGenerationError::PdfGeneration(format!("font bold_italic: {e}")))?,
    };

    let mut doc = Document::new(family);
    doc.set_title("Klauzula informacyjna RODO");
    doc.set_minimal_conformance();
    let mut decorator = SimplePageDecorator::new();
    decorator.set_margins(20);
    doc.set_page_decorator(decorator);

    // Split rendered text into paragraphs on blank lines. Each non-empty block
    // becomes a Paragraph element; genpdf handles wrapping + pagination.
    for block in text.split("\n\n") {
        let trimmed = block.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        // Detect section headings — short lines in ALL CAPS or starting with
        // a digit + period (e.g. "1. ADMINISTRATOR …"). Rendered bold.
        let is_heading = trimmed.lines().count() == 1
            && (trimmed
                .chars()
                .all(|c| c.is_uppercase() || !c.is_alphabetic())
                || trimmed
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false));
        let mut p = elements::Paragraph::default();
        if is_heading {
            p.push_styled(trimmed.replace('\n', " "), style::Effect::Bold);
        } else {
            p.push(trimmed.replace('\n', " "));
        }
        doc.push(p);
        doc.push(elements::Break::new(0.6));
    }

    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    doc.render(&mut buf)
        .map_err(|e| RodoGenerationError::PdfGeneration(format!("render: {e}")))?;
    Ok(buf)
}

#[allow(clippy::too_many_arguments)]
fn audit_emit_generate(
    conn: &Connection,
    org_id: &str,
    user_id: &str,
    doc_id: &str,
    variant: RodoVariant,
    content_hash: &str,
    frames_days: u32,
    recordings_days: u32,
) -> Result<(), rusqlite::Error> {
    // `audit_log.user_id` is INTEGER but the RODO subsystem identifies callers
    // by TEXT user_id (`org_memberships.user_id`). Same pattern as
    // `dispatch::camera_admin::audit_row`: the INTEGER column stays NULL for
    // TEXT-keyed actors, and the actor string is preserved inside the
    // `details` JSON so audit consumers can still query by user.
    let details = serde_json::json!({
        "variant": variant.as_str(),
        "doc_id": doc_id,
        "content_hash": content_hash,
        "retention_frames_days": frames_days,
        "retention_recordings_days": recordings_days,
        "user_id": user_id,
    })
    .to_string();

    // Compute the Merkle chain pair the same way `audit_log_with_risk` and
    // `repository::log_audit_full` do — every audit row in the system must
    // participate in the F1b P4 hash chain.
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let action = "legal.generate";
    let resource_type = Some("legal_document");
    let resource_id = Some(doc_id);
    let result = Some("ok");
    let severity = Some("info");
    let risk_class = "B";
    let hash_input = AuditRowHashInput {
        user_id: None,
        addon_id: None,
        instance_id: None,
        action,
        resource: None,
        resource_type,
        resource_id,
        result,
        error_message: None,
        details: Some(details.as_str()),
        ip_address: None,
        node_id: None,
        severity,
        risk_class,
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = compute_chain_for_insert(conn, &hash_input)?;
    conn.execute(
        "INSERT INTO audit_log \
            (timestamp, action, resource_type, resource_id, result, \
             severity, risk_class, details, org_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            timestamp,
            action,
            resource_type,
            resource_id,
            result,
            severity,
            risk_class,
            details,
            org_id,
            prev_hash,
            hash,
        ],
    )?;
    Ok(())
}

// --- tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    // Stable UUIDv4 used across tests in place of the legacy `org-default`
    // string. The `organizations` row seeded by migrations is patched to this
    // id so the on-disk path layout (which now requires UUIDv4) stays valid.
    const TEST_ORG_ID: &str = "11111111-1111-4111-8111-111111111111";
    const TEST_USER_ID: &str = "u-test";

    fn open_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).expect("run migrations");
        // Seed a dedicated UUIDv4 org (path containment requires UUIDv4) instead
        // of renaming the migration-seeded `org-default`: 24 child tables FK the
        // org PK, so an in-place rename would trip foreign-key enforcement.
        seed_org(&conn, TEST_ORG_ID, "test-org");
        seed_membership(&conn, TEST_ORG_ID, TEST_USER_ID);
        conn
    }

    fn seed_membership(conn: &Connection, org_id: &str, user_id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships \
                (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-org-admin', '2026-01-01T00:00:00Z', 'system')",
            params![org_id, user_id],
        )
        .unwrap();
    }

    fn seed_org(conn: &Connection, id: &str, slug: &str) {
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
             VALUES (?1, ?2, ?3, 'office@example.test', NULL, NULL, 'active', '2026-01-01T00:00:00Z')",
            params![id, id, slug],
        )
        .unwrap();
    }

    fn tmp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn generate_short_happy_path() {
        let conn = open_db();
        let root = tmp_root();
        let out = generate(
            &conn,
            root.path(),
            &RodoGenerationInput {
                org_id: TEST_ORG_ID.into(),
                variant: RodoVariant::Short,
                generated_by_user_id: TEST_USER_ID.into(),
            },
            1_700_000_000_000,
        )
        .expect("generate");
        assert!(out.pdf_path.exists());
        let bytes = std::fs::read(&out.pdf_path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        // doc_id is a UUIDv4 minted by the repo.
        assert_eq!(out.doc_id.len(), 36);
        // content_hash is blake3 hex of 32 bytes => 64 lowercase hex chars.
        assert_eq!(out.content_hash.len(), 64);
        assert!(out
            .content_hash
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn generate_rejects_non_member() {
        let conn = open_db();
        let other_org = "22222222-2222-4222-8222-222222222222";
        seed_org(&conn, other_org, "other");
        let root = tmp_root();
        let err = generate(
            &conn,
            root.path(),
            &RodoGenerationInput {
                org_id: other_org.into(),
                variant: RodoVariant::Short,
                generated_by_user_id: TEST_USER_ID.into(),
            },
            1_700_000_000_000,
        )
        .expect_err("must reject non-member");
        assert!(matches!(err, RodoGenerationError::UserNotMember));
    }

    #[test]
    fn generate_rejects_missing_org_as_non_member() {
        // Missing org + non-member collapse to the same external error so a
        // remote caller cannot enumerate org existence.
        let conn = open_db();
        let root = tmp_root();
        let err = generate(
            &conn,
            root.path(),
            &RodoGenerationInput {
                org_id: "33333333-3333-4333-8333-333333333333".into(),
                variant: RodoVariant::Standard,
                generated_by_user_id: TEST_USER_ID.into(),
            },
            1_700_000_000_000,
        )
        .expect_err("must reject missing org");
        assert!(matches!(err, RodoGenerationError::UserNotMember));
    }

    #[test]
    fn path_traversal_org_id_cannot_escape() {
        // A non-UUIDv4 `org_id` must be refused by the path builder before
        // the row is even consulted — defence in depth in case a future
        // schema migration drops the org_id format constraint.
        let res = build_pdf_path(
            std::env::temp_dir().as_path(),
            "../escape",
            1_700_000_000_000,
        );
        assert!(matches!(res, Err(RodoGenerationError::PathTraversal)));
    }

    #[test]
    fn path_traversal_creates_no_dirs_outside_root() {
        // After MED2 the org subdir must NOT be created when the org_id fails
        // the containment / UUIDv4 check. We run inside a sandbox so the
        // sibling that a traversal would target sits next to a brand-new
        // root and is observable to belong to this test only.
        let sandbox = tempfile::tempdir().expect("sandbox tempdir");
        let root_path = sandbox.path().join("legal-root");
        std::fs::create_dir_all(&root_path).unwrap();

        // Bad org_id with traversal segments — must be rejected up-front.
        let res = build_pdf_path(&root_path, "../escape", 1_700_000_000_000);
        assert!(matches!(res, Err(RodoGenerationError::PathTraversal)));
        // The would-be traversal target `<sandbox>/escape` must not exist —
        // nothing in this test touched it.
        let leaked = sandbox.path().join("escape");
        assert!(
            !leaked.exists(),
            "traversal target was created outside root: {:?}",
            leaked
        );

        // An org_id with a separator/dot is unsafe and rejected up-front — no
        // dir made inside the root either. (A plain non-UUID *slug* like
        // `org-default` is now allowed — that is the default org and must be
        // able to generate documents.)
        let res2 = build_pdf_path(&root_path, "bad.slug/x", 1_700_000_000_000);
        assert!(matches!(res2, Err(RodoGenerationError::PathTraversal)));
        let inside = root_path.join("bad.slug");
        assert!(
            !inside.exists(),
            "rejected org subdir was still created: {:?}",
            inside
        );

        // The root itself stays empty — no canonicalize side-effect created
        // any org subdir.
        let entries: Vec<_> = std::fs::read_dir(&root_path)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert!(
            entries.is_empty(),
            "legal_root should still be empty after rejected paths, got {:?}",
            entries
        );
    }

    #[test]
    fn default_org_slug_is_accepted() {
        // Regression: the seeded default org id is `org-default` (not a UUID).
        // It must build a path CONTAINED in the root, otherwise RODO document
        // generation is impossible on a default install.
        let sandbox = tempfile::tempdir().expect("sandbox tempdir");
        let root_path = sandbox.path().join("legal-root");
        let candidate = build_pdf_path(&root_path, "org-default", 1_700_000_000_000)
            .expect("org-default must be a valid org id");
        let root_canon = root_path.canonicalize().unwrap();
        assert!(
            candidate.starts_with(&root_canon),
            "candidate {:?} escaped root {:?}",
            candidate,
            root_canon
        );
    }

    #[test]
    fn content_hash_is_blake3_64_lowercase_hex() {
        let conn = open_db();
        let root = tmp_root();
        let out = generate(
            &conn,
            root.path(),
            &RodoGenerationInput {
                org_id: TEST_ORG_ID.into(),
                variant: RodoVariant::Standard,
                generated_by_user_id: TEST_USER_ID.into(),
            },
            1_700_000_000_000,
        )
        .unwrap();
        assert_eq!(out.content_hash.len(), 64);
        for c in out.content_hash.chars() {
            assert!(c.is_ascii_hexdigit());
            assert!(!c.is_ascii_uppercase());
        }
        // Hash must match a fresh blake3 of the file on disk.
        let recomputed = blake3_hex_of_file(&out.pdf_path).unwrap();
        assert_eq!(out.content_hash, recomputed);
    }

    #[test]
    fn strict_mode_rejects_missing_placeholder() {
        // Independent of the generator pipeline: render a template that
        // references an undefined variable under strict mode and verify it
        // surfaces as a render error.
        let mut hb = Handlebars::new();
        hb.set_strict_mode(true);
        let ctx = serde_json::json!({ "org": { "name": "X" } });
        let res = hb.render_template("{{org.foo}}", &ctx);
        assert!(res.is_err(), "strict mode must reject missing placeholder");
    }

    #[test]
    fn dpo_section_skipped_when_empty() {
        // Default org has no DPO contact. Render the full variant and verify
        // the "INSPEKTOR OCHRONY DANYCH" heading is absent (the {{#if}} block
        // evaluated to false because dpo_email was empty).
        let conn = open_db();
        let root = tmp_root();
        let out = generate(
            &conn,
            root.path(),
            &RodoGenerationInput {
                org_id: TEST_ORG_ID.into(),
                variant: RodoVariant::Full,
                generated_by_user_id: TEST_USER_ID.into(),
            },
            1_700_000_000_000,
        )
        .unwrap();
        // Re-render the template ourselves with the same context shape to
        // assert on the text — the PDF bytes are opaque to grep.
        let mut hb = Handlebars::new();
        hb.set_strict_mode(true);
        let rendered = hb
            .render_template(
                TPL_FULL,
                &serde_json::json!({
                    "org": {
                        "name": "X", "address": "X", "email": "x@y", "dpo_email": ""
                    },
                    "generation": { "date": "2026-05-18" },
                    "retention": { "frames_days": 7u32, "recordings_days": 30u32 },
                    "data_categories": ["a"],
                    "recipients": [{ "name": "n", "purpose": "p" }],
                }),
            )
            .unwrap();
        assert!(
            !rendered.contains("INSPEKTOR OCHRONY DANYCH"),
            "DPO section must be skipped when dpo_email is empty"
        );
        assert!(out.pdf_path.exists());
    }
}
