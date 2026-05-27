// ============ File: services/role_catalog/error.rs — typy bledow katalogu rol ============
//
// Bledy zwracane przez warstwe repo + audit dla katalogu rol biznesowych.
// `DbError` enkapsuluje rusqlite + lock poison; pozostale warianty maja jasna
// semantyke walidacyjna i przekladaja sie 1:1 na komunikaty UI / API.

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoleCatalogError {
    #[error("role not found: {0}")]
    NotFound(String),

    #[error("role slug already exists in org {org_id}: {slug}")]
    SlugConflict { org_id: String, slug: String },

    #[error("invalid slug '{0}' (must match [a-z][a-z0-9_]*, max 50 chars)")]
    InvalidSlug(String),

    #[error("invalid role kind: '{0}'")]
    InvalidKind(String),

    #[error("invalid visibility scope: '{0}'")]
    InvalidScope(String),

    /// name_translations brakuje wpisu dla locale wymaganych w platform_locales.
    #[error("missing translation for required locale(s): {missing:?} (required: {required:?})")]
    MissingTranslations {
        required: Vec<String>,
        missing: Vec<String>,
    },

    /// Wartosc translacji jest pusta (whitespace-only liczy sie jako puste).
    #[error("empty translation value for locale '{locale}' in field '{field}'")]
    EmptyTranslation { locale: String, field: String },

    /// Blad parsowania JSON kolumn `name_translations` / `description_translations`.
    #[error("invalid translations JSON: {0}")]
    InvalidJson(String),

    /// Ikona spoza zatwierdzonej listy (tf-* icon library).
    #[error("unknown icon name: '{0}' (must be from tf-* icon library)")]
    UnknownIcon(String),

    /// `color_hint` nie pasuje do dozwolonego wzorca.
    #[error("invalid color hint: '{0}' (expected #rrggbb or --css-var-name)")]
    InvalidColorHint(String),

    /// W `platform_locales` brak aktywnych wpisow dla danego `org_id`.
    #[error("no active platform_locales found for org_id={0}")]
    NoActiveLocales(String),

    #[error("role catalog DB error: {0}")]
    DbError(String),
}

pub type Result<T> = std::result::Result<T, RoleCatalogError>;
