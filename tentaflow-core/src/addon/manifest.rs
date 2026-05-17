// =============================================================================
// Plik: addon/manifest.rs
// Opis: Rozszerzone sekcje manifestu addona (storage, alias, gate,
//       vector_namespace, flow_template, ui_component, gpu) + walidacja
//       cross-sekcyjna (duplikaty id, enum-y, sygnatury Ed25519, semver).
//       Parsing TOML zywie w addon::lifecycle::parse_manifest_toml — ten
//       modul dostarcza typy + validate_manifest_extensions().
// =============================================================================

use anyhow::{bail, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::OnceLock;

// =============================================================================
// Stale walidacyjne
// =============================================================================

/// Dozwolone wartosci `[storage].sql_dialect`.
pub const STORAGE_SQL_DIALECTS: &[&str] = &["ansi", "sqlite", "postgres"];

/// Dozwolone wartosci `[storage].sql_backends[*]`. `postgres` jest deklaratywnie
/// dopuszczalny w manifescie juz w F1a, ale runtime obsluguje go dopiero F8.
pub const STORAGE_SQL_BACKENDS: &[&str] = &["sqlite", "postgres"];

/// Dozwolone wartosci `[storage].encryption`.
pub const STORAGE_ENCRYPTION_MODES: &[&str] = &["none", "at-rest"];

/// Dozwolone wartosci `[[vector_namespace]].distance`.
pub const VECTOR_DISTANCES: &[&str] = &["cosine", "euclidean", "dot"];

/// Dozwolone wartosci `[[vector_namespace]].data_class`.
pub const VECTOR_DATA_CLASSES: &[&str] = &["A", "B", "C"];

/// Dozwolone wartosci `[[ui_component]].risk`.
pub const UI_COMPONENT_RISK_LEVELS: &[&str] = &["low", "medium", "high"];

/// Dozwolone typy claims dla `[[gate]].required_claims[*].type`.
pub const CLAIM_TYPES: &[&str] = &["approval", "grant", "deployment_profile", "consent"];

/// Regex sygnatury Ed25519 bundli UI: `ed25519:<base64>`.
/// Ed25519 signature = 64 bajty raw → base64 z paddingiem `==` to dokladnie
/// 88 znakow (86 base64 + 2 padding). Strict — nie akceptujemy innej dlugosci.
fn signature_regex() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^ed25519:[A-Za-z0-9+/]{86}==$").expect("regex stale poprawny")
    })
}

/// Placeholder z draft manifestu (sekcja 5 planu TentaVision). Pozwalamy na niego
/// w F1a tylko po to, by przyklad z `notes/tentavision-plan.md` parsowal sie 1:1
/// — narzedzia packaging w F1c podmieniaja go na rzeczywista sygnature przed
/// pakowaniem addona do dystrybucji.
const SIGNATURE_PLACEHOLDER: &str = "ed25519:<base64-signature-placeholder>";

// =============================================================================
// Sekcja [storage]
// =============================================================================

/// Konfiguracja warstwy storage addona. KV (per-addon namespace w hostowej bazie)
/// jest domyslnie wlaczony; SQL wymaga jawnej deklaracji backendow i dialektu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Czy addon uzywa key-value store (host_functions::storage_*). Default true.
    #[serde(default = "default_true")]
    pub kv: bool,

    /// Czy addon uzywa per-addon SQL bazy danych (host_functions::sql_*).
    /// Default false. Wymagane: jesli true → `sql_backends` niepuste.
    #[serde(default)]
    pub sql: bool,

    /// Lista dopuszczalnych backendow SQL. Wymagane gdy `sql=true`.
    /// Dozwolone wartosci: zob. STORAGE_SQL_BACKENDS.
    #[serde(default)]
    pub sql_backends: Vec<String>,

    /// Dialekt SQL stosowany przez addon w migracjach i zapytaniach.
    /// Default "ansi" (przenosny miedzy SQLite/Postgres).
    #[serde(default = "default_sql_dialect")]
    pub sql_dialect: String,

    /// Katalog migracji (sciezka wzgledna do katalogu addona). Default "migrations".
    #[serde(default = "default_migrations_dir")]
    pub migrations_dir: String,

    /// Tryb szyfrowania storage: "none" lub "at-rest". Default "none".
    #[serde(default = "default_encryption")]
    pub encryption: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            kv: true,
            sql: false,
            sql_backends: Vec::new(),
            sql_dialect: default_sql_dialect(),
            migrations_dir: default_migrations_dir(),
            encryption: default_encryption(),
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_sql_dialect() -> String {
    "ansi".to_string()
}
fn default_migrations_dir() -> String {
    "migrations".to_string()
}
fn default_encryption() -> String {
    "none".to_string()
}

