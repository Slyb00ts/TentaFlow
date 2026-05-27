// ============ File: services/role_catalog/mod.rs — administrowalny katalog rol biznesowych ============
//
// Katalog rol funkcjonalnych w organizacji (Handlowiec / PM techniczny /
// Architekt / Decydent klienta itp.). Rola opisuje KIM ktos jest (atrybuty
// strukturalne), nie CO MOZE ROBIC (akcje per addon — te zyja jako reguly
// w P2 Permissions). Multi-tenant per `org_id`. Pelen i18n od dnia 1
// (name_translations + description_translations jako JSON). Lista wspieranych
// jezykow per-org zyje w `platform_locales` (migracja v40).

pub mod audit;
pub mod error;
pub mod repo;

pub use error::{Result, RoleCatalogError};
pub use repo::{
    create_role, deactivate_role, get_role, get_role_by_slug, list_active_locale_codes,
    list_active_locales, list_roles, search_roles, update_role, RoleCreateInput, RoleListFilter,
    RoleUpdateInput,
};

use std::collections::BTreeMap;

/// Whitelist nazw ikon dozwolonych dla `role_catalog.icon`. Obejmuje pelen
/// zestaw uzywany przez seed migracji v41 plus rezerwowane nazwy dla
/// kolejnych iteracji UI. Lista jest celowo zamknieta — nieznane nazwy
/// blokowane sa w warstwie walidacji aby ikona nie wyciekla w UI jako
/// nieistniejacy glyph.
pub const ALLOWED_ICONS: &[&str] = &[
    "i-briefcase",
    "i-shield",
    "i-users",
    "i-user",
    "i-deal",
    "i-activity",
    "i-calendar",
    "i-receipt",
    "i-folder",
    "i-grid",
    "i-puzzle",
    "i-network",
    "i-star",
    "i-info",
    "i-key",
    "i-clipboard",
    "i-cube",
    "i-headset",
    "i-code",
    "i-bug",
    "i-building",
    "i-chart",
    "i-crown",
    "i-user-check",
    "i-user-cog",
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Role {
    pub id: String,
    pub org_id: String,
    pub slug: String,
    pub kind: RoleKind,
    /// Mapa code -> display name (np. {"pl": "Handlowiec", "en": "Sales Rep"}).
    pub name_translations: BTreeMap<String, String>,
    /// Analogicznie dla opisu. Moze byc pusta gdy admin nie podal opisu.
    pub description_translations: BTreeMap<String, String>,
    pub icon: Option<String>,
    pub color_hint: Option<String>,
    pub is_manager: bool,
    pub default_visibility_scope: VisibilityScope,
    pub is_active: bool,
    /// ISO-8601 UTC.
    pub created_at: String,
    pub updated_at: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RoleKind {
    Sales,
    Technical,
    Management,
    External,
    Other,
}

impl RoleKind {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            RoleKind::Sales => "sales",
            RoleKind::Technical => "technical",
            RoleKind::Management => "management",
            RoleKind::External => "external",
            RoleKind::Other => "other",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self> {
        match s {
            "sales" => Ok(RoleKind::Sales),
            "technical" => Ok(RoleKind::Technical),
            "management" => Ok(RoleKind::Management),
            "external" => Ok(RoleKind::External),
            "other" => Ok(RoleKind::Other),
            other => Err(RoleCatalogError::InvalidKind(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityScope {
    Assigned,
    Own,
    Section,
    Department,
    All,
}

impl VisibilityScope {
    pub fn as_db_str(&self) -> &'static str {
        match self {
            VisibilityScope::Assigned => "assigned",
            VisibilityScope::Own => "own",
            VisibilityScope::Section => "section",
            VisibilityScope::Department => "department",
            VisibilityScope::All => "all",
        }
    }

    pub fn from_db_str(s: &str) -> Result<Self> {
        match s {
            "assigned" => Ok(VisibilityScope::Assigned),
            "own" => Ok(VisibilityScope::Own),
            "section" => Ok(VisibilityScope::Section),
            "department" => Ok(VisibilityScope::Department),
            "all" => Ok(VisibilityScope::All),
            other => Err(RoleCatalogError::InvalidScope(other.to_string())),
        }
    }
}

/// Wpis tabeli `platform_locales` (migracja v40). Uzywany przez warstwe repo
/// do walidacji kompletnosci translacji i serializowany do warstwy wyzszej
/// kiedy admin UI rysuje pickery jezykow.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PlatformLocale {
    pub id: String,
    pub org_id: String,
    pub code: String,
    pub display_name: String,
    pub is_default: bool,
    pub is_active: bool,
}