// =============================================================================
// Sekcja [[alias]]
// =============================================================================

/// Zakres widocznosci aliasu dla addonow konsumenckich (F1a §6.6 v0.6.0).
/// Domyslnie `Private` — tylko addon-wlasciciel moze zaresolveowac alias.
/// `Restricted` wymaga jawnego whitelisty w `allowed_consumers`.
/// `Public` dopuszcza kazdego addona z wpisem w `addon_uses_alias`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasVisibility {
    Private,
    Restricted,
    Public,
}

impl Default for AliasVisibility {
    fn default() -> Self {
        AliasVisibility::Private
    }
}

impl AliasVisibility {
    /// Postac przechowywana w `model_alias_visibility.visibility`.
    pub fn as_db_str(self) -> &'static str {
        match self {
            AliasVisibility::Private => "private",
            AliasVisibility::Restricted => "restricted",
            AliasVisibility::Public => "public",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "private" => Ok(AliasVisibility::Private),
            "restricted" => Ok(AliasVisibility::Restricted),
            "public" => Ok(AliasVisibility::Public),
            _ => bail!("invalid alias visibility '{}' (allowed: private/restricted/public)", s),
        }
    }
}

/// Deklaracja aliasu AI wystawianego przez addon. Przy instalacji rdzen tworzy
/// rekord w globalnej tabeli `model_aliases` (lub reaktywuje istniejacy). Jesli
/// `suggested_default` jest pusty, alias powstaje jako `is_active=0` az do
/// momentu, kiedy admin podepnie konkretny model/service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AliasSpec {
    /// Globalnie unikalne id aliasu (np. "tentavision-yolo").
    pub id: String,
    /// Nazwa wyswietlana w UI M16 (Services → Aliasy).
    pub display_name: String,
    /// Metody, ktore alias obsluguje (np. ["detect", "track"]).
    #[serde(default)]
    pub methods: Vec<String>,
    /// Sugerowany domyslny model/service do podpiecia. Moze byc pusty.
    #[serde(default)]
    pub suggested_default: String,
    /// Opcjonalny gate (id z `[[gate]]`); alias pozostaje nieaktywny dopoki
    /// gate nie jest zaspokojony (sprawdzane przez policy engine z F2).
    #[serde(default)]
    pub gate: Option<String>,
    /// Zakres widocznosci dla konsumentow. Default `Private`.
    #[serde(default)]
    pub visibility: AliasVisibility,
    /// Whitelist consumer addon ids — wymagana niepusta dla `visibility="restricted"`,
    /// musi byc pusta dla `private`/`public`.
    #[serde(default)]
    pub allowed_consumers: Vec<String>,
}

/// Deklaracja `[[uses_alias]]` — addon zaglasza, ze chce uzywac aliasu o danej
/// nazwie. Alias moze byc owned przez ten sam addon lub inny (cross-addon).
/// Przy install zapisywane do `addon_uses_alias`; status zalezy od visibility
/// owner-aliasu i obecnosci wpisu w `model_alias_consumers`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsesAliasSpec {
    /// Globalna nazwa aliasu (matchuje `model_aliases.alias`).
    pub id: String,
    /// `true` = brak grantu blokuje install. Default `false`.
    #[serde(default)]
    pub required: bool,
    /// Wymagane uzasadnienie biznesowe (dla audytu i UI install wizarda).
    #[serde(default)]
    pub reason: String,
}

/// Analogiczna deklaracja `[[uses_model]]` — bezposredni dostep do modelu
/// (rzadziej niz alias). `id` to free-form model_id (no FK do `models`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsesModelSpec {
    pub id: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub reason: String,
}

// =============================================================================
// Sekcja [[gate]]
// =============================================================================

/// Pojedynczy wymagany claim w definicji gate. Struktura jest celowo
/// odluzniona — kazdy claim ma `type`, reszta pol jest opcjonalna i
/// interpretowana przez policy engine F2 (subject/scope/oneof/...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRequirement {
    /// Typ claim: zob. CLAIM_TYPES.
    #[serde(rename = "type")]
    pub claim_type: String,

    /// Subject (np. "dpia", "fria") — dla type="approval".
    #[serde(default)]
    pub subject: Option<String>,

    /// Scope (np. "biometric:historical") — dla type="grant"/"consent".
    #[serde(default)]
    pub scope: Option<String>,

    /// Status (np. "signed") — dla type="approval".
    #[serde(default)]
    pub status: Option<String>,

    /// Konkretna wartosc (np. nazwa profilu) — uzywane przez type="deployment_profile".
    #[serde(default)]
    pub value: Option<String>,

    /// Lista dopuszczalnych wartosci alternatywnych (np. ["lea","critical_infra"]).
    #[serde(default)]
    pub oneof: Vec<String>,

    /// Czy claim musi byc aktualnie wazny (default: implikowane przez policy engine).
    #[serde(default)]
    pub valid: Option<bool>,

    /// Czy claim musi miec ustawione `expires_at` (claim z hard expiry).
    #[serde(default)]
    pub has_expiry: Option<bool>,
}

/// Bramka prawno-biznesowa zdefiniowana przez addon. Lista `required_claims`
/// jest interpretowana przez policy engine (F2) — F1a tylko parsuje i waliduje
/// strukturalnie. Aliasy/permissions/komponenty UI moga referowac gate przez `gate`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateSpec {
    /// Globalnie unikalne id gate (np. "d4-historical").
    pub id: String,
    /// Nazwa wyswietlana w UI (opcjonalna).
    #[serde(default)]
    pub display_name: String,
    /// Lista wymagan claimow.
    #[serde(default)]
    pub required_claims: Vec<ClaimRequirement>,
}

// =============================================================================
// Sekcja [[vector_namespace]]
// =============================================================================

/// Deklaracja namespace wektorowego addona. F1a parsuje i przechowuje, ale
/// vector API (vector_upsert/search) jest stubem do F1c/F2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorNamespaceSpec {
    /// Nazwa namespace (np. "faces", "attributes"). Unikalna w obrebie addona.
    pub name: String,
    /// Wymiar wektora.
    pub dimensions: u32,
    /// Metryka: zob. VECTOR_DISTANCES.
    pub distance: String,
    /// Klasa danych RODO: "A" / "B" / "C". Wplywa na audyt i retention.
    pub data_class: String,
    /// Opcjonalny gate ograniczajacy uzycie namespace (np. d4-historical dla "faces").
    #[serde(default)]
    pub gate: Option<String>,
}

// =============================================================================
// Sekcja [[flow_template]]
// =============================================================================

/// Szablon Flow dostarczany przez addon (opt-in install — admin moze go
/// zaimportowac do flow-engine po instalacji addona).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowTemplateSpec {
    /// Globalnie unikalne id template (np. "tv-realtime-adr").
    pub id: String,
    /// Nazwa wyswietlana w UI install wizard.
    pub display_name: String,
    /// Sciezka wzgledna do pliku `.flow.json` w katalogu addona.
    pub path: String,
    /// Krotki opis dla admina.
    #[serde(default)]
    pub description: String,
}

// =============================================================================
// Sekcja [[ui_component]]
// =============================================================================

/// Custom komponent UI dostarczany przez addon. `signature` to Ed25519 podpis
/// (`ed25519:<base64>`) nad bundle JS, weryfikowany przez rdzen przy instalacji
/// (narzedzia packaging w F1c). `risk` decyduje o modelu sandboxowania:
/// `low/medium` ladowane jako shadow DOM, `high` jako iframe sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiComponentSpec {
    /// Globalnie unikalne id komponentu (np. "tv-video-grid").
    pub id: String,
    /// Nazwa wyswietlana.
    pub display_name: String,
    /// Slot UI ("main", "sidebar", ...).
    pub slot: String,
    /// Sciezka do bundla JS, wzgledem katalogu addona.
    pub src: String,
    /// Sygnatura Ed25519 bundla, format `ed25519:<base64>`.
    pub signature: String,
    /// Poziom ryzyka: zob. UI_COMPONENT_RISK_LEVELS.
    pub risk: String,
    /// Permisje hosta wymagane przez komponent UI do wywolan host-functions
    /// z iframe via postMessage bridge (F1c P1+). Musi byc podzbiorem permisji
    /// zadeklarowanych w `[[permission]]` addona. Pusta lista = komponent
    /// czysto prezentacyjny (zero wywolan hosta). Auto-derived check: kazda
    /// akcja bridge jest zmapowana na wymagany scope; brak scope -> EPERM.
    #[serde(default)]
    pub host_permissions: Vec<String>,
}

// =============================================================================
// Sekcja [publisher]
// =============================================================================

/// Deklaracja wydawcy addona — Ed25519 public key + display name. Obecnosc
/// `[publisher]` w manifescie oznacza, ze addon dostarcza zaufane bundle
/// UI: kazdy `[[ui_component]]` musi miec `signature` weryfikowalny tym
/// kluczem, a klucz musi byc w trust store (`trusted_publishers` v26).
///
/// Brak `[publisher]` jest dozwolony tylko gdy addon nie deklaruje
/// `[[ui_component]]` (czysty backend) — w przeciwnym razie install jest
/// odrzucany. Polityka default-deny: pusta tabela trust = zaden zewnetrzny
/// addon z UI nie zainstaluje sie, dopoki admin nie wykona
/// `tentaflow-cli addon trust-key <pk> --label "..."`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherInfo {
    /// Ed25519 public key (32 bajty raw → 44 znakow base64 z paddingiem `=`).
    pub ed25519_public_key: String,
    /// Czytelna nazwa wyswietlana w admin UI / install wizard
    /// (np. "TentaFlow Inc", "ACME Sp. z o.o.").
    pub label: String,
    /// Opcjonalny kanal kontaktu (email lub URL) — pokazywany przy
    /// pytaniu o zaufanie nowego wydawcy.
    #[serde(default)]
    pub contact: Option<String>,
}

/// Format Ed25519 pub key w manifescie i trust store: standard base64,
/// 32 bajty raw → 44 znaki (43 base64 + 1 padding `=`).
pub fn validate_publisher_pk_b64(pk_b64: &str) -> Result<()> {
    if pk_b64.len() != 44 {
        bail!(
            "publisher.ed25519_public_key '{}' has invalid length {} (expected 44 base64 chars for 32-byte key)",
            pk_b64,
            pk_b64.len()
        );
    }
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(pk_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("publisher.ed25519_public_key not valid base64: {}", e))?;
    if raw.len() != 32 {
        bail!(
            "publisher.ed25519_public_key decoded to {} bytes (expected 32 for Ed25519)",
            raw.len()
        );
    }
    Ok(())
}

// =============================================================================
// Sekcja [gpu] (info-only)
// =============================================================================

/// Wskazowki adminowi dot. wymagan GPU. Czysto informacyjne — rdzen nie blokuje
/// instalacji na podstawie tych pol, ale UI install wizard moze ostrzegac.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GpuInfo {
    /// Zalecana ilosc VRAM w MB do plynnego dzialania pelnego profilu.
    #[serde(default)]
    pub recommended_vram_mb: Option<u32>,
    /// Dodatkowe uwagi (np. "D4 wymaga osobnego node-a").
    #[serde(default)]
    pub notes: Option<String>,
}

// =============================================================================
// Walidacja rozszerzen manifestu
// =============================================================================

/// Sprawdza wewnetrzna spojnosc nowych sekcji manifestu:
/// - duplikaty id w `[[alias]]` / `[[gate]]` / `[[vector_namespace]]` /
///   `[[flow_template]]` / `[[ui_component]]`,
/// - dozwolone wartosci enumow (`storage.sql_dialect`, `sql_backends`,
///   `encryption`, `distance`, `data_class`, `ui_component.risk`, `claim.type`),
/// - obecnosc `sql_backends` gdy `storage.sql=true`,
/// - format `signature` (`^ed25519:<86 base64 chars>==$` = 64 bajty Ed25519,
///   lub jawny placeholder `ed25519:<base64-signature-placeholder>` z draftu),
/// - `sdk_version` jako prawidlowy semver `VersionReq` jesli podany.
pub fn validate_manifest_extensions(
    storage: Option<&StorageConfig>,
    aliases: &[AliasSpec],
    gates: &[GateSpec],
    vector_namespaces: &[VectorNamespaceSpec],
    flow_templates: &[FlowTemplateSpec],
    ui_components: &[UiComponentSpec],
    sdk_version: Option<&str>,
    uses_aliases: &[UsesAliasSpec],
    uses_models: &[UsesModelSpec],
    publisher: Option<&PublisherInfo>,
) -> Result<()> {
    if let Some(cfg) = storage {
        validate_storage(cfg)?;
    }
    check_unique_ids("alias", aliases.iter().map(|a| a.id.as_str()))?;
    check_unique_ids("uses_alias", uses_aliases.iter().map(|u| u.id.as_str()))?;
    check_unique_ids("uses_model", uses_models.iter().map(|u| u.id.as_str()))?;
    for alias in aliases {
        match alias.visibility {
            AliasVisibility::Restricted => {
                if alias.allowed_consumers.is_empty() {
                    bail!(
                        "alias '{}' has visibility='restricted' but allowed_consumers is empty",
                        alias.id
                    );
                }
            }
            AliasVisibility::Private | AliasVisibility::Public => {
                if !alias.allowed_consumers.is_empty() {
                    bail!(
                        "alias '{}' has visibility='{}' so allowed_consumers must be empty (got {} entries)",
                        alias.id,
                        alias.visibility.as_db_str(),
                        alias.allowed_consumers.len()
                    );
                }
            }
        }
        let mut consumer_seen: HashSet<&str> = HashSet::new();
        for entry in &alias.allowed_consumers {
            let trimmed = entry.trim();
            if trimmed.is_empty() {
                bail!(
                    "alias '{}' has empty/whitespace entry in allowed_consumers",
                    alias.id
                );
            }
            if trimmed.len() != entry.len() {
                bail!(
                    "alias '{}' allowed_consumers entry '{}' has leading/trailing whitespace",
                    alias.id,
                    entry.escape_debug()
                );
            }
            if entry.len() > 64 {
                bail!(
                    "alias '{}' allowed_consumers entry '{}' exceeds 64 chars",
                    alias.id,
                    entry
                );
            }
            if !entry
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                bail!(
                    "alias '{}' allowed_consumers entry '{}' contains invalid characters (allowed: [a-z0-9_-])",
                    alias.id,
                    entry
                );
            }
            if !consumer_seen.insert(entry.as_str()) {
                bail!(
                    "alias '{}' has duplicate consumer '{}' in allowed_consumers",
                    alias.id,
                    entry
                );
            }
        }
    }
    for u in uses_aliases {
        validate_uses_target_id("[[uses_alias]]", &u.id)?;
        if u.required && u.reason.trim().is_empty() {
            bail!("[[uses_alias]] id='{}' is required=true but reason is empty", u.id);
        }
    }
    for u in uses_models {
        validate_uses_target_id("[[uses_model]]", &u.id)?;
        if u.required && u.reason.trim().is_empty() {
            bail!("[[uses_model]] id='{}' is required=true but reason is empty", u.id);
        }
    }
    check_unique_ids("gate", gates.iter().map(|g| g.id.as_str()))?;
    check_unique_ids(
        "vector_namespace",
        vector_namespaces.iter().map(|v| v.name.as_str()),
    )?;
    check_unique_ids(
        "flow_template",
        flow_templates.iter().map(|f| f.id.as_str()),
    )?;
    check_unique_ids(
        "ui_component",
        ui_components.iter().map(|u| u.id.as_str()),
    )?;

    for vns in vector_namespaces {
        if !VECTOR_DISTANCES.contains(&vns.distance.as_str()) {
            bail!(
                "vector_namespace '{}' has invalid distance '{}' (allowed: {:?})",
                vns.name,
                vns.distance,
                VECTOR_DISTANCES
            );
        }
        if !VECTOR_DATA_CLASSES.contains(&vns.data_class.as_str()) {
            bail!(
                "vector_namespace '{}' has invalid data_class '{}' (allowed: {:?})",
                vns.name,
                vns.data_class,
                VECTOR_DATA_CLASSES
            );
        }
        if vns.dimensions == 0 {
            bail!("vector_namespace '{}' has dimensions=0", vns.name);
        }
    }

    for uic in ui_components {
        if !UI_COMPONENT_RISK_LEVELS.contains(&uic.risk.as_str()) {
            bail!(
                "ui_component '{}' has invalid risk '{}' (allowed: {:?})",
                uic.id,
                uic.risk,
                UI_COMPONENT_RISK_LEVELS
            );
        }
        if uic.signature != SIGNATURE_PLACEHOLDER && !signature_regex().is_match(&uic.signature) {
            bail!(
                "ui_component '{}' has invalid signature format (expected 'ed25519:<base64>')",
                uic.id
            );
        }
    }

    for gate in gates {
        for claim in &gate.required_claims {
            if !CLAIM_TYPES.contains(&claim.claim_type.as_str()) {
                bail!(
                    "gate '{}' has invalid claim type '{}' (allowed: {:?})",
                    gate.id,
                    claim.claim_type,
                    CLAIM_TYPES
                );
            }
        }
    }

    // Publisher coherence: if any [[ui_component]] is declared the manifest
    // must also carry a [publisher] block (so install can verify signatures).
    // A standalone [publisher] without ui_components is allowed (an addon may
    // pre-declare its publisher identity for future UI bundles).
    match (publisher, ui_components.is_empty()) {
        (Some(p), _) => {
            if p.label.trim().is_empty() {
                bail!("publisher.label must not be empty");
            }
            validate_publisher_pk_b64(&p.ed25519_public_key)?;
        }
        (None, false) => {
            bail!(
                "manifest declares {} [[ui_component]] entries but no [publisher] block — \
                 signed UI bundles require publisher.ed25519_public_key",
                ui_components.len()
            );
        }
        (None, true) => {}
    }

    if let Some(req) = sdk_version {
        // Akceptujemy zarowno czyste `Version` ("0.2.0"), jak i `VersionReq`
        // (">=0.2.0", "^0.3"). semver::VersionReq parsuje oba.
        semver::VersionReq::parse(req).map_err(|e| {
            anyhow::anyhow!("addon.sdk_version '{}' is not a valid semver req: {}", req, e)
        })?;
    }

    Ok(())
}

fn validate_storage(cfg: &StorageConfig) -> Result<()> {
    if !STORAGE_SQL_DIALECTS.contains(&cfg.sql_dialect.as_str()) {
        bail!(
            "storage.sql_dialect '{}' is invalid (allowed: {:?})",
            cfg.sql_dialect,
            STORAGE_SQL_DIALECTS
        );
    }
    if !STORAGE_ENCRYPTION_MODES.contains(&cfg.encryption.as_str()) {
        bail!(
            "storage.encryption '{}' is invalid (allowed: {:?})",
            cfg.encryption,
            STORAGE_ENCRYPTION_MODES
        );
    }
    if cfg.sql {
        if cfg.sql_backends.is_empty() {
            bail!("storage.sql=true requires non-empty storage.sql_backends");
        }
        for be in &cfg.sql_backends {
            if !STORAGE_SQL_BACKENDS.contains(&be.as_str()) {
                bail!(
                    "storage.sql_backends contains invalid backend '{}' (allowed: {:?})",
                    be,
                    STORAGE_SQL_BACKENDS
                );
            }
        }
    }
    Ok(())
}

/// Walidacja `[[uses_alias]].id` / `[[uses_model]].id`. Identyfikatory
/// pochodza z nieprzefiltrowanego TOML — wymagamy formy zgodnej z innymi
/// elementami rejestru (lowercase alphanum + `_`/`-`/`.`) i max 128 znakow.
/// Trim wykonujemy na wejsciu, bo whitespace-only id w TOML to zazwyczaj
/// blad operatora a nie zamierzona wartosc — odrzucamy zamiast normalizowac.
fn validate_uses_target_id(kind: &str, raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("{} entry has empty id", kind);
    }
    if trimmed.len() != raw.len() {
        bail!(
            "{} id '{}' has leading/trailing whitespace",
            kind,
            raw.escape_debug()
        );
    }
    if raw.len() > 128 {
        bail!("{} id '{}' exceeds 128 chars", kind, raw);
    }
    if !raw
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.')
    {
        bail!(
            "{} id '{}' contains invalid characters (allowed: [a-z0-9_.-])",
            kind,
            raw
        );
    }
    Ok(())
}

fn check_unique_ids<'a, I>(kind: &str, ids: I) -> Result<()>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen: HashSet<&str> = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            bail!("Duplicate {} id: {}", kind, id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod chunk_c_validation_tests {
    use super::*;

    fn make_alias(id: &str, visibility: AliasVisibility, consumers: Vec<String>) -> AliasSpec {
        AliasSpec {
            id: id.to_string(),
            display_name: id.to_string(),
            methods: vec![],
            suggested_default: String::new(),
            gate: None,
            visibility,
            allowed_consumers: consumers,
        }
    }

    fn validate(
        aliases: &[AliasSpec],
        uses_aliases: &[UsesAliasSpec],
        uses_models: &[UsesModelSpec],
    ) -> Result<()> {
        validate_manifest_extensions(
            None,
            aliases,
            &[],
            &[],
            &[],
            &[],
            None,
            uses_aliases,
            uses_models,
            None,
        )
    }

    #[test]
    fn ui_component_without_publisher_is_rejected() {
        let uic = UiComponentSpec {
            id: "panel".into(),
            display_name: "Panel".into(),
            slot: "main".into(),
            src: "ui/panel.js".into(),
            signature: SIGNATURE_PLACEHOLDER.into(),
            risk: "low".into(),
            host_permissions: vec![],
        };
        let err = validate_manifest_extensions(
            None, &[], &[], &[], &[], &[uic], None, &[], &[], None,
        )
        .expect_err("missing publisher must reject");
        assert!(err.to_string().contains("no [publisher] block"));
    }

    #[test]
    fn publisher_with_bad_pk_length_is_rejected() {
        let pub_info = PublisherInfo {
            ed25519_public_key: "too-short".into(),
            label: "ACME".into(),
            contact: None,
        };
        let err = validate_manifest_extensions(
            None, &[], &[], &[], &[], &[], None, &[], &[], Some(&pub_info),
        )
        .expect_err("bad pk must reject");
        assert!(err.to_string().contains("invalid length"));
    }

    #[test]
    fn publisher_with_empty_label_is_rejected() {
        let pub_info = PublisherInfo {
            ed25519_public_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
            label: "   ".into(),
            contact: None,
        };
        let err = validate_manifest_extensions(
            None, &[], &[], &[], &[], &[], None, &[], &[], Some(&pub_info),
        )
        .expect_err("empty label must reject");
        assert!(err.to_string().contains("publisher.label"));
    }

    #[test]
    fn restricted_alias_without_consumers_is_rejected() {
        let alias = make_alias("ra", AliasVisibility::Restricted, vec![]);
        let err = validate(&[alias], &[], &[]).expect_err("must reject");
        assert!(err.to_string().contains("allowed_consumers is empty"));
    }

    #[test]
    fn public_alias_with_consumers_is_rejected() {
        let alias = make_alias("pa", AliasVisibility::Public, vec!["c1".into()]);
        let err = validate(&[alias], &[], &[]).expect_err("must reject");
        assert!(err.to_string().contains("must be empty"));
    }

    #[test]
    fn private_alias_with_consumers_is_rejected() {
        let alias = make_alias("p", AliasVisibility::Private, vec!["c1".into()]);
        assert!(validate(&[alias], &[], &[]).is_err());
    }

    #[test]
    fn uses_alias_required_without_reason_is_rejected() {
        let u = UsesAliasSpec {
            id: "x".into(),
            required: true,
            reason: String::new(),
        };
        let err = validate(&[], &[u], &[]).expect_err("must reject");
        assert!(err.to_string().contains("reason is empty"));
    }

    #[test]
    fn uses_alias_duplicate_ids_rejected() {
        let u1 = UsesAliasSpec {
            id: "dup".into(),
            required: false,
            reason: String::new(),
        };
        let u2 = UsesAliasSpec {
            id: "dup".into(),
            required: false,
            reason: String::new(),
        };
        assert!(validate(&[], &[u1, u2], &[]).is_err());
    }

    #[test]
    fn well_formed_aliases_and_uses_accepted() {
        let aliases = vec![
            make_alias("public-a", AliasVisibility::Public, vec![]),
            make_alias(
                "restr-a",
                AliasVisibility::Restricted,
                vec!["friend".into()],
            ),
            make_alias("priv-a", AliasVisibility::Private, vec![]),
        ];
        let uses_aliases = vec![UsesAliasSpec {
            id: "external".into(),
            required: false,
            reason: "telemetry".into(),
        }];
        let uses_models = vec![UsesModelSpec {
            id: "llama-3".into(),
            required: true,
            reason: "primary chat model".into(),
        }];
        validate(&aliases, &uses_aliases, &uses_models).expect("accepts");
    }
}
