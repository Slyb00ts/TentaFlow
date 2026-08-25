// =============================================================================
// Plik: addon/lifecycle.rs
// Opis: Cykl zycia addonu — instalacja, deinstalacja, upgrade. Parsowanie
//       manifest.toml, walidacja, rejestracja w DB, zarzadzanie plikami WASM.
// =============================================================================

use std::path::Path;

use anyhow::{bail, Result};
use tracing::{info, warn};

use super::{
    AddonDeclaredPermission, AddonManifest, AddonOAuthProviderSection, AddonVisibilitySection,
    DisambiguationRule, ManifestNetworkRule, ManifestTool, ManifestToolParameter,
    ResourceRequirements,
};
use crate::db::DbPool;

// =============================================================================
// install — instalacja addonu
// =============================================================================

/// Instaluje addon z podanego katalogu.
///
/// Kroki:
/// 1. Odczytaj manifest.toml
/// 2. Waliduj manifest (wymagane pola, poprawnosc)
/// 3. Odczytaj plik WASM (walidacja istnienia + rozmiar do logowania)
/// 4. Zarejestruj addon w DB (tabela addons — manifest_json zawiera pelny manifest)
/// 5. Ustaw domyslne limity zasobow (addon_resource_limits)
pub fn install(addon_dir: &Path, db: &DbPool) -> Result<AddonManifest> {
    // 1. Odczytaj manifest.toml
    let manifest_path = addon_dir.join("manifest.toml");
    if !manifest_path.exists() {
        bail!("Brak pliku manifest.toml w {:?}", addon_dir);
    }

    let manifest_content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac manifest.toml: {e}"))?;

    let manifest = parse_manifest_toml(&manifest_content)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie sparsowac manifest.toml: {e}"))?;

    // Instalacja 1:1 (bundled/upload): id instancji == package_id, materializujemy
    // pakiet do wersjonowanego store'u. install_instance() reuzywa install_core
    // z osobnym id instancji i bez materializacji (pakiet juz w katalogu).
    let package_id = manifest.addon_id.clone();
    let package_version = manifest.version.clone();
    install_core(
        addon_dir,
        db,
        manifest,
        &manifest_content,
        &package_id,
        &package_version,
        true,
        true,
    )
}

/// Catalog-only upload: materialize an uploaded package into the store + catalog
/// (+ replicate its bytes as a blob) WITHOUT creating an instance. Re-uploading
/// the same version overwrites the package files; a new version is added to the
/// catalog so existing instances see it as an available update — there is no
/// "already installed" error because no instance is created. Instances are
/// created/updated from the catalog (install_instance / update_instance),
/// exactly like bundled packages. Returns (package_id, version).
pub fn install_package_to_catalog(addon_dir: &Path, db: &DbPool) -> Result<(String, String)> {
    let manifest_path = addon_dir.join("manifest.toml");
    if !manifest_path.exists() {
        bail!("Brak pliku manifest.toml w {:?}", addon_dir);
    }
    let manifest_content = std::fs::read_to_string(&manifest_path)?;
    let manifest = parse_manifest_toml(&manifest_content)?;
    let package_id = manifest.addon_id.clone();
    let package_version = manifest.version.clone();
    // A BUNDLED package version is owned by the binary — refuse to overwrite its
    // files/source with an upload (which would run uploaded bytes under the
    // bundled version until the next startup reconcile, and could replicate that
    // swap to peers). An update is always a NEW version, so this never blocks a
    // legitimate update — only a same-version clobber of a bundled package.
    if let Some(existing) =
        crate::db::repository::get_addon_package(db, &package_id, &package_version)?
    {
        if existing.source == "bundled" {
            bail!(
                "wersja '{package_version}' pakietu '{package_id}' jest wbudowana — \
                 nie mozna jej nadpisac uploadem; podbij wersje w manifescie"
            );
        }
    }
    install_core(
        addon_dir,
        db,
        manifest,
        &manifest_content,
        &package_id,
        &package_version,
        true,
        false,
    )?;
    Ok((package_id, package_version))
}

/// Instaluje NOWA instancje pakietu z katalogu pod wlasnym, syntetycznym
/// addon_id. Instancja ma wlasny storage/config/permissions/flow-bloki/sync
/// (wszystko scope'owane po addon_id) i przypiety `package_version`. Dane
/// startuja puste (tylko migracje). Zwraca addon_id utworzonej instancji.
pub fn install_instance(
    db: &DbPool,
    package_id: &str,
    package_version: &str,
    display_name: &str,
    config: &std::collections::BTreeMap<String, String>,
) -> Result<String> {
    let name = display_name.trim();
    if name.is_empty() {
        bail!("nazwa instancji nie moze byc pusta");
    }
    let pkg = crate::db::repository::get_addon_package(db, package_id, package_version)?
        .ok_or_else(|| {
            anyhow::anyhow!("pakiet '{package_id}' v{package_version} nie istnieje w katalogu")
        })?;
    let pkg_dir = crate::addon::bundled::package_dir(package_id, package_version);
    if !pkg_dir.join("manifest.toml").exists() {
        bail!(
            "pliki pakietu '{package_id}' v{package_version} nie istnieja w store ({:?})",
            pkg_dir
        );
    }

    // Validate every declared connection_param against the provided config:
    // a required param must be present and non-empty. No silent defaults.
    let declared = parse_connection_params(&pkg.manifest_json)?;
    for param in &declared {
        if param.required {
            let present = config
                .get(&param.key)
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !present {
                bail!(
                    "wymagany parametr polaczenia '{}' ({}) jest pusty",
                    param.key,
                    param.label
                );
            }
        }
    }

    // Syntetyczny, unikalny addon_id instancji. Czytelny prefix pakietu jest
    // uzytkowy w flow blokach (addon.{id}.{block}) i toolach LLM ({id}.{tool}).
    let instance_id = unique_instance_id(db, package_id)?;

    // Manifest instancji = manifest pakietu z przepisanym [addon] id/name ORAZ
    // podstawionymi ${key} placeholderami w hostach regul sieciowych, zeby
    // persistowany manifest niosl konkretny adres robota (Network tab approval).
    let instance_manifest =
        rewrite_manifest_for_instance(&pkg.manifest_json, &instance_id, name, config)?;
    let manifest = parse_manifest_toml(&instance_manifest)
        .map_err(|e| anyhow::anyhow!("manifest instancji niepoprawny: {e}"))?;

    install_core(
        &pkg_dir,
        db,
        manifest,
        &instance_manifest,
        package_id,
        package_version,
        false,
        true,
    )?;

    // Persist the entered connection-param values into `addon_config` (scoped to
    // the new instance_id) so the addon can read its own IP/serial at runtime.
    for param in &declared {
        if let Some(value) = config.get(&param.key) {
            let value = value.trim();
            if !value.is_empty() {
                crate::db::repository::upsert_addon_config_value(
                    db,
                    &instance_id,
                    &param.key,
                    value,
                    false,
                    None,
                )?;
            }
        }
    }

    Ok(instance_id)
}

/// Odinstalowuje instancje: usuwa wpisy DB (uninstall) ORAZ katalog danych
/// instancji (orgs/<org>/addons/<addon_id>/), bo instancja jest wlascicielem
/// swoich danych. Nie rusza wspoldzielonego store'u pakietow.
pub fn uninstall_instance(addon_id: &str, db: &DbPool) -> Result<()> {
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    // Zamknij per-instancyjny SQLite pool i usun katalog danych ZANIM skasujemy
    // wiersz z DB. Gdyby purge sie nie udal, instancja zostaje w DB (retry
    // mozliwy) zamiast zniknac z listy zostawiajac dane-widmo na dysku.
    // Czysci tylko katalog instancji (orgs/<org>/addons/<addon_id>/), nigdy
    // wspoldzielonego store'u pakietow.
    crate::addon::storage_sql::close_addon_db(org_id, addon_id);
    // B2 (RAG): jawny cleanup grafu PRZED `remove_dir_all(addon_data_dir)` —
    // zamyka backendy sled, kasuje wiersze `addon_graph_collections` i pliki
    // `.cozo` tej instancji, kluczowane `(org_id, addon_id)`. Inwariant izolacji:
    // kasuje WYŁĄCZNIE graf tej instancji, nie rusza grafu innej instancji tego
    // samego pakietu (osobny `instance_id` → osobny `addon_id` w kluczu).
    // Korekta B1+B2 (MED #5): błąd cleanupu grafu NIE może być połknięty. Cozo
    // zamyka uchwyty sled (`seal_key_for_delete` → slot `Removed`) i kasuje wiersze
    // `addon_graph_collections` + pliki `.cozo` PRZED `remove_dir_all`. Gdy to się
    // nie uda, PRZERYWAMY uninstall (instancja zostaje w DB → retry możliwy)
    // zamiast usuwać katalog z na wpół-skasowanym grafem i zostawiać dane-widmo.
    #[cfg(feature = "graph")]
    {
        let mgr = crate::services::graph_manager(db);
        mgr.delete_all_for_addon(org_id, addon_id).map_err(|e| {
            anyhow::anyhow!("uninstall_instance: graph cleanup dla '{addon_id}' nieudany: {e}")
        })?;
    }
    if let Ok(dir) = crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id) {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| {
                anyhow::anyhow!("nie udalo sie usunac danych instancji {:?}: {e}", dir)
            })?;
        }
    }
    // RAG E1.3 (Bug 3): skasuj wpis muteksu instancji z document store, żeby mapa
    // `instance_locks()` nie rosła w nieskończoność dla usuwanych instancji.
    crate::addon::host_functions::document::forget_instance_lock(org_id, addon_id);
    uninstall(addon_id, db)
}

/// Generuje unikalny addon_id instancji w formie `{package_id}-{8hex}`.
fn unique_instance_id(db: &DbPool, package_id: &str) -> Result<String> {
    for _ in 0..8 {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let candidate = format!("{}-{}", package_id, &suffix[..8]);
        if crate::db::repository::get_addon(db, &candidate)?.is_none() {
            return Ok(candidate);
        }
    }
    bail!("nie udalo sie wygenerowac unikalnego id instancji dla '{package_id}'")
}

/// One declared `[[robot.connection_param]]` entry, parsed from the package
/// manifest. Drives install-time validation, the GUI form and (via `required`)
/// the "must be provided" check.
#[derive(Debug, Clone)]
pub struct DeclaredConnectionParam {
    pub key: String,
    pub label: String,
    pub param_type: String,
    pub required: bool,
    pub placeholder: String,
}

/// Parses the `[[robot.connection_param]]` list from a package manifest TOML.
/// Returns an empty vec for non-robot packages (no `[robot]` section).
pub fn parse_connection_params(manifest_toml: &str) -> Result<Vec<DeclaredConnectionParam>> {
    let value: toml::Value = toml::from_str(manifest_toml)
        .map_err(|e| anyhow::anyhow!("manifest pakietu niepoprawny: {e}"))?;
    let Some(params) = value
        .get("robot")
        .and_then(|r| r.get("connection_param"))
        .and_then(|p| p.as_array())
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(params.len());
    for entry in params {
        let key = entry
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("connection_param bez 'key'"))?
            .to_string();
        let label = entry
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or(&key)
            .to_string();
        let param_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("string")
            .to_string();
        let required = entry
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let placeholder = entry
            .get("placeholder")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(DeclaredConnectionParam {
            key,
            label,
            param_type,
            required,
            placeholder,
        });
    }
    Ok(out)
}

/// Substitutes `${key}` placeholders in `input` using `config`. Every `${...}`
/// must resolve — an unresolved placeholder bails so no half-resolved host
/// (e.g. a literal `${ip}`) is ever persisted. When `validate_host` is set, each
/// substituted value is checked as a clean host token (see `validate_host_token`)
/// BEFORE it lands in the output, so an operator-supplied connection-param can
/// never inject a scheme/port/path/userinfo into a network_rule host.
fn substitute_placeholders(
    input: &str,
    config: &std::collections::BTreeMap<String, String>,
    validate_host: bool,
) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("niezamkniety placeholder ${{ w manifescie"))?;
        let key = &after[..end];
        let value = config.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
        let value =
            value.ok_or_else(|| anyhow::anyhow!("brak wartosci dla placeholdera '${{{key}}}'"))?;
        if validate_host {
            validate_host_token(key, value)?;
        }
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Validates that a connection-param `value` substituted into a network_rule
/// `host` is a clean host token: a bare IP literal (IPv4/IPv6) or a DNS
/// hostname. This is the security gate for operator-supplied robot addresses —
/// the substituted host later becomes an admin-approvable exact host and is used
/// to build `http://{host}:{port}/...`, so anything carrying a scheme, port,
/// path, userinfo, whitespace or control characters must be rejected here.
fn validate_host_token(key: &str, value: &str) -> Result<()> {
    let reject = |reason: &str| -> anyhow::Error {
        anyhow::anyhow!("connection-param '{key}' nie jest poprawnym hostem: {reason}")
    };
    if value.is_empty() {
        return Err(reject("pusta wartosc"));
    }
    if value.contains("://") {
        return Err(reject("zawiera schemat URL"));
    }
    // Whitespace/control + path/userinfo/query separators are never legal in a
    // bare host. ':' is checked separately below so a bare IPv6 literal (which
    // legitimately contains ':') is still accepted, while `host:port` is not.
    if let Some(bad) = value
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '?' | '#' | '\\'))
    {
        return Err(reject(&format!("niedozwolony znak '{bad}'")));
    }
    // A bare IP literal is always a valid host (IPv4 has no ':'; IPv6 does, so
    // it is matched here BEFORE the ':' rejection that guards against host:port).
    if value.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    // Beyond an IP literal, ':' can only mean an attached port — reject it.
    if value.contains(':') {
        return Err(reject("niedozwolony znak ':'"));
    }
    if is_valid_dns_hostname(value) {
        return Ok(());
    }
    Err(reject("nie jest adresem IP ani nazwa DNS"))
}

/// True when `host` is a syntactically valid DNS hostname: 1+ dot-separated
/// labels of `[A-Za-z0-9-]`, each 1..=63 chars, no leading/trailing hyphen.
fn is_valid_dns_hostname(host: &str) -> bool {
    if host.len() > 253 {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// Przepisuje manifest pakietu na instancje: ustawia [addon].id = instance_id,
/// [addon].name = display_name (manifest.display_name mapuje sie z pola `name`)
/// ORAZ podstawia ${key} placeholdery w `host`/`port` kazdej reguly sieciowej
/// uzywajac wartosci connection-param. Dzieki temu persistowany manifest oraz
/// sparsowane `manifest.network_rules` niosa konkretny adres (Network tab pinuje
/// realny host robota). Zwraca manifest jako TOML.
fn rewrite_manifest_for_instance(
    manifest_toml: &str,
    instance_id: &str,
    display_name: &str,
    config: &std::collections::BTreeMap<String, String>,
) -> Result<String> {
    let mut value: toml::Value = toml::from_str(manifest_toml)
        .map_err(|e| anyhow::anyhow!("manifest pakietu niepoprawny: {e}"))?;
    let addon = value
        .get_mut("addon")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| anyhow::anyhow!("manifest pakietu bez sekcji [addon]"))?;
    addon.insert(
        "id".to_string(),
        toml::Value::String(instance_id.to_string()),
    );
    addon.insert(
        "name".to_string(),
        toml::Value::String(display_name.to_string()),
    );

    if let Some(rules) = value.get_mut("network_rule").and_then(|v| v.as_array_mut()) {
        for rule in rules.iter_mut() {
            let Some(table) = rule.as_table_mut() else {
                continue;
            };
            if let Some(toml::Value::String(host)) = table.get("host") {
                let resolved = substitute_placeholders(host, config, true)?;
                table.insert("host".to_string(), toml::Value::String(resolved));
            }
            if let Some(toml::Value::String(port)) = table.get("port") {
                let resolved = substitute_placeholders(port, config, false)?;
                table.insert("port".to_string(), toml::Value::String(resolved));
            }
        }
    }

    toml::to_string(&value).map_err(|e| anyhow::anyhow!("serializacja manifestu instancji: {e}"))
}

/// Rdzen rejestracji addona/instancji. `addon_dir` to katalog zrodlowy z
/// plikami (manifest.toml + wasm + migrations); `manifest`/`manifest_content`
/// niosa tozsamosc docelowa (`manifest.addon_id` == id instancji, wiec storage,
/// permissions, flow bloki i sync sa scope'owane po nim). `package_id`/
/// `package_version` wskazuja wersjonowany pakiet w store'ie — uzywane do
/// resolucji wasm/migracji oraz kolumn `addons.package_*`. `materialize=true`
/// kopiuje pliki zrodlowe do store'u i katalogizuje wersje (bundled/upload);
/// instancja z istniejacego pakietu wola z false.
fn install_core(
    addon_dir: &Path,
    db: &DbPool,
    manifest: AddonManifest,
    manifest_content: &str,
    package_id: &str,
    package_version: &str,
    materialize: bool,
    create_instance: bool,
) -> Result<AddonManifest> {
    // 2. Walidacja
    validate_manifest(&manifest)?;

    // Sprawdzenie kompatybilnosci SDK addona z rdzeniem (F1a §6.2.Y).
    // None → kompatybilny (addon nie deklaruje wymagan); Some(req) → musi
    // matchowac CORE_SDK_VERSION.
    if let Err(e) = crate::addon::sdk_version::check_compatibility(manifest.sdk_version.as_deref())
    {
        bail!("Addon '{}': {}", manifest.addon_id, e);
    }

    // 2b. F1c P2 — verify Ed25519 signatures of [[ui_component]] bundles
    // against [publisher] key in the trust store. Failure aborts install
    // before any DB row is written. The manifest validator already rejected
    // "ui_components without publisher" combinations, so an Some(publisher)
    // implies every ui_component must verify.
    if let Some(publisher) = manifest.publisher.as_ref() {
        for component in &manifest.ui_components {
            let bundle_path =
                match crate::util::path_safety::safe_resolve(addon_dir, &component.src) {
                    Ok(p) => p,
                    Err(e) => {
                        let pk_short = crate::addon::signature::truncate_pk_for_audit(
                            &publisher.ed25519_public_key,
                        );
                        let _ = crate::db::repository::log_audit(
                            db,
                            None,
                            Some(&manifest.addon_id),
                            "addon.ui_signature_verify",
                            Some(component.id.as_str()),
                            Some(&format!(
                                "denied: unsafe bundle path; publisher_pk={pk_short}"
                            )),
                            None,
                            None,
                        );
                        bail!(
                            "ui_component '{}' src '{}' rejected: {}",
                            component.id,
                            component.src,
                            e
                        );
                    }
                };
            if let Err(e) = crate::addon::signature::verify_ui_component_bundle(
                &bundle_path,
                &publisher.ed25519_public_key,
                &component.signature,
                db,
            ) {
                let pk_short =
                    crate::addon::signature::truncate_pk_for_audit(&publisher.ed25519_public_key);
                let _ = crate::db::repository::log_audit(
                    db,
                    None,
                    Some(&manifest.addon_id),
                    "addon.ui_signature_verify",
                    Some(component.id.as_str()),
                    Some(&format!("denied: {e}; publisher_pk={pk_short}")),
                    None,
                    None,
                );
                bail!(
                    "ui_component '{}': signature verify failed ({})",
                    component.id,
                    e
                );
            }
            let pk_short =
                crate::addon::signature::truncate_pk_for_audit(&publisher.ed25519_public_key);
            let _ = crate::db::repository::log_audit(
                db,
                None,
                Some(&manifest.addon_id),
                "addon.ui_signature_verify",
                Some(component.id.as_str()),
                Some(&format!("ok: publisher_pk={pk_short}")),
                None,
                None,
            );
        }
    }

    // 3. Odczytaj plik WASM
    let wasm_path = addon_dir.join(&manifest.wasm_file);

    // CR-010: Ochrona przed path traversal — sprawdz czy sciezka nie wychodzi poza katalog addonu
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = addon_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!(
                    "Path traversal wykryty w wasm_file: {:?}",
                    manifest.wasm_file
                );
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    let wasm_bytes = std::fs::read(&wasm_path)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac pliku WASM: {e}"))?;

    // F1c P5 — compile every declared [[flow_template]] before touching the
    // DB. A flow.json that fails schema, edge, or cycle validation aborts
    // install with a precise diagnostic rather than landing a half-installed
    // addon whose templates silently no-op at runtime. Registry insertion
    // happens after the DB COMMIT so a later failure does not leave a flow
    // visible to invokers for an addon that is not actually installed.
    // Fail-fast validation before any DB write — materialize recompiles+registers.
    let _ = compile_flow_templates(&manifest, addon_dir)?;

    let platforms_json =
        serde_json::to_string(&manifest.platforms).unwrap_or_else(|_| "[\"all\"]".to_string());

    let wasm_size = wasm_bytes.len() as i64;

    // Materializuj pakiet do wersjonowanego store'u packages/{id}/{version}/.
    // Runtime (get_or_compile_module) oraz migracje rozwiazuja wasm/migracje
    // wylacznie z tego katalogu, niezaleznie od callera. Bundled reconcile pisze
    // juz bezposrednio do package_dir (addon_dir == package_dir) i sam wpisuje
    // wersje do `addon_packages`; inni callerzy (upload przez dashboard)
    // dostarczaja addon_dir w katalogu tymczasowym, wiec kopiujemy pliki do
    // store'u i katalogizujemy wersje, inaczej runtime nie znajdzie wasm.
    let package_dir = crate::addon::bundled::package_dir(package_id, package_version);
    if materialize {
        // Kopiujemy tylko gdy zrodlo rozni sie od katalogu wersji w store'ie.
        let needs_copy = match (addon_dir.canonicalize(), package_dir.canonicalize()) {
            (Ok(a), Ok(b)) => a != b,
            _ => addon_dir != package_dir.as_path(),
        };
        if needs_copy {
            // Katalog wersji jest immutable (jedna wersja = jeden niezmienny
            // zestaw plikow), wiec czyscimy go przed kopiowaniem — inaczej
            // re-upload tej samej wersji albo nieudany wczesniejszy install
            // zostawilby nieaktualne pliki (np. usuniete migracje SQL).
            let _ = std::fs::remove_dir_all(&package_dir);
            copy_package_into_store(addon_dir, &package_dir)?;
        }
        // Katalogizacja wersji jest niezalezna od kopiowania — robimy ja zawsze
        // przy materialize, tez gdy pliki juz byly w store'ie.
        crate::db::repository::upsert_addon_package(
            db,
            package_id,
            package_version,
            &manifest.display_name,
            manifest_content,
            "",
            "uploaded",
        )?;
        // Replicate the package BYTES so other mesh nodes can install this
        // uploaded addon (bundled packages already live in every node's binary,
        // so they skip this). Best-effort: failure never fails the local install.
        if let Err(e) = capture_addon_package_blob(db, package_id, package_version, &package_dir) {
            tracing::warn!(
                "addon package '{package_id}' v{package_version}: blob sync capture nieudany: {e}"
            );
        }
    }

    // Catalog-only mode (uploaded package): the package template + its bytes are
    // now in the store/catalog/blob outbox — stop here without creating a 1:1
    // instance. Instances are created explicitly from the catalog
    // (install_instance), exactly like bundled packages.
    if !create_instance {
        return Ok(manifest);
    }

    // Hash katalogowy wersji, z ktorej instalujemy instancje — zapisany na
    // instancji, zeby detekcja aktualizacji reagowala na zmiane TRESCI pakietu
    // (manifest/wasm/migracje) nawet bez podbicia numeru wersji. Pobierane przed
    // zajeciem locka, zeby nie zagniezdzac blokady DB.
    let installed_bundle_hash =
        crate::db::repository::get_addon_package(db, package_id, package_version)
            .ok()
            .flatten()
            .map(|p| p.bundle_hash)
            .unwrap_or_default();

    // 5-9. Zarejestruj w DB (w jednej transakcji)
    let conn = db.write().unwrap();

    conn.execute("BEGIN TRANSACTION", [])?;

    // Sprawdz czy addon juz istnieje
    let existing: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![&manifest.addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if existing {
        conn.execute("ROLLBACK", [])?;
        bail!(
            "Addon '{}' jest juz zainstalowany. Podbij wersje w manifescie i zaktualizuj instancje zamiast install()",
            manifest.addon_id
        );
    }

    // Odczytaj SKILL.md z katalogu addonu (jesli istnieje)
    let skill_md = std::fs::read_to_string(addon_dir.join("SKILL.md")).ok();

    let keywords_json =
        serde_json::to_string(&manifest.keywords).unwrap_or_else(|_| "[]".to_string());

    let category = manifest.category.as_deref().unwrap_or("");

    let disambiguation_json =
        serde_json::to_string(&manifest.disambiguation).unwrap_or_else(|_| "[]".to_string());

    let icon = manifest.icon.as_deref().unwrap_or("");
    let runtime = manifest.runtime.as_deref().unwrap_or("wasmtime");
    let license = manifest.license.as_deref().unwrap_or("");
    let show_in_catalog = manifest.show_in_catalog.unwrap_or(true) as i64;

    // 5. Tabela addons — schemat z migracji 14 + 25 + 26 + 43 + 44
    // (skill_md, keywords_json, category, disambiguation_json, icon, runtime,
    //  wasm_size_bytes, license, show_in_catalog)
    // Faza 0: instancja 1:1 z pakietem — package_id == addon_id,
    // package_version == manifest.version. Faza 1 wprowadzi syntetyczne id
    // instancji rozne od package_id (install_instance).
    conn.execute(
        "INSERT INTO addons (addon_id, name, display_name, version, package_id, package_version, description, author, platforms, manifest_json, is_enabled, is_system, skill_md, keywords_json, category, disambiguation_json, icon, runtime, wasm_size_bytes, license, show_in_catalog, installed_bundle_hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        rusqlite::params![
            &manifest.addon_id,
            &manifest.display_name,
            &manifest.display_name,
            &manifest.version,
            package_id,
            package_version,
            &manifest.description.as_deref().unwrap_or(""),
            &manifest.author.as_deref().unwrap_or(""),
            &platforms_json,
            manifest_content,
            &skill_md,
            &keywords_json,
            category,
            &disambiguation_json,
            icon,
            runtime,
            wasm_size,
            license,
            show_in_catalog,
            &installed_bundle_hash,
        ],
    ).map_err(|e| anyhow::anyhow!("Nie udalo sie zarejestrowac addonu w DB: {e}"))?;

    // Uprawnienia, narzedzia i limity sa przechowywane w manifest_json
    // (tabela addons.manifest_json zawiera pelny manifest)

    conn.execute("COMMIT", [])?;
    drop(conn);

    // Derived state (resource limits, network rules, manifest metadata, per-addon
    // SQL migrations, compiled flows) — idempotent, shared with the mesh-sync
    // reconcile path. Runs after COMMIT so a fallible step never leaves the
    // `addons` row half-materialized in the same tx. The pre-tx flow compile
    // above already fail-fast-validated the templates before any DB write.
    materialize_addon_derived_state(db, &manifest, &package_dir)?;

    info!(
        "Addon '{}' v{} installed ({} WASM bytes, {} permissions, {} tools, {} network rules)",
        manifest.addon_id,
        manifest.version,
        wasm_size,
        manifest.declared_permissions.len(),
        manifest.tools.len(),
        manifest.network_rules.len()
    );

    Ok(manifest)
}

/// Upsert per-addon resource limits from the manifest's `[resources]` (or
/// defaults = no limit). Idempotent. Shared by install + sync reconcile.
fn upsert_addon_resource_limits(
    conn: &rusqlite::Connection,
    manifest: &AddonManifest,
) -> Result<()> {
    if let Some(ref res) = manifest.resources {
        conn.execute(
            "INSERT OR REPLACE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, document_storage_mb, http_requests_per_min, \
              llm_tokens_per_min, fuel_limit) \
             VALUES (?1, 0, 0, ?2, 1, 0, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                &manifest.addon_id,
                res.memory_mb.unwrap_or(0) as i64,
                res.storage_total_mb.unwrap_or(0) as i64,
                res.document_storage_mb.unwrap_or(0) as i64,
                res.http_requests_per_minute.unwrap_or(0) as i64,
                res.llm_tokens_per_minute.unwrap_or(0) as i64,
                res.fuel_limit.unwrap_or(0) as i64,
            ],
        )
        .ok();
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES (?1, 0, 0, 0, 1, 0, 0, 0, 0)",
            rusqlite::params![&manifest.addon_id],
        )
        .ok();
    }
    Ok(())
}

/// Upsert declared network rules (each starts `approved=0` — a manifest
/// declaration is not admin consent). Idempotent. Shared by install + reconcile.
fn upsert_addon_network_rules(conn: &rusqlite::Connection, manifest: &AddonManifest) -> Result<()> {
    for rule in &manifest.network_rules {
        conn.execute(
            "INSERT OR IGNORE INTO addon_network_rules \
             (addon_id, rule_id, protocol, host, port, description, required, approved) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &manifest.addon_id,
                &rule.id,
                &rule.protocol,
                &rule.host,
                rule.port,
                rule.description.as_deref().unwrap_or(""),
                rule.required as i32,
                0
            ],
        )
        .ok();
    }
    Ok(())
}

/// Materialize an installed addon's LOCAL derived state from its package files:
/// resource limits, network rules, manifest metadata (permission catalog, oauth
/// providers, visibility), per-instance SQL migrations, and compiled flows. The
/// `addons` row must already exist. Idempotent — used by `install_core` AND by
/// the mesh-sync reconcile path (a replicated instance row rebuilds its derived
/// state locally from the package it already has in the store).
pub(crate) fn materialize_addon_derived_state(
    db: &DbPool,
    manifest: &AddonManifest,
    package_dir: &Path,
) -> Result<()> {
    {
        let conn = db.write().unwrap();
        upsert_addon_resource_limits(&conn, manifest)?;
        upsert_addon_network_rules(&conn, manifest)?;
    }
    sync_manifest_metadata(db, manifest)?;
    if matches!(manifest.storage.as_ref(), Some(s) if s.sql) {
        apply_addon_sql_migrations(manifest, package_dir, db)?;
    }
    let compiled_flows = compile_flow_templates(manifest, package_dir)?;
    let registry = crate::flow_runtime::registry::global();
    for flow in compiled_flows {
        registry.register(&manifest.addon_id, flow);
    }
    // RAG E2.0 — rejestruj `[[engine_flow]]` jako published modele flow_engine
    // (unikalna-per-instancję nazwa + wiązanie modelu). Idempotentne: re-install
    // / upgrade / mesh reconcile odtwarzają flow ze świeżego JSON.
    register_engine_flows(db, manifest, package_dir)?;
    materialize_addon_skill(db, manifest, package_dir);
    // B2 (RAG): unieważnij cache otwartych backendów grafowych tego addona po
    // re-materializacji (upgrade) — następny `graph_*` odbuduje backend ze
    // świeżego wpisu. NIE kasuje danych na dysku, tylko uchwyty (DashMap).
    #[cfg(feature = "graph")]
    crate::services::graph_manager(db).invalidate_addon(&manifest.addon_id);
    materialize_addon_aliases(db, manifest)?;
    Ok(())
}

/// Materializuje wystawiane przez addon `[[alias]]` do globalnej tabeli
/// `model_aliases` (+ owner/visibility/methods). Wywolywane z KAZDEJ sciezki
/// odbudowy stanu addona (install_core ORAZ mesh-sync reconcile), zeby addon z
/// `[[alias]]` byl samowystarczalny: instalacja tworzy aliasy z `suggested_default`
/// jako hint-targetem, a admin pozniej przepina je na realny model. Bez tego
/// panel addona nie ma jak wolac modeli (alias nie istnieje / nie resolwuje).
///
/// IDEMPOTENTNE i nieniszczace bindowan admina: `create_or_reactivate_model_alias_within_tx`
/// dla istniejacego aliasu tylko reaktywuje wiersz i NIE nadpisuje jego
/// `target_model` — re-install / upgrade / mesh reconcile zachowuja model
/// podpiety recznie przez admina. Cala petla idzie w jednej transakcji: bledny
/// alias rolluje sie razem z wpisami owner/audit (audit nie ma FK na alias).
pub(crate) fn materialize_addon_aliases(db: &DbPool, manifest: &AddonManifest) -> Result<()> {
    if manifest.aliases.is_empty() {
        return Ok(());
    }
    use crate::db::repository::{
        create_or_reactivate_model_alias_within_tx, set_alias_methods_within_tx,
        set_alias_visibility_within_tx, set_model_alias_active_audited_within_tx,
    };
    let mut conn = db
        .write()
        .map_err(|e| anyhow::anyhow!("db write for alias materialization: {e}"))?;
    let tx = conn.transaction()?;
    for alias_spec in &manifest.aliases {
        let alias_id = create_or_reactivate_model_alias_within_tx(
            &tx,
            &alias_spec.id,
            &alias_spec.suggested_default,
            "first_available",
            "addon",
            Some(&manifest.addon_id),
        )
        .map_err(|e| {
            anyhow::anyhow!(
                "addon '{}' alias '{}' registration failed: {e}",
                manifest.addon_id,
                alias_spec.id
            )
        })?;

        set_alias_visibility_within_tx(&tx, alias_id, alias_spec.visibility.as_db_str(), None)
            .map_err(|e| {
                anyhow::anyhow!(
                    "addon '{}' alias '{}' visibility write failed: {e}",
                    manifest.addon_id,
                    alias_spec.id
                )
            })?;

        set_alias_methods_within_tx(&tx, alias_id, &alias_spec.methods).map_err(|e| {
            anyhow::anyhow!(
                "addon '{}' alias '{}' methods write failed: {e}",
                manifest.addon_id,
                alias_spec.id
            )
        })?;

        // Gated alias zostaje zaparkowany (is_active=0) az policy engine / admin
        // go aktywuje — router nigdy nie widzi aktywnego gated aliasu.
        if alias_spec.gate.is_some() {
            set_model_alias_active_audited_within_tx(
                &tx,
                &alias_spec.id,
                false,
                Some(&manifest.addon_id),
            )
            .map_err(|e| {
                anyhow::anyhow!(
                    "addon '{}' gated alias '{}' deactivate failed: {e}",
                    manifest.addon_id,
                    alias_spec.id
                )
            })?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Deterministic skill id for an addon's materialized SKILL.md: UUIDv5 of the
/// addon_id under the OID namespace with a project-scoped prefix. Every fleet
/// node derives the identical id from the replicated `addons` row, so the
/// skills sync apply stays idempotent (a random id per node would leave
/// permanent duplicates).
pub(crate) fn addon_skill_id(addon_id: &str) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        format!("tentaflow:addon-skill:{addon_id}").as_bytes(),
    )
    .to_string()
}

/// Optional frontmatter of a SKILL.md file (the three keys the Harness plan
/// defines) plus the markdown body with the frontmatter block stripped.
pub(crate) struct SkillFrontmatter {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) body: String,
}

/// Strips surrounding single or double quotes from a scalar value.
fn unquote(value: &str) -> &str {
    let v = value.trim();
    if v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')))
    {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Tolerant frontmatter parser for SKILL.md. Hand-rolled on purpose:
/// serde_yaml is not a dependency of this crate and the format is limited to
/// three known keys (`name`, `description`, `tags`), so a full YAML engine
/// would be dead weight. Supported tag shapes: inline `[a, b]`, a plain
/// comma list, and a `- item` block list. A document without an opening
/// `---` line (or with an unterminated block) is returned verbatim as body.
pub(crate) fn parse_skill_frontmatter(raw: &str) -> SkillFrontmatter {
    let mut fm = SkillFrontmatter {
        name: None,
        description: None,
        tags: Vec::new(),
        body: raw.to_string(),
    };
    let after_open = if let Some(rest) = raw.strip_prefix("---\r\n") {
        rest
    } else if let Some(rest) = raw.strip_prefix("---\n") {
        rest
    } else {
        return fm;
    };
    let mut close: Option<(usize, usize)> = None;
    let mut offset = 0usize;
    for line in after_open.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            close = Some((offset, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    let Some((block_end, body_start)) = close else {
        return fm;
    };
    fm.body = after_open[body_start..]
        .trim_start_matches(['\r', '\n'])
        .to_string();

    let mut in_tags_list = false;
    for line in after_open[..block_end].lines() {
        let trimmed = line.trim();
        if in_tags_list {
            if let Some(item) = trimmed.strip_prefix("- ") {
                let item = unquote(item);
                if !item.is_empty() {
                    fm.tags.push(item.to_string());
                }
                continue;
            }
            in_tags_list = false;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "name" => {
                let v = unquote(value);
                if !v.is_empty() {
                    fm.name = Some(v.to_string());
                }
            }
            "description" => {
                let v = unquote(value);
                if !v.is_empty() {
                    fm.description = Some(v.to_string());
                }
            }
            "tags" => {
                if value.is_empty() {
                    in_tags_list = true;
                } else {
                    let inner = value
                        .strip_prefix('[')
                        .and_then(|v| v.strip_suffix(']'))
                        .unwrap_or(value);
                    fm.tags = inner
                        .split(',')
                        .map(|tag| unquote(tag).to_string())
                        .filter(|tag| !tag.is_empty())
                        .collect();
                }
            }
            _ => {}
        }
    }
    fm
}

/// Derives a registry-valid fallback skill name from an addon id. Addon ids
/// allow uppercase, '.', '_' and Unicode alphanumerics (`validate_manifest`),
/// but skill names are strict ASCII kebab-case — lowercase the ASCII
/// alphanumerics, fold every other run into a single hyphen, cut at the limit.
fn fallback_skill_name(addon_id: &str) -> String {
    let mut name = String::with_capacity(addon_id.len());
    for ch in addon_id.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_lowercase());
        } else if !name.is_empty() && !name.ends_with('-') {
            name.push('-');
        }
    }
    // Only ASCII was pushed, so the byte cut is char-boundary safe.
    name.truncate(crate::db::repository::SKILL_NAME_MAX_CHARS);
    while name.ends_with('-') {
        name.pop();
    }
    name
}

/// Materializes the addon's SKILL.md into the `skills` registry (Harness §3.2):
/// deterministic UUIDv5 id, source='addon', source_ref=addon_id. Optional
/// frontmatter overrides name/description/tags; fallbacks are the addon id and
/// the manifest description. A package WITHOUT a SKILL.md removes a previously
/// materialized row (the update dropped its skill). Best-effort by design: a
/// malformed or oversized skill must not fail an otherwise valid install, so
/// failures are logged and the skill is skipped.
pub(crate) fn materialize_addon_skill(db: &DbPool, manifest: &AddonManifest, package_dir: &Path) {
    use crate::db::repository::{
        delete_addon_skills, get_skill, is_kebab_case, upsert_skill, SKILL_DESCRIPTION_MAX_CHARS,
        SKILL_NAME_MAX_CHARS,
    };
    let addon_id = &manifest.addon_id;
    let skill_md = std::fs::read_to_string(package_dir.join("SKILL.md")).unwrap_or_default();
    if skill_md.trim().is_empty() {
        match delete_addon_skills(db, addon_id) {
            Ok(0) => {}
            Ok(removed) => info!(
                "Addon '{addon_id}': removed {removed} materialized skill(s) — package has no SKILL.md"
            ),
            Err(e) => warn!("Addon '{addon_id}': failed to remove materialized skill: {e}"),
        }
        return;
    }

    let fm = parse_skill_frontmatter(&skill_md);
    let skill_id = addon_skill_id(addon_id);
    let name = match fm.name.as_deref() {
        Some(n) if is_kebab_case(n) && n.chars().count() <= SKILL_NAME_MAX_CHARS => n.to_string(),
        Some(other) => {
            warn!(
                "Addon '{addon_id}': SKILL.md frontmatter name '{other}' is not valid kebab-case — deriving from the addon id"
            );
            fallback_skill_name(addon_id)
        }
        None => fallback_skill_name(addon_id),
    };
    let manifest_description = manifest.description.clone().unwrap_or_default();
    let description = match fm.description.as_deref() {
        Some(d) if d.chars().count() <= SKILL_DESCRIPTION_MAX_CHARS => d.to_string(),
        _ if !manifest_description.is_empty() => manifest_description,
        _ => manifest.display_name.clone(),
    };
    // Admin-editable fields (status, tags — only edits the upsert handler allows
    // on addon skills) survive package updates and mesh reconciles; frontmatter
    // tags only seed the first materialization.
    let existing = get_skill(db, &skill_id).ok().flatten();
    let (status, tags_json) = match &existing {
        Some(row) => (row.status.clone(), row.tags_json.clone()),
        None => (
            "active".to_string(),
            serde_json::to_string(&fm.tags).unwrap_or_else(|_| "[]".to_string()),
        ),
    };
    let category = manifest.category.as_deref().filter(|c| !c.is_empty());
    // Reconcile runs on every addon event on every node, and each upsert records
    // a sync capture — a no-op write would re-emit the row mesh-wide (op
    // amplification) and widen the LWW race against concurrent admin edits.
    if let Some(row) = &existing {
        let unchanged = row.source == "addon"
            && row.source_ref.as_deref() == Some(addon_id.as_str())
            && row.name == name
            && row.display_name.as_deref() == Some(manifest.display_name.as_str())
            && row.description == description
            && row.content == fm.body
            && row.category.as_deref() == category;
        if unchanged {
            return;
        }
    }
    let params = crate::db::models::SkillParams {
        id: &skill_id,
        name: &name,
        display_name: Some(&manifest.display_name),
        description: &description,
        content: &fm.body,
        tags_json: &tags_json,
        category,
        source: "addon",
        source_ref: Some(addon_id),
        status: &status,
        created_by: None,
        actor_user_id: None,
    };
    if let Err(e) = upsert_skill(db, &params) {
        warn!("Addon '{addon_id}': SKILL.md not materialized into the skills registry: {e}");
    }
}

/// Cap on a synced addon-package archive (compressed). Bounds disk/bandwidth a
/// trusted-but-buggy peer can push; real addon packages are well under this.
const MAX_SYNCED_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;

/// Blob id of a synced addon package: `addon-package:<package_id>:<version>`.
/// `package_id` and semver `version` never contain `:`, so the receiver splits
/// on the first `:` after the prefix unambiguously.
fn addon_package_blob_id(package_id: &str, version: &str) -> String {
    format!("addon-package:{package_id}:{version}")
}

/// Tar.gz the materialized package dir and hand it to the blob sync mechanism so
/// other mesh nodes receive the bytes (content-addressed by sha256). Only the
/// upload path calls this (bundled packages are in every binary). The blob is
/// deduped fleet-wide by content hash.
fn capture_addon_package_blob(
    db: &DbPool,
    package_id: &str,
    version: &str,
    package_dir: &Path,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    // Stage the archive in temp; the blob capture reads it to chunk into the
    // ledger, after which we can drop it (the origin keeps the package_dir).
    let tmp = std::env::temp_dir().join(format!(
        "tf-addon-pkg-{}.tar.gz",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let file = std::fs::File::create(&tmp)
            .map_err(|e| anyhow::anyhow!("create package archive: {e}"))?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(enc);
        builder
            .append_dir_all(".", package_dir)
            .map_err(|e| anyhow::anyhow!("tar package dir: {e}"))?;
        builder
            .into_inner()
            .map_err(|e| anyhow::anyhow!("finish tar: {e}"))?
            .finish()
            .map_err(|e| anyhow::anyhow!("finish gzip: {e}"))?;
    }
    // All fallible work in one closure so the temp archive is removed on EVERY
    // exit path (early errors included).
    let result = (|| -> Result<()> {
        let bytes =
            std::fs::read(&tmp).map_err(|e| anyhow::anyhow!("read package archive: {e}"))?;
        if bytes.len() as u64 > MAX_SYNCED_PACKAGE_BYTES {
            bail!(
                "pakiet '{package_id}' v{version} za duzy do synca ({} B > {} B)",
                bytes.len(),
                MAX_SYNCED_PACKAGE_BYTES
            );
        }
        let sha = hex::encode(Sha256::digest(&bytes));
        let capture = crate::sync::blob_capture::BlobWriteCapture::new(
            crate::services::org::DEFAULT_ORG_ID,
            addon_package_blob_id(package_id, version),
            &sha,
            "application/gzip",
            bytes.len() as u64,
            tmp.to_string_lossy().to_string(),
            None,
        );
        {
            let conn = db.write().map_err(|e| anyhow::anyhow!("db lock: {e}"))?;
            crate::sync::blob_capture::record_blob_write_capture(&conn, &capture)?;
        }
        crate::sync::blob_capture::ledger_blob_capture_now(db, &capture)
    })();
    let _ = std::fs::remove_file(&tmp);
    result
}

/// Receiver side: a synced `addon-package:` blob fully arrived. Extract the
/// tar.gz into this node's package store and upsert the catalog row so the
/// package becomes installable here. `blob_path` is the reassembled blob.
/// Returns (package_id, version). Trusted-peer input (mesh executor gate); tar
/// unpack rejects path traversal.
pub(crate) fn materialize_synced_addon_package(
    db: &DbPool,
    blob_id: &str,
    blob_path: &Path,
) -> Result<(String, String)> {
    let rest = blob_id
        .strip_prefix("addon-package:")
        .ok_or_else(|| anyhow::anyhow!("not an addon-package blob: {blob_id}"))?;
    let (package_id, version) = rest
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("malformed addon-package blob id: {blob_id}"))?;
    // Path-safety: package_id/version become path segments under the store, so
    // reject anything that could traverse out of it (trusted peer, but defense
    // in depth).
    let safe = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    };
    if !safe(package_id) || !safe(version) {
        bail!("unsafe addon-package ref: {package_id}:{version}");
    }

    // Receiver-side cap (defense in depth; the origin also caps before sync).
    if let Ok(meta) = std::fs::metadata(blob_path) {
        if meta.len() > MAX_SYNCED_PACKAGE_BYTES {
            bail!("synced package '{package_id}' v{version} przekracza limit rozmiaru");
        }
    }
    let pkg_dir = crate::addon::bundled::package_dir(package_id, version);
    // Extract into a sibling STAGING dir first and validate; only on success do
    // we atomically replace the live package dir. A bad/truncated archive or a
    // mismatched manifest never destroys the existing package.
    let staging = pkg_dir.with_file_name(format!(
        ".incoming-{version}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let extract = || -> Result<(String, String)> {
        let file = std::fs::File::open(blob_path)
            .map_err(|e| anyhow::anyhow!("open package blob: {e}"))?;
        let dec = flate2::read::GzDecoder::new(file);
        let mut archive = tar::Archive::new(dec);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)
            .map_err(|e| anyhow::anyhow!("create staging dir: {e}"))?;
        archive
            .unpack(&staging)
            .map_err(|e| anyhow::anyhow!("unpack package: {e}"))?;
        let manifest_toml = std::fs::read_to_string(staging.join("manifest.toml"))
            .map_err(|e| anyhow::anyhow!("read synced manifest.toml: {e}"))?;
        let manifest = parse_manifest_toml(&manifest_toml)?;
        // The blob id is untrusted metadata — the extracted manifest is the
        // truth. Refuse a mismatch (e.g. blob 'a:1' carrying manifest for 'b:2').
        if manifest.addon_id != package_id || manifest.version != version {
            bail!(
                "synced package mismatch: blob '{package_id}:{version}' but manifest is \
                 '{}:{}'",
                manifest.addon_id,
                manifest.version
            );
        }
        Ok((manifest.display_name.clone(), manifest_toml))
    };
    let (display_name, manifest_toml) = match extract() {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
    };
    // Swap: replace the live dir with the validated staging dir (same parent →
    // rename is atomic on-disk).
    let _ = std::fs::remove_dir_all(&pkg_dir);
    std::fs::rename(&staging, &pkg_dir).map_err(|e| {
        let _ = std::fs::remove_dir_all(&staging);
        anyhow::anyhow!("swap package dir: {e}")
    })?;
    crate::db::repository::upsert_addon_package(
        db,
        package_id,
        version,
        &display_name,
        &manifest_toml,
        "",
        "uploaded",
    )?;
    Ok((package_id.to_string(), version.to_string()))
}

/// Kopiuje cala zawartosc katalogu zrodlowego addonu (wasm, manifest.toml,
/// migrations/, pliki pomocnicze) do wersjonowanego store'u pakietow. Nadpisuje
/// istniejace pliki, zeby ponowny install tej samej wersji byl spójny.
fn copy_package_into_store(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie utworzyc katalogu pakietu {:?}: {e}", dst))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| anyhow::anyhow!("Nie udalo sie odczytac katalogu addonu {:?}: {e}", src))?
    {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_package_into_store(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| {
                anyhow::anyhow!("Nie udalo sie skopiowac {:?} -> {:?}: {e}", from, to)
            })?;
        }
    }
    Ok(())
}

/// Otwiera per-addon SQLite i aplikuje migracje z `<bundle>/<migrations_dir>/`.
/// Wywolywane tylko gdy `manifest.storage.sql == true`. Migration fail =
/// install fail z rollbackiem: czyscimy zarejestrowanego addona z core DB
/// oraz purgujemy pool, zeby kolejna proba install nie kolidowala.
fn apply_addon_sql_migrations(
    manifest: &AddonManifest,
    addon_dir: &Path,
    db: &DbPool,
) -> Result<()> {
    let storage = manifest.storage.as_ref().expect("checked by caller");
    let migrations_dir = storage.migrations_dir.as_str();

    if storage.encryption == "at-rest" {
        // F1a: deklaracja akceptowana, ale SQLCipher integracja przyjdzie w F8.
        tracing::warn!(
            "addon '{}': [storage].encryption='at-rest' — F1a nie wymusza szyfrowania (planowane F8 SQLCipher)",
            manifest.addon_id
        );
    }

    // F2 P1.b — install runs under `org-default` during P1.b. Per-tenant
    // install (lifecycle::install_for_org) lands in P1.c together with the
    // CLI surface; until then every install is owned by `org-default`,
    // matching the v32 backfill for `addons.org_id`.
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    match crate::addon::migrations::apply_migrations(
        &manifest.addon_id,
        &manifest.version,
        migrations_dir,
        addon_dir,
        db,
        org_id,
    ) {
        Ok(n) => {
            info!(
                "addon '{}': SQL storage gotowy ({} migracji zaaplikowanych w tej sesji)",
                manifest.addon_id, n
            );
            Ok(())
        }
        Err(e) => {
            // Rollback rejestracji addonu — usuwamy go z DB i zamykamy pool,
            // zeby kolejny install_addon nie trafil na "addon juz istnieje".
            tracing::error!(
                "addon '{}': migracje SQL FAILED ({}) — rollback install",
                manifest.addon_id,
                e.as_i32()
            );
            crate::addon::storage_sql::close_addon_db(org_id, &manifest.addon_id);
            // Usun z DB (best-effort, install i tak juz failuje).
            let _ = uninstall(&manifest.addon_id, db);
            bail!(
                "addon '{}': blad migracji SQL (kod {})",
                manifest.addon_id,
                e.as_i32()
            );
        }
    }
}

// =============================================================================
// Synchronizacja katalogu uprawnien, providerow OAuth i widocznosci z manifestu
// =============================================================================

/// Synchronizuje wpisy pomocnicze po install/upgrade addona:
/// - permission_catalog (upsert + diff delete)
/// - oauth_providers_decl (upsert per wpis)
/// - visibility (admin_only + default_groups)
pub fn sync_manifest_metadata(db: &crate::db::DbPool, manifest: &AddonManifest) -> Result<()> {
    use crate::db::repository;

    // 1. Permission catalog — zrodlem prawdy sa declared_permissions
    let addon_id = &manifest.addon_id;
    let mut keep_ids: Vec<String> = Vec::with_capacity(manifest.declared_permissions.len());
    for (idx, perm) in manifest.declared_permissions.iter().enumerate() {
        if perm.id.is_empty() {
            continue;
        }
        let entry = repository::DbAddonPermissionCatalogEntry {
            addon_id: addon_id.clone(),
            permission_id: perm.id.clone(),
            display_name: if perm.display_name.is_empty() {
                perm.id.clone()
            } else {
                perm.display_name.clone()
            },
            description: perm.description.clone(),
            risk: if perm.risk.is_empty() {
                "low".to_string()
            } else {
                perm.risk.clone()
            },
            sort_order: idx as i32,
        };
        repository::upsert_permission_catalog(db, &entry)?;
        keep_ids.push(perm.id.clone());
    }

    // Every addon exposing at least one `[[tool]]` also needs the "llm" entry:
    // that permission decides whether an agent may see and call those tools, and
    // no addon declares it in `[[permission]]`. Without a catalog row the admin
    // matrix (which renders catalog entries only) has nothing to click, so the
    // grant is unreachable and the tools stay invisible to every non-admin. The
    // entry is catalogued, never granted — deny-by-default is unchanged.
    if !manifest.tools.is_empty()
        && !manifest
            .declared_permissions
            .iter()
            .any(|p| p.id == crate::addon::permissions::LLM_PERMISSION_ID)
    {
        let entry = repository::DbAddonPermissionCatalogEntry {
            addon_id: addon_id.clone(),
            permission_id: crate::addon::permissions::LLM_PERMISSION_ID.to_string(),
            display_name: "Udostepnij narzedzia addonu agentom AI".to_string(),
            description: format!(
                "Pozwala agentom i modelom wywolywac narzedzia tego addonu ({}). \
                 Bez tej zgody narzedzia nie pojawiaja sie w katalogu agenta.",
                manifest
                    .tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            risk: "high".to_string(),
            sort_order: manifest.declared_permissions.len() as i32,
        };
        repository::upsert_permission_catalog(db, &entry)?;
        keep_ids.push(entry.permission_id);
    }

    // `keep_ids` carries the synthetic entry too, so the diff-delete below never
    // removes it.
    repository::delete_permission_catalog_missing(db, addon_id, &keep_ids)?;

    // 2. OAuth providers — upsert deklaracji
    for prov in &manifest.oauth_provider {
        if prov.id.is_empty() {
            continue;
        }
        let decl = repository::DbAddonOAuthProviderDecl {
            addon_id: addon_id.clone(),
            provider_id: prov.id.clone(),
            display_name: prov.display_name.clone(),
            authorize_url: prov.authorize_url.clone(),
            token_url: prov.token_url.clone(),
            revoke_url: prov.revoke_url.clone(),
            scopes: prov.scopes.join(" "),
            mode: prov.mode.clone(),
            pkce: prov.pkce,
        };
        repository::upsert_oauth_providers_decl(db, &decl)?;
    }

    // 3. Widocznosc: admin_only + domyslne grupy
    if let Some(v) = &manifest.visibility {
        repository::set_addon_admin_only(db, addon_id, v.admin_only)?;
        for group_name in &v.default_groups {
            if let Some(gid) = repository::get_group_id_by_name(db, group_name)? {
                repository::set_addon_visibility(db, addon_id, &gid, true, None)?;
            }
        }
    }

    Ok(())
}

// =============================================================================
// uninstall — deinstalacja addonu
// =============================================================================

/// Odinstalowuje addon — usuwa z DB i czysci storage.
///
/// Kroki:
/// 1. Sprawdz czy addon istnieje
/// 2. Usun z tabel powiazanych (addon_permissions, addon_secrets, addon_resource_limits, addon_config)
/// 3. Usun z addons
pub fn uninstall(addon_id: &str, db: &DbPool) -> Result<()> {
    // RAG E2.0 — usuń published modele flow_engine tej instancji (wiersze `flows`
    // + wiązania) ZANIM skasujemy wiersz addona. Manifest instancji niesie listę
    // `[[engine_flow]]`; czytamy go ze stored `addons.manifest_json`. Inwariant
    // izolacji: nazwy są `{addon_id}:{id}`, więc kasujemy wyłącznie flow tej
    // instancji. Best-effort — błąd parsowania manifestu nie blokuje uninstall.
    if let Ok(Some(addon)) = crate::db::repository::get_addon(db, addon_id) {
        if let Ok(manifest) = parse_manifest_toml(&addon.manifest_json) {
            unregister_engine_flows(db, &manifest);
        }
    }

    // B2 (RAG): graf kasujemy PRZED wzięciem write-locka na `db` i otwarciem
    // transakcji DB. `delete_all_for_addon` używa tej samej `DbPool` (read+write
    // lock), więc trzymanie tu `db.write()` zakleszczyłoby się na własnym RwLocku.
    // Cleanup robi close-handle → pliki → wiersz (files-before-row, jak
    // `seal_key_for_delete`), więc wiersze `addon_graph_collections` znikają tą
    // ścieżką, NIE generycznym DELETE w transakcji poniżej — inaczej zostałyby
    // osierocone pliki `.cozo` bez wierszy rejestru. Błąd propagujemy (nie
    // połykamy), spójnie z `uninstall_instance`.
    #[cfg(feature = "graph")]
    {
        let org_id = crate::services::org::DEFAULT_ORG_ID;
        let mgr = crate::services::graph_manager(db);
        mgr.delete_all_for_addon(org_id, addon_id).map_err(|e| {
            anyhow::anyhow!("uninstall: graph cleanup dla '{addon_id}' nieudany: {e}")
        })?;
    }

    let conn = db.write().unwrap();

    // Sprawdz czy addon istnieje
    let exists: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .unwrap_or(false);

    if !exists {
        bail!("Addon '{}' nie jest zainstalowany", addon_id);
    }

    conn.execute("BEGIN TRANSACTION", [])?;

    // Usun w kolejnosci (foreign keys CASCADE powinno to zalatwic,
    // ale robimy explicite dla pewnosci)
    // VULN-039: Dodano addon_storage — pełne czyszczenie danych przy deinstalacji
    let tables = [
        "addon_storage",
        "addon_permissions",
        "addon_secrets",
        "addon_resource_limits",
        "addon_config",
        "addon_network_rules",
        // Bez tego ponowny install innej wersji o tej samej nazwie pliku
        // migracji ale roznym hashu trafia na "hash mismatch" guard.
        "addon_migrations_applied",
    ];

    for table in &tables {
        conn.execute(
            &format!("DELETE FROM {} WHERE addon_id = ?1", table),
            rusqlite::params![addon_id],
        )
        .ok(); // Ignoruj bledy — tabela moze nie istniec jeszcze
    }

    // Glowna tabela addons
    conn.execute(
        "DELETE FROM addons WHERE addon_id = ?1",
        rusqlite::params![addon_id],
    )
    .map_err(|e| anyhow::anyhow!("Nie udalo sie usunac addonu z DB: {e}"))?;

    conn.execute("COMMIT", [])?;
    drop(conn);

    // The materialized skill row follows its addon out of the registry; the
    // delete also emits a core.skill sync capture so peers drop it. Best-effort:
    // the addon itself is already gone, so a failure here only logs.
    if let Err(e) = crate::db::repository::delete_addon_skills(db, addon_id) {
        warn!("Addon '{addon_id}': failed to remove materialized skill: {e}");
    }

    // F1a §6.5 M1.W4: zamknij per-addon SQLite pool. Plik data.db pozostaje
    // na dysku (user moze chciec backup) — czyszczenie tylko manualne.
    // F2 P1.b — uninstall is single-tenant in P1.b (org-default). Per-org
    // uninstall lands with the CLI in P1.c.
    crate::addon::storage_sql::close_addon_db(crate::services::org::DEFAULT_ORG_ID, addon_id);

    // F1c P5 — drop any compiled flows this addon registered so a later
    // invoke against a stale id reports "not found" instead of executing a
    // template owned by an addon that no longer exists.
    crate::flow_runtime::registry::global().unregister_addon(addon_id);

    info!("Addon '{}' odinstalowany", addon_id);

    Ok(())
}

// =============================================================================
// F2 P1.b — boot-time migration of pre-F2 addon dirs to per-org layout
// =============================================================================

/// Move every legacy `<home>/.tentaflow/addons/<addon_id>/` directory into the
/// new per-org layout `<home>/.tentaflow/orgs/org-default/addons/<addon_id>/`.
/// Called from the boot path AFTER `db::migrations::run` so the DB v32
/// backfill has already promoted every row to `org-default`.
///
/// Idempotent: a second invocation finds the legacy root already absent (or
/// empty) and returns 0. Returns the number of addon dirs that were moved
/// successfully. IO failures on individual entries are logged and skipped —
/// the boot does not abort on a single stuck dir (e.g. open file handle on
/// Windows). Missing legacy root → returns Ok(0).
pub fn migrate_addon_dirs_to_org_default(home: &std::path::Path) -> std::io::Result<usize> {
    let legacy_root = home.join(".tentaflow").join("addons");
    if !legacy_root.exists() {
        return Ok(0);
    }
    let target_root = home
        .join(".tentaflow")
        .join("orgs")
        .join(crate::services::org::DEFAULT_ORG_ID)
        .join("addons");
    std::fs::create_dir_all(&target_root)?;

    let mut moved = 0usize;
    for entry in std::fs::read_dir(&legacy_root)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("migrate_addon_dirs: read_dir entry skipped: {e}");
                continue;
            }
        };
        let path = entry.path();
        let name = match path.file_name().and_then(|s| s.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        // `symlink_metadata` does NOT follow links — a symlinked addon dir
        // (operator's manual customisation, e.g. linking into a dev tree)
        // must not be silently moved or dereferenced. Warn and skip so the
        // operator can reconcile by hand; following the link would corrupt
        // both the legacy path and whatever it pointed at.
        let lstat = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "migrate_addon_dirs: symlink_metadata('{}') failed: {} — skipping",
                    name,
                    e
                );
                continue;
            }
        };
        if lstat.file_type().is_symlink() {
            tracing::warn!(
                "migrate_addon_dirs: '{}' is a symlink — skipping (manual move required)",
                name
            );
            continue;
        }
        // Skip non-directory entries (stray files from manual operator
        // intervention should not block the migration).
        if !lstat.is_dir() {
            continue;
        }
        let dest = target_root.join(&name);
        if dest.exists() {
            // Target already populated — refuse to continue. A collision
            // means the operator has a second copy of the same addon under
            // the per-org root; silently leaving the legacy dir in place
            // would hide the inconsistency until a later boot fails. Stop
            // the migration so a human reconciles before the daemon comes
            // up.
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "migrate_addon_dirs: '{}' exists at both legacy and per-org paths",
                    name
                ),
            ));
        }
        match std::fs::rename(&path, &dest) {
            Ok(_) => {
                moved += 1;
                tracing::info!("migrate_addon_dirs: '{}' moved to per-org layout", name);
            }
            Err(e) => {
                // `rename` across filesystem boundaries fails with EXDEV on
                // Linux. The legacy root and target are siblings under the
                // same `.tentaflow` tree so this should not happen, but log
                // and continue rather than abort the entire boot.
                tracing::warn!(
                    "migrate_addon_dirs: rename '{}' failed: {} — manual move required",
                    name,
                    e
                );
            }
        }
    }

    // Try to drop the now-empty legacy root. Failure is non-fatal.
    let _ = std::fs::remove_dir(&legacy_root);
    Ok(moved)
}

// =============================================================================
// upgrade — aktualizacja addonu
// =============================================================================

/// Rdzen aktualizacji instancji do nowej, JUZ skatalogowanej wersji jej pakietu.
/// `new_dir` to katalog wersji w store'ie (`packages/{package_id}/{version}/`),
/// `new_manifest`/`manifest_content` niosa tozsamosc docelowa
/// (`new_manifest.addon_id` == `addon_id`). `package_id`/`package_version`
/// wskazuja wersjonowany pakiet (resolucja wasm/migracji + kolumny
/// `addons.package_*`). Pakiet i jego bajty musza juz byc w store'ie/katalogu
/// (zasilone przez upload do katalogu albo reconcile sync) — ta funkcja tylko
/// przepina instancje. Zwraca docelowy manifest (do re-rejestracji runtime).
fn upgrade_core(
    addon_id: &str,
    new_dir: &Path,
    db: &DbPool,
    new_manifest: AddonManifest,
    manifest_content: &str,
    package_id: &str,
    package_version: &str,
) -> Result<AddonManifest> {
    validate_manifest(&new_manifest)?;

    if new_manifest.addon_id != addon_id {
        bail!(
            "addon_id w manifescie ('{}') nie zgadza sie z '{}' ",
            new_manifest.addon_id,
            addon_id
        );
    }

    // Odczytaj nowy WASM
    let wasm_path = new_dir.join(&new_manifest.wasm_file);

    // CR-010: Ochrona przed path traversal
    if let Ok(canonical) = wasm_path.canonicalize() {
        if let Ok(base) = new_dir.canonicalize() {
            if !canonical.starts_with(&base) {
                bail!(
                    "Path traversal wykryty w wasm_file: {:?}",
                    new_manifest.wasm_file
                );
            }
        }
    }

    if !wasm_path.exists() {
        bail!("Brak pliku WASM: {:?}", wasm_path);
    }

    // Runtime (get_or_compile_module) rozwiazuje wasm/migracje po (package_id,
    // package_version) ustawianym ponizej; ta wersja jest juz w store'ie.
    let package_dir = crate::addon::bundled::package_dir(package_id, package_version);

    // F1c P5 — compile the new flow templates BEFORE touching the DB. Any
    // compile error (cycle, schema, missing file) aborts the upgrade with the
    // old registry entries intact. Registry swap happens at the bottom, after
    // every fallible step.
    let new_compiled_flows = compile_flow_templates(&new_manifest, new_dir)?;

    // Size is captured from the WASM file on disk; metadata() avoids reading
    // the module contents twice (install() does a full read for validation,
    // upgrade() trusts the lifecycle path traversal check above).
    let wasm_size = std::fs::metadata(&wasm_path)
        .map(|m| m.len() as i64)
        .unwrap_or(0);

    let platforms_json = serde_json::to_string(&new_manifest.platforms)?;

    let icon = new_manifest.icon.as_deref().unwrap_or("");
    let runtime = new_manifest.runtime.as_deref().unwrap_or("wasmtime");
    let category = new_manifest.category.as_deref().unwrap_or("");
    let license = new_manifest.license.as_deref().unwrap_or("");
    let show_in_catalog = new_manifest.show_in_catalog.unwrap_or(true) as i64;

    // The new package version owns the skill: refresh `addons.skill_md` so the
    // row mirrors the package (install does the same) and the skills-registry
    // materialization below sees the updated content.
    let new_skill_md = std::fs::read_to_string(new_dir.join("SKILL.md")).ok();

    let old_version: String = {
        let conn = db.read().unwrap();
        conn.query_row(
            "SELECT version FROM addons WHERE addon_id = ?1",
            rusqlite::params![addon_id],
            |row| row.get(0),
        )
        .map_err(|e| anyhow::anyhow!("Addon nie znaleziony: {e}"))?
    };

    if matches!(new_manifest.storage.as_ref(), Some(s) if s.sql) {
        apply_addon_sql_migrations(&new_manifest, &package_dir, db)?;
    }

    // Hash katalogowy wersji docelowej — zapisany na instancji, zeby po
    // aktualizacji detekcja przestala raportowac "dostepna aktualizacja"
    // (i znow ja zaraportowala przy kolejnej zmianie tresci). Pobrane przed
    // lockiem, zeby nie zagniezdzac blokady DB.
    let installed_bundle_hash =
        crate::db::repository::get_addon_package(db, package_id, package_version)
            .ok()
            .flatten()
            .map(|p| p.bundle_hash)
            .unwrap_or_default();

    let conn = db.write().unwrap();
    conn.execute("BEGIN TRANSACTION", [])?;

    info!(
        "Upgrade addonu '{}': {} -> {}",
        addon_id, old_version, new_manifest.version
    );

    // Zaktualizuj metadane addonu (w tym UI metadata z migracji 43 + 44).
    conn.execute(
        "UPDATE addons SET version = ?1, name = ?2, description = ?3, author = ?4, \
         manifest_json = ?5, platforms = ?6, category = ?7, icon = ?8, runtime = ?9, \
         wasm_size_bytes = ?10, license = ?11, show_in_catalog = ?12, \
         package_version = ?13, skill_md = ?14, installed_bundle_hash = ?15, \
         updated_at = datetime('now') \
         WHERE addon_id = ?16",
        rusqlite::params![
            &new_manifest.version,
            &new_manifest.display_name,
            &new_manifest.description.as_deref().unwrap_or(""),
            &new_manifest.author.as_deref().unwrap_or(""),
            manifest_content,
            &platforms_json,
            category,
            icon,
            runtime,
            wasm_size,
            license,
            show_in_catalog,
            package_version,
            &new_skill_md,
            &installed_bundle_hash,
            addon_id,
        ],
    )?;

    // Limity zasobow — jesli nowy manifest deklaruje [resources], zaktualizuj; inaczej zachowaj istniejace
    if let Some(ref res) = new_manifest.resources {
        conn.execute(
            "INSERT OR REPLACE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, document_storage_mb, http_requests_per_min, \
              llm_tokens_per_min, fuel_limit) \
             VALUES (?1, 0, 0, ?2, 1, 0, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                addon_id,
                res.memory_mb.unwrap_or(0) as i64,
                res.storage_total_mb.unwrap_or(0) as i64,
                res.document_storage_mb.unwrap_or(0) as i64,
                res.http_requests_per_minute.unwrap_or(0) as i64,
                res.llm_tokens_per_minute.unwrap_or(0) as i64,
                res.fuel_limit.unwrap_or(0) as i64,
            ],
        )
        .ok();
    } else {
        conn.execute(
            "INSERT OR IGNORE INTO addon_resource_limits \
             (addon_id, max_instances, cpu_limit_ms_per_min, ram_limit_mb, gpu_enabled, \
              vram_limit_mb, storage_limit_mb, http_requests_per_min, llm_tokens_per_min) \
             VALUES (?1, 0, 0, 0, 1, 0, 0, 0, 0)",
            rusqlite::params![addon_id],
        )
        .ok();
    }

    // Synchronizacja regul sieciowych:
    // - Zachowaj approved status istniejacych regul (juz zatwierdzonych przez admina)
    // - Dodaj nowe reguly z approved=0 (wymagaja zatwierdzenia)
    // - Usun reguly ktore nie istnieja w nowym manifescie
    sync_network_rules(&conn, addon_id, &new_manifest.network_rules)?;

    conn.execute("COMMIT", [])?;
    drop(conn);

    // Synchronizacja metadanych z manifestu (permission catalog, oauth providers, visibility)
    sync_manifest_metadata(db, &new_manifest)?;

    // The skills-registry row tracks the package: re-materialize from the new
    // SKILL.md (or drop the row when the new version removed the file).
    materialize_addon_skill(db, &new_manifest, new_dir);

    // Reconcile vector-namespace metadata schemas against the new manifest:
    // add/drop typed columns on collections that already exist so a declared
    // schema change in `[[vector_namespace]].fields` is applied on upgrade.
    reconcile_vector_namespaces(db, &new_manifest);

    // F1c P5 — atomically swap compiled flows: drop every previous-version
    // entry for this addon and publish the new set under a single write lock,
    // so no concurrent flow_invoke_v1 ever observes a partial publish (no
    // not-found-then-found window). In-flight invocations holding an Arc to
    // the old CompiledFlow keep running against the old graph until they
    // finish.
    crate::flow_runtime::registry::global().replace_addon_flows(addon_id, new_compiled_flows);

    // RAG E2.0 — hot-update instancji (zmiana bundla przy tej samej wersji)
    // odswieza tylko rejestr `flow_runtime` powyzej, ale `[[engine_flow]]` zyja
    // jako published modele flow_engine + KV `engine_flow_model:<id>`. Bez tego
    // wywolania nowy `[[engine_flow]]` z manifestu (np. `ingest`) nigdy nie
    // trafia do publikacji na hot-update — tylko pelna instalacja / mesh
    // reconcile go materializuja (materialize_addon_derived_state). Funkcja jest
    // idempotentna (usuwa stary published flow + binding i tworzy od nowa wraz z
    // synchronicznym flushem KV), wiec re-rejestracja query/retrieval-round jest
    // bezpieczna. Manifest i katalog jak reszta upgrade'u: `new_manifest`/`new_dir`.
    register_engine_flows(db, &new_manifest, new_dir)?;

    info!(
        "Addon '{}' zaktualizowany do v{}",
        addon_id, new_manifest.version
    );

    Ok(new_manifest)
}

/// Aktualizuje INSTANCJE do innej (juz skatalogowanej) wersji jej pakietu.
/// W przeciwienstwie do `upgrade` (model 1:1, materializacja z `new_dir`) tu
/// wersja docelowa jest juz w katalogu i w store'ie (zasilona przez reconciler),
/// a `package_id` rozni sie od `addon_id`. Nie kopiuje plikow — reuzywa istniejacy
/// katalog pakietu, aplikuje brakujace migracje do wlasnego SQLite instancji i
/// zwraca docelowy manifest (manager re-rejestruje toole/flow bloki). NIE rusza
/// uruchomionych instancji wasm — to robi warstwa managera (hot reload).
pub fn update_instance(db: &DbPool, addon_id: &str, target_version: &str) -> Result<AddonManifest> {
    let (package_id, current_version) =
        crate::db::repository::get_addon_instance_package_ref(db, addon_id)?
            .ok_or_else(|| anyhow::anyhow!("instancja '{addon_id}' nie istnieje"))?;

    let pkg = crate::db::repository::get_addon_package(db, &package_id, target_version)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "wersja '{target_version}' pakietu '{package_id}' nie istnieje w katalogu"
            )
        })?;
    let pkg_dir = crate::addon::bundled::package_dir(&package_id, target_version);
    if !pkg_dir.join("manifest.toml").exists() {
        bail!(
            "pliki wersji '{target_version}' pakietu '{package_id}' nie istnieja w store ({:?})",
            pkg_dir
        );
    }

    // Zachowaj nazwe instancji nadana przez usera (kolumna addons.name ==
    // display_name instancji), zeby update nie nadpisal jej nazwa pakietu.
    let display_name = crate::db::repository::get_addon(db, addon_id)?
        .map(|a| a.name)
        .unwrap_or_else(|| addon_id.to_string());

    // Manifest docelowy = manifest wersji pakietu z tozsamoscia instancji ORAZ
    // podstawionymi ${key} z istniejacej konfiguracji instancji (np. IP robota),
    // zeby update nie zostawil niepodstawionego placeholdera w hostach regul.
    let config: std::collections::BTreeMap<String, String> =
        crate::db::repository::list_addon_config_rows(db, addon_id)?
            .into_iter()
            .filter(|row| !row.is_secret)
            .map(|row| (row.key, row.value))
            .collect();
    let instance_manifest =
        rewrite_manifest_for_instance(&pkg.manifest_json, addon_id, &display_name, &config)?;
    let new_manifest = parse_manifest_toml(&instance_manifest)
        .map_err(|e| anyhow::anyhow!("manifest docelowej wersji niepoprawny: {e}"))?;

    info!(
        "Aktualizacja instancji '{}' ({}): v{} -> v{}",
        addon_id, package_id, current_version, target_version
    );

    upgrade_core(
        addon_id,
        &pkg_dir,
        db,
        new_manifest,
        &instance_manifest,
        &package_id,
        target_version,
    )
}

// =============================================================================
// Walidacja manifestu
// =============================================================================

/// Valid risk levels for declared permissions.
const VALID_RISK: &[&str] = &["low", "medium", "high", "critical"];

/// Legacy manifest sections that are explicitly rejected to prevent silent
/// acceptance of mixed formats. Addons must be rewritten to the canonical
/// format (see SCHEMA in repository docs).
const LEGACY_SECTIONS: &[&str] = &[
    "permissions",       // old [permissions] with required/optional category lists
    "addon_permissions", // old [[addon_permissions]] array
    "network_rules",     // old [[network_rules]] (singular in new format)
    "tools",             // old [tools.name] nested subtables
];

/// Validates a parsed manifest — required fields, permission risk levels,
/// unique network rule ids, non-empty tool fields.
fn validate_manifest(manifest: &AddonManifest) -> Result<()> {
    if manifest.addon_id.is_empty() {
        bail!("addon.id is empty");
    }
    if manifest.addon_id.len() > 128 {
        bail!("addon.id too long (max 128 chars)");
    }
    if !manifest
        .addon_id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        bail!("addon.id contains disallowed characters (allowed: a-z, 0-9, '.', '-', '_')");
    }
    if manifest.version.is_empty() {
        bail!("addon.version is empty");
    }
    if manifest.display_name.is_empty() {
        bail!("addon.name is empty");
    }
    if manifest.wasm_file.is_empty() {
        bail!("addon.wasm_file is empty");
    }

    let mut perm_ids = std::collections::HashSet::new();
    for perm in &manifest.declared_permissions {
        if perm.id.is_empty() {
            bail!("permission.id is empty");
        }
        if !perm_ids.insert(&perm.id) {
            bail!("duplicate permission.id: '{}'", perm.id);
        }
        if perm.display_name.is_empty() {
            bail!("permission '{}': display_name is empty", perm.id);
        }
        if !VALID_RISK.contains(&perm.risk.as_str()) {
            bail!(
                "permission '{}': risk must be low|medium|high|critical (got '{}')",
                perm.id,
                perm.risk
            );
        }
    }

    for tool in &manifest.tools {
        if tool.name.is_empty() {
            bail!("tool.id is empty");
        }
        if tool.description.is_empty() {
            bail!("tool '{}': description is empty", tool.name);
        }
    }

    let mut rule_ids = std::collections::HashSet::new();
    for rule in &manifest.network_rules {
        if rule.id.is_empty() {
            bail!("network_rule.id is empty");
        }
        if !rule_ids.insert(&rule.id) {
            bail!("duplicate network_rule.id: '{}'", rule.id);
        }
        if rule.host.is_empty() {
            bail!("network_rule '{}': host is empty", rule.id);
        }
        if rule.host.contains('*')
            && rule.host != "*"
            && !(rule.host.starts_with("*.") && !rule.host[2..].contains('*'))
        {
            bail!(
                "network_rule '{}': wildcard host must be '*' or '*.domain'",
                rule.id
            );
        }
        if rule.port == 0 {
            bail!("network_rule '{}': port must be 1-65535", rule.id);
        }
        if rule.protocol != "tcp" && rule.protocol != "udp" {
            bail!(
                "network_rule '{}': protocol must be 'tcp' or 'udp'",
                rule.id
            );
        }
    }

    for prov in &manifest.oauth_provider {
        if prov.id.is_empty() {
            bail!("oauth_provider.id is empty");
        }
        if prov.authorize_url.is_empty() || prov.token_url.is_empty() {
            bail!(
                "oauth_provider '{}': authorize_url and token_url must be set",
                prov.id
            );
        }
        if !matches!(prov.mode.as_str(), "global" | "individual" | "none") {
            bail!(
                "oauth_provider '{}': mode must be global|individual|none",
                prov.id
            );
        }
    }

    Ok(())
}

/// Parses the canonical manifest format:
/// - `[addon]` section holding id/name/version/wasm_file/... (required).
/// - `[[permission]]` array of declared granular permissions.
/// - `[[tool]]` array with optional nested `[[tool.parameter]]` items.
/// - `[[oauth_provider]]`, `[[network_rule]]` arrays.
/// - Sections `[visibility]`, `[resources]`, `[lifecycle]`, `[config.schema]`.
///
/// Legacy sections (`[permissions]`, `[[addon_permissions]]`, singular `[tools.X]`,
/// `[[network_rules]]`) are rejected with a clear error — addons must migrate to
/// the canonical format instead of relying on backward-compat shims.
pub fn parse_manifest_toml(content: &str) -> Result<AddonManifest> {
    let parsed: toml::Value =
        toml::from_str(content).map_err(|e| anyhow::anyhow!("invalid TOML: {e}"))?;

    let top = parsed
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("manifest root must be a TOML table"))?;

    for legacy in LEGACY_SECTIONS {
        if top.contains_key(*legacy) {
            bail!(
                "manifest uses legacy section '[{}]' — migrate to the canonical format \
                 ([[permission]], [[tool]], [[network_rule]])",
                legacy
            );
        }
    }

    let addon = top
        .get("addon")
        .and_then(|v| v.as_table())
        .ok_or_else(|| anyhow::anyhow!("missing [addon] section"))?;

    let addon_id = addon
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing addon.id"))?
        .to_string();
    let version = addon
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing addon.version"))?
        .to_string();
    let display_name = addon
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(&addon_id)
        .to_string();
    let description = addon
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from);
    let author = addon
        .get("author")
        .and_then(|v| v.as_str())
        .map(String::from);
    let wasm_file = addon
        .get("wasm_file")
        .and_then(|v| v.as_str())
        .unwrap_or("addon.wasm")
        .to_string();
    let platforms = addon
        .get("platforms")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let keywords = addon
        .get("keywords")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let category = addon
        .get("category")
        .and_then(|v| v.as_str())
        .map(String::from);
    let icon = addon.get("icon").and_then(|v| v.as_str()).map(String::from);
    let runtime = addon
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(ref rt) = runtime {
        if !crate::addon::runtime::KNOWN_RUNTIMES.contains(&rt.as_str()) {
            anyhow::bail!(
                "unknown addon runtime '{}', expected one of: {}",
                rt,
                crate::addon::runtime::KNOWN_RUNTIMES.join(", ")
            );
        }
    }
    let license = addon
        .get("license")
        .and_then(|v| v.as_str())
        .map(String::from);

    let declared_permissions = top
        .get("permission")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| AddonDeclaredPermission {
                    id: p
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    display_name: p
                        .get("display_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    description: p
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    risk: p
                        .get("risk")
                        .and_then(|v| v.as_str())
                        .unwrap_or("low")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let oauth_provider = top
        .get("oauth_provider")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let id = p.get("id").and_then(|v| v.as_str())?.to_string();
                    Some(AddonOAuthProviderSection {
                        id,
                        display_name: p
                            .get("display_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        authorize_url: p
                            .get("authorize_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        token_url: p
                            .get("token_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        revoke_url: p
                            .get("revoke_url")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                        scopes: p
                            .get("scopes")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|s| s.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        mode: p
                            .get("mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or("individual")
                            .to_string(),
                        pkce: p.get("pkce").and_then(|v| v.as_bool()).unwrap_or(true),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let network_rules = top
        .get("network_rule")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|r| ManifestNetworkRule {
                    id: r
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    protocol: r
                        .get("protocol")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tcp")
                        .to_string(),
                    host: r
                        .get("host")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    port: r.get("port").and_then(|v| v.as_integer()).unwrap_or(443) as u16,
                    description: r
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    required: r.get("required").and_then(|v| v.as_bool()).unwrap_or(true),
                })
                .collect()
        })
        .unwrap_or_default();

    let tools = top
        .get("tool")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    let id = t
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let description = t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let keywords_t = t
                        .get("keywords")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let parameters: Vec<ManifestToolParameter> = t
                        .get("parameter")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .map(|p| ManifestToolParameter {
                                    name: p
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    param_type: p
                                        .get("param_type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("string")
                                        .to_string(),
                                    description: p
                                        .get("description")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    required: p
                                        .get("required")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ManifestTool {
                        name: id,
                        description,
                        parameters_schema: build_parameters_schema(&parameters),
                        return_schema: None,
                        keywords: keywords_t,
                        read_only: t
                            .get("read_only")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    let visibility = top.get("visibility").map(|v| AddonVisibilitySection {
        admin_only: v
            .get("admin_only")
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        default_groups: v
            .get("default_groups")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        show_in_catalog: v.get("show_in_catalog").and_then(|x| x.as_bool()),
    });

    // `[visibility].show_in_catalog` controls the top-level flag stored in the
    // addons table; falls back to `[addon].show_in_catalog` if someone puts it there.
    let show_in_catalog = visibility
        .as_ref()
        .and_then(|v| v.show_in_catalog)
        .or_else(|| addon.get("show_in_catalog").and_then(|v| v.as_bool()));

    let disambiguation = addon
        .get("disambiguation")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let trigger = item
                        .get("trigger")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let prefer = item.get("prefer").and_then(|v| v.as_str())?.to_string();
                    let over = item.get("over").and_then(|v| v.as_str())?.to_string();
                    let when = item
                        .get("when")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(DisambiguationRule {
                        trigger,
                        prefer,
                        over,
                        when,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let service = top.get("service").and_then(|v| v.as_table()).map(|svc| {
        crate::addon::AddonServiceSection {
            enabled: svc.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
            tick_interval_ms: svc
                .get("tick_interval_ms")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
            tick_fuel_budget: svc
                .get("tick_fuel_budget")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
            tick_timeout_ms: svc
                .get("tick_timeout_ms")
                .and_then(|v| v.as_integer())
                .map(|v| v as u64),
        }
    });

    let application = match top.get("application") {
        None => None,
        Some(v) => {
            let tbl = v
                .as_table()
                .ok_or_else(|| anyhow::anyhow!("[application] must be a TOML table"))?;
            let entry_panel = tbl
                .get("entry_panel")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("[application] missing entry_panel (string)"))?
                .to_string();
            let title = tbl
                .get("title")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("[application] missing title (string)"))?
                .to_string();
            let icon = tbl
                .get("icon")
                .and_then(|x| x.as_str())
                .ok_or_else(|| anyhow::anyhow!("[application] missing icon (string)"))?
                .to_string();
            let description = tbl
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let sort_order = match tbl.get("sort_order") {
                None => 100,
                Some(toml::Value::Integer(n)) => {
                    if !(i32::MIN as i64..=i32::MAX as i64).contains(n) {
                        bail!("application.sort_order {n} outside i32 range");
                    }
                    *n as i32
                }
                Some(other) => bail!(
                    "application.sort_order must be integer (got {})",
                    other.type_str()
                ),
            };
            let section = crate::addon::AddonApplicationSection {
                entry_panel,
                title,
                icon,
                description,
                sort_order,
            };
            section.validate()?;
            Some(section)
        }
    };

    let sdk_version = addon
        .get("sdk_version")
        .and_then(|v| v.as_str())
        .map(String::from);

    let storage = parse_storage_section(top.get("storage"))?;
    let aliases = parse_aliases(top.get("alias"))?;
    let gates = parse_gates(top.get("gate"))?;
    let vector_namespaces = parse_vector_namespaces(top.get("vector_namespace"))?;
    let graph_collections = parse_graph_collections(top.get("graph_collection"))?;
    let flow_templates = parse_flow_templates(top.get("flow_template"))?;
    let engine_flows = parse_engine_flows(top.get("engine_flow"))?;
    let ui_components = parse_ui_components(top.get("ui_component"))?;
    let gpu = parse_gpu_section(top.get("gpu"));
    let uses_aliases = parse_uses_aliases(top.get("uses_alias"))?;
    let uses_models = parse_uses_models(top.get("uses_model"))?;
    let publisher = parse_publisher_section(top.get("publisher"))?;
    let runtime_overrides = parse_runtime_section(top.get("runtime"))?;
    let robot = parse_robot_section(top.get("robot"));

    crate::addon::manifest::validate_manifest_extensions(
        storage.as_ref(),
        &aliases,
        &gates,
        &vector_namespaces,
        &flow_templates,
        &ui_components,
        sdk_version.as_deref(),
        &uses_aliases,
        &uses_models,
        publisher.as_ref(),
    )?;
    crate::addon::manifest::validate_graph_collections(&graph_collections)?;

    let resources = top.get("resources").map(|res| ResourceRequirements {
        storage_total_mb: res
            .get("storage_total_mb")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("storage_mb").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        storage_value_mb: res
            .get("storage_value_mb")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64),
        document_storage_mb: res
            .get("document_storage_mb")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64),
        llm_tokens_per_minute: res
            .get("llm_tokens_per_minute")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("llm_tokens_per_min").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        http_requests_per_minute: res
            .get("http_requests_per_minute")
            .and_then(|v| v.as_integer())
            .or_else(|| {
                res.get("http_requests_per_min")
                    .and_then(|v| v.as_integer())
            })
            .map(|v| v as u64),
        memory_mb: res
            .get("memory_mb")
            .and_then(|v| v.as_integer())
            .or_else(|| res.get("ram_mb").and_then(|v| v.as_integer()))
            .map(|v| v as u64),
        fuel_limit: res
            .get("fuel_limit")
            .and_then(|v| v.as_integer())
            .map(|v| v as u64),
    });

    Ok(AddonManifest {
        addon_id,
        version,
        display_name,
        description,
        author,
        platforms,
        wasm_file,
        keywords,
        category,
        icon,
        runtime,
        tools,
        declared_permissions,
        network_rules,
        disambiguation,
        resources,
        visibility,
        oauth_provider,
        license,
        show_in_catalog,
        service,
        application,
        storage,
        aliases,
        gates,
        vector_namespaces,
        graph_collections,
        flow_templates,
        engine_flows,
        ui_components,
        gpu,
        sdk_version,
        uses_aliases,
        uses_models,
        publisher,
        runtime_overrides,
        robot,
    })
}

/// Parses the optional top-level `[robot]` TOML table. Only the fields the
/// cross-node robot-control receiver needs are read (`controls_robot`, `kind`,
/// `[robot.safety]`). Absent section → `None`.
fn parse_robot_section(value: Option<&toml::Value>) -> Option<crate::addon::RobotManifestSection> {
    let table = value?.as_table()?;
    let safety =
        table
            .get("safety")
            .and_then(|s| s.as_table())
            .map(|st| crate::addon::RobotSafetySection {
                max_linear_mps: st.get("max_linear_mps").and_then(|v| v.as_float()),
                max_yaw_rps: st.get("max_yaw_rps").and_then(|v| v.as_float()),
                require_estop_clear: st.get("require_estop_clear").and_then(|v| v.as_bool()),
            });
    Some(crate::addon::RobotManifestSection {
        controls_robot: table
            .get("controls_robot")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        kind: table
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        safety,
    })
}

/// Parses the optional top-level `[runtime]` TOML table into a
/// `RuntimeSection`. Distinct from the `addon.runtime` scalar (which names
/// the wasm engine — `"wasmtime"` / `"wasmi"`). Strict on field types: a
/// non-integer `max_concurrency` or `rate_limit_per_min` is a hard parse
/// error rather than a silent default, so a manifest typo cannot mask a
/// regression in addon tuning.
fn parse_runtime_section(
    val: Option<&toml::Value>,
) -> Result<Option<crate::addon::manifest::RuntimeSection>> {
    let Some(v) = val else {
        return Ok(None);
    };
    let t = v
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("[runtime] must be a TOML table"))?;
    let max_concurrency = match t.get("max_concurrency") {
        Some(toml::Value::Integer(n)) => {
            if *n < 0 {
                bail!("runtime.max_concurrency must be >= 0 (got {n})");
            }
            // Treat 0 as "no override" — the default cap stays in force.
            if *n == 0 {
                None
            } else {
                Some(*n as u32)
            }
        }
        Some(other) => bail!(
            "runtime.max_concurrency must be an integer (got {})",
            other.type_str()
        ),
        None => None,
    };
    let rate_limit_per_min = match t.get("rate_limit_per_min") {
        Some(toml::Value::Integer(n)) => {
            if *n < 0 {
                bail!("runtime.rate_limit_per_min must be >= 0 (got {n})");
            }
            if *n == 0 {
                None
            } else {
                Some(*n as u32)
            }
        }
        Some(other) => bail!(
            "runtime.rate_limit_per_min must be an integer (got {})",
            other.type_str()
        ),
        None => None,
    };
    Ok(Some(crate::addon::manifest::RuntimeSection {
        max_concurrency,
        rate_limit_per_min,
    }))
}

/// Parses optional `[publisher]` table. Strict on field types — `label` and
/// `ed25519_public_key` must be strings when present.
fn parse_publisher_section(
    val: Option<&toml::Value>,
) -> Result<Option<crate::addon::manifest::PublisherInfo>> {
    let Some(v) = val else {
        return Ok(None);
    };
    let tbl = v
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("[publisher] must be a table"))?;
    let ed25519_public_key = tbl
        .get("ed25519_public_key")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("[publisher] missing ed25519_public_key (string)"))?
        .to_string();
    let label = tbl
        .get("label")
        .and_then(|x| x.as_str())
        .ok_or_else(|| anyhow::anyhow!("[publisher] missing label (string)"))?
        .to_string();
    let contact = tbl
        .get("contact")
        .and_then(|x| x.as_str())
        .map(String::from);
    Ok(Some(crate::addon::manifest::PublisherInfo {
        ed25519_public_key,
        label,
        contact,
    }))
}

// Parsery sekcji rozszerzonych (F1a). Trzymamy je w lifecycle.rs zeby utrzymac
// jeden punkt wejscia parsowania (parse_manifest_toml) i nie dublowac iteracji
// po toml::Value w manifest.rs.

fn parse_storage_section(
    val: Option<&toml::Value>,
) -> Result<Option<crate::addon::manifest::StorageConfig>> {
    let Some(v) = val else {
        return Ok(None);
    };
    let tbl = v
        .as_table()
        .ok_or_else(|| anyhow::anyhow!("[storage] must be a table"))?;
    let cfg = crate::addon::manifest::StorageConfig {
        kv: tbl.get("kv").and_then(|v| v.as_bool()).unwrap_or(true),
        sql: tbl.get("sql").and_then(|v| v.as_bool()).unwrap_or(false),
        sql_backends: tbl
            .get("sql_backends")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        sql_dialect: tbl
            .get("sql_dialect")
            .and_then(|v| v.as_str())
            .unwrap_or("ansi")
            .to_string(),
        migrations_dir: tbl
            .get("migrations_dir")
            .and_then(|v| v.as_str())
            .unwrap_or("migrations")
            .to_string(),
        encryption: tbl
            .get("encryption")
            .and_then(|v| v.as_str())
            .unwrap_or("none")
            .to_string(),
        scope: tbl
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("org")
            .to_string(),
    };
    Ok(Some(cfg))
}

fn parse_aliases(val: Option<&toml::Value>) -> Result<Vec<crate::addon::manifest::AliasSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[alias]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let methods = item
            .get("methods")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let suggested_default = item
            .get("suggested_default")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let gate = item.get("gate").and_then(|v| v.as_str()).map(String::from);
        let visibility = match item.get("visibility").and_then(|v| v.as_str()) {
            Some(s) => crate::addon::manifest::AliasVisibility::parse(s)?,
            None => crate::addon::manifest::AliasVisibility::Private,
        };
        let allowed_consumers = item
            .get("allowed_consumers")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        out.push(crate::addon::manifest::AliasSpec {
            id,
            display_name,
            methods,
            suggested_default,
            gate,
            visibility,
            allowed_consumers,
        });
    }
    Ok(out)
}

fn parse_uses_aliases(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UsesAliasSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[uses_alias]][{idx}] missing 'id'"))?
            .to_string();
        let required = item
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::UsesAliasSpec {
            id,
            required,
            reason,
        });
    }
    Ok(out)
}

fn parse_uses_models(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UsesModelSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[uses_model]][{idx}] missing 'id'"))?
            .to_string();
        let required = item
            .get("required")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let reason = item
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::UsesModelSpec {
            id,
            required,
            reason,
        });
    }
    Ok(out)
}

fn parse_gates(val: Option<&toml::Value>) -> Result<Vec<crate::addon::manifest::GateSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[gate]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let required_claims = item
            .get("required_claims")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(parse_claim_requirement)
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        out.push(crate::addon::manifest::GateSpec {
            id,
            display_name,
            required_claims,
        });
    }
    Ok(out)
}

fn parse_claim_requirement(val: &toml::Value) -> Result<crate::addon::manifest::ClaimRequirement> {
    let claim_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("claim requirement missing 'type'"))?
        .to_string();
    Ok(crate::addon::manifest::ClaimRequirement {
        claim_type,
        subject: val
            .get("subject")
            .and_then(|v| v.as_str())
            .map(String::from),
        scope: val.get("scope").and_then(|v| v.as_str()).map(String::from),
        status: val.get("status").and_then(|v| v.as_str()).map(String::from),
        value: val.get("value").and_then(|v| v.as_str()).map(String::from),
        oneof: val
            .get("oneof")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        valid: val.get("valid").and_then(|v| v.as_bool()),
        has_expiry: val.get("has_expiry").and_then(|v| v.as_bool()),
    })
}

/// On addon upgrade, bring every already-materialized vector namespace in line
/// with the new manifest's declared `[[vector_namespace]].fields`. Iterates all
/// orgs that hold a row for each declared namespace (an addon may be installed
/// in several tenants) and reconciles each. Best-effort: a reconciliation
/// failure for one (org, namespace) is logged and the upgrade still completes —
/// the schema mismatch surfaces later as a clear filter/insert error rather
/// than aborting an otherwise-valid upgrade.
fn reconcile_vector_namespaces(db: &DbPool, manifest: &AddonManifest) {
    use tentaflow_sdk_spec::{FieldSpec, FieldType};

    if manifest.vector_namespaces.is_empty() {
        return;
    }
    let mgr = crate::services::vector::NamespaceManager::new(db.clone());
    for ns in &manifest.vector_namespaces {
        let desired: Vec<FieldSpec> = ns
            .fields
            .iter()
            .filter_map(|f| {
                let field_type = match f.field_type.as_str() {
                    "str" => FieldType::Str,
                    "int" => FieldType::Int,
                    "float" => FieldType::Float,
                    "bool" => FieldType::Bool,
                    _ => return None,
                };
                Some(FieldSpec {
                    name: f.name.clone(),
                    field_type,
                    indexed: f.indexed,
                })
            })
            .collect();

        let orgs: Vec<String> = {
            let conn = match db.read() {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut stmt = match conn.prepare(
                "SELECT org_id FROM addon_vector_namespaces WHERE addon_id = ?1 AND namespace = ?2",
            ) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rows = stmt
                .query_map(rusqlite::params![manifest.addon_id, ns.name], |r| {
                    r.get::<_, String>(0)
                })
                .and_then(|m| m.collect::<std::result::Result<Vec<_>, _>>());
            match rows {
                Ok(v) => v,
                Err(_) => continue,
            }
        };

        for org_id in orgs {
            match mgr.reconcile_namespace(&org_id, &manifest.addon_id, &ns.name, &desired) {
                Ok(report) if !report.is_noop() => info!(
                    "vector namespace '{}' (org {}, addon {}) reconciled: +{:?} -{:?}",
                    ns.name, org_id, manifest.addon_id, report.added, report.dropped
                ),
                Ok(_) => {}
                Err(e) => warn!(
                    "vector namespace '{}' (org {}, addon {}) reconcile failed: {e}",
                    ns.name, org_id, manifest.addon_id
                ),
            }
        }
    }
}

fn parse_vector_namespaces(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::VectorNamespaceSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'name'"))?
            .to_string();
        let dimensions = item
            .get("dimensions")
            .and_then(|v| v.as_integer())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'dimensions'"))?
            as u32;
        let distance = item
            .get("distance")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'distance'"))?
            .to_string();
        let data_class = item
            .get("data_class")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[vector_namespace]][{idx}] missing 'data_class'"))?
            .to_string();
        let gate = item.get("gate").and_then(|v| v.as_str()).map(String::from);
        let mut fields = Vec::new();
        if let Some(field_arr) = item.get("fields").and_then(|v| v.as_array()) {
            for (fidx, f) in field_arr.iter().enumerate() {
                let fname = f
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("[[vector_namespace]][{idx}].fields[{fidx}] missing 'name'")
                    })?
                    .to_string();
                let ftype = f
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("[[vector_namespace]][{idx}].fields[{fidx}] missing 'type'")
                    })?
                    .to_string();
                if !matches!(ftype.as_str(), "str" | "int" | "float" | "bool") {
                    return Err(anyhow::anyhow!(
                        "[[vector_namespace]][{idx}].fields[{fidx}] invalid type '{ftype}' \
                         (expected str|int|float|bool)"
                    ));
                }
                let indexed = f.get("indexed").and_then(|v| v.as_bool()).unwrap_or(false);
                fields.push(crate::addon::manifest::VectorFieldSpec {
                    name: fname,
                    field_type: ftype,
                    indexed,
                });
            }
        }
        let sparse = item
            .get("sparse")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        out.push(crate::addon::manifest::VectorNamespaceSpec {
            name,
            dimensions,
            distance,
            data_class,
            gate,
            fields,
            sparse,
        });
    }
    Ok(out)
}

/// Parsuje sekcje `[[graph_collection]]` (RAG 0.2). Brak sekcji = pusta lista.
fn parse_graph_collections(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::GraphCollectionSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[graph_collection]][{idx}] missing 'name'"))?
            .to_string();
        let data_class = item
            .get("data_class")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[graph_collection]][{idx}] missing 'data_class'"))?
            .to_string();
        let gate = item.get("gate").and_then(|v| v.as_str()).map(String::from);
        out.push(crate::addon::manifest::GraphCollectionSpec {
            name,
            data_class,
            gate,
        });
    }
    Ok(out)
}

/// F1c P5 — load + compile every `[[flow_template]].path` against the
/// addon bundle directory. Returns the compiled flows on success; on
/// failure aborts install with a precise per-template error.
fn compile_flow_templates(
    manifest: &AddonManifest,
    addon_dir: &Path,
) -> Result<Vec<std::sync::Arc<crate::flow_runtime::types::CompiledFlow>>> {
    let mut out = Vec::with_capacity(manifest.flow_templates.len());
    for template in &manifest.flow_templates {
        let compiled = crate::flow_runtime::parser::load_from_addon_dir(addon_dir, &template.path)
            .map_err(|e| {
                anyhow::anyhow!(
                    "flow_template '{}' (path '{}'): compile failed: {}",
                    template.id,
                    template.path,
                    e
                )
            })?;
        if compiled.def.id != template.id {
            bail!(
                "flow_template '{}': manifest id does not match flow.json id '{}'",
                template.id,
                compiled.def.id
            );
        }
        out.push(std::sync::Arc::new(compiled));
    }
    Ok(out)
}

/// Deterministyczna nazwa published-model dla engine-flow instancji:
/// `"{addon_id}:{engine_flow.id}"`. `addon_id` == instance_id, więc nazwa jest
/// unikalna per instancja (UNIQUE `flows.published_model_name`). Ta sama funkcja
/// liczy nazwę przy rejestracji (install) i przy cleanupie (uninstall).
pub(crate) fn engine_flow_published_name(addon_id: &str, engine_flow_id: &str) -> String {
    format!("{addon_id}:{engine_flow_id}")
}

/// Klucz instancyjnego KV (durable), pod którym instancja zapisuje nazwę swojego
/// published query-flow. Addon czyta go przez SDK `state_get`, więc nie musi znać
/// własnego `addon_id` — odbiera gotową nazwę modelu do wyzwolenia flow.
const ENGINE_FLOW_STATE_KEY: &str = "engine_flow_model";

/// Prefiks klucza KV (durable), pod którym instancja zapisuje published-name
/// KAŻDEGO swojego engine-flow indywidualnie (`engine_flow_model:<id>`). Pierwszy
/// flow ma dodatkowo skrót `engine_flow_model` (legacy, query). Addon wołający
/// konkretny flow (np. `ingest`) odczytuje jego nazwę przez `engine_flow_model:ingest`,
/// bo `flow_model_bindings` matchuje po dokładnym `{addon_id}:{id}`, a nie po
/// literalnym aliasie modelu.
const ENGINE_FLOW_STATE_KEY_PREFIX: &str = "engine_flow_model:";

/// Rejestruje wszystkie `[[engine_flow]]` instancji jako published modele
/// flow_engine. Dla każdego flow: wczytuje JSON z katalogu addona, WALIDUJE go
/// przez rejestr adapterów (R1–R10), wstawia/aktualizuje wiersz `flows` z
/// unikalną-per-instancję `published_model_name` oraz wiązanie
/// `flow_model_bindings` (model_pattern == published name) tak, by
/// `route_chat_completion(model=<nazwa>)` rozwiązał się na ten flow. Nazwa
/// PIERWSZEGO engine-flow trafia do durable KV instancji (`engine_flow_model`),
/// żeby addon mógł ją odczytać bez znajomości własnego addon_id.
///
/// Idempotentny (install + mesh reconcile + upgrade): istniejący flow o tej
/// nazwie jest najpierw usuwany (kasuje też wiązanie przez ON DELETE CASCADE +
/// jawnie), a potem tworzony od nowa ze świeżego JSON.
fn register_engine_flows(db: &DbPool, manifest: &AddonManifest, addon_dir: &Path) -> Result<()> {
    let addon_id = &manifest.addon_id;

    // Najpierw sprzataj flowy USUNIETE z manifestu. Upgrade dotad tylko DODAWAL,
    // wiec `[[engine_flow]]` skasowany w nowej wersji zostawal na zawsze — z zywym
    // wiazaniem modelu, ktore nadal rozwiazywalo sie na nieaktualny graf.
    prune_stale_engine_flows(db, manifest);

    if manifest.engine_flows.is_empty() {
        crate::flow_engine::dispatcher::global_flow_dispatcher().map(|d| d.invalidate_cache());
        return Ok(());
    }

    // Rejestr adapterów z globalnego dispatchera (w produkcji ustawiony przed
    // obsługą instalacji). Brak dispatchera (bardzo wczesny start / część
    // fixture testowych) => walidacja semantyczna pominięta, jak w
    // `dispatch/handlers.rs::validate_flow_json_str`. Sam JSON i tak musi się
    // sparsować do FlowDefinition poniżej.
    let dispatcher = crate::flow_engine::dispatcher::global_flow_dispatcher();

    for spec in &manifest.engine_flows {
        let published_name = engine_flow_published_name(addon_id, &spec.id);

        // Wczytaj DAG flow_engine z katalogu addona (ochrona path-traversal).
        let flow_path =
            crate::util::path_safety::safe_resolve(addon_dir, &spec.path).map_err(|e| {
                anyhow::anyhow!(
                    "[[engine_flow]] '{}': ścieżka '{}' odrzucona: {e}",
                    spec.id,
                    spec.path
                )
            })?;
        let flow_json = std::fs::read_to_string(&flow_path).map_err(|e| {
            anyhow::anyhow!(
                "[[engine_flow]] '{}': nie udało się odczytać '{}': {e}",
                spec.id,
                spec.path
            )
        })?;

        // Parse + walidacja — fail-fast jak przy save flow. Bug 4: ZAWSZE
        // walidujemy, żeby nie persystować niepoprawnego DAG. Z dispatcherem:
        // pełne R1–R10 (rejestr adapterów). Bez dispatchera (bardzo wczesny start /
        // fixture): walidacja STRUKTURALNA (R1/R5/unikalność) — nie pomijamy jej.
        let parsed: crate::flow_engine::types::FlowDefinition = serde_json::from_str(&flow_json)
            .map_err(|e| {
                anyhow::anyhow!("[[engine_flow]] '{}': niepoprawny flow_json: {e}", spec.id)
            })?;
        match dispatcher.as_ref() {
            Some(d) => {
                crate::flow_engine::validation::validate(&parsed, d.registry()).map_err(|e| {
                    anyhow::anyhow!(
                        "[[engine_flow]] '{}': walidacja flow nie przeszła: {e}",
                        spec.id
                    )
                })?
            }
            None => crate::flow_engine::validation::validate_structural(&parsed).map_err(|e| {
                anyhow::anyhow!(
                    "[[engine_flow]] '{}': walidacja strukturalna flow nie przeszła: {e}",
                    spec.id
                )
            })?,
        }

        // Bug 3 (rejestracja atomowa): usunięcie starego flow+wiązania, wstawienie
        // nowego flow i wiązania dzieją się W JEDNEJ TRANSAKCJI — brak okna
        // „flow bez wiązania" / „model niedostępny" podczas re-install/upgrade.
        let params = crate::db::models::FlowParams {
            name: &format!("{addon_id} — {}", spec.id),
            description: if spec.description.is_empty() {
                None
            } else {
                Some(spec.description.as_str())
            },
            is_default: false,
            service_type: Some(spec.service_type.as_str()),
            flow_json: &flow_json,
            status: "active",
            published_model_name: Some(published_name.as_str()),
            actor_user_id: None,
        };
        let flow_id =
            crate::db::repository::register_engine_flow_atomic(db, &params, &published_name, 100)?;

        // Per-flow published-name do durable KV (`engine_flow_model:<id>`), żeby
        // addon mógł wyzwolić KONKRETNY engine-flow (np. ingest) bez znajomości
        // własnego addon_id i bez zgadywania, który flow jest „pierwszy".
        let per_id_key = format!("{ENGINE_FLOW_STATE_KEY_PREFIX}{}", spec.id);
        if let Err(e) = crate::addon::state_store::AddonStateStore::global().set(
            addon_id,
            &per_id_key,
            published_name.clone().into_bytes(),
            crate::addon::state_store::Tier::Durable,
        ) {
            tracing::warn!(
                "engine_flow: nie udało się zapisać '{per_id_key}' do KV instancji '{addon_id}': {e}"
            );
        }

        tracing::info!(
            "engine_flow '{}' instancji '{}' zarejestrowany jako model '{}' (flow_id={})",
            spec.id,
            addon_id,
            published_name,
            flow_id
        );
    }

    // Zapisz nazwę PIERWSZEGO engine-flow do durable KV instancji, żeby addon
    // odczytał ją bez znajomości własnego addon_id (SDK `state_get`).
    if let Some(first) = manifest.engine_flows.first() {
        let published_name = engine_flow_published_name(addon_id, &first.id);
        if let Err(e) = crate::addon::state_store::AddonStateStore::global().set(
            addon_id,
            ENGINE_FLOW_STATE_KEY,
            published_name.into_bytes(),
            crate::addon::state_store::Tier::Durable,
        ) {
            tracing::warn!(
                "engine_flow: nie udało się zapisać nazwy modelu do KV instancji '{}': {e}",
                addon_id
            );
        }
        // Install nie startuje instancji, więc durable write powyżej siedzi tylko
        // w RAM jako dirty i czeka na periodyczny flush (2 s) albo flush-na-stopie.
        // Jeśli proces/shard zniknie wcześniej (reinstall, restart przed tickiem),
        // engine_flow_model przepada i query-flow staje się nieosiągalny mimo
        // istniejącego wiązania. Install to rzadkie zdarzenie, więc flushujemy ten
        // klucz synchronicznie — gwarancja trwałości natychmiast po rejestracji.
        if let Err(e) = crate::addon::state_flusher::flush_addon(
            db.as_ref(),
            crate::addon::state_store::AddonStateStore::global(),
            addon_id,
        ) {
            tracing::warn!(
                "engine_flow: synchroniczny flush KV instancji '{}' nieudany: {e}",
                addon_id
            );
        }
    }

    // Świeży flow w katalogu modeli + invalidacja cache resolvera.
    crate::flow_engine::dispatcher::global_flow_dispatcher().map(|d| d.invalidate_cache());
    Ok(())
}

/// Usuwa wszystkie engine-flow instancji (wiersze `flows` + wiązania) przy
/// uninstall. Inwariant izolacji: kasuje WYŁĄCZNIE flow tej instancji (po jej
/// unikalnych published-name `{addon_id}:{id}`), nie rusza flow innej instancji
/// tego samego pakietu (inny `addon_id` => inna nazwa). `manifest` instancji
/// niesie listę `[[engine_flow]]`; gdy go nie ma (np. uszkodzony wpis), funkcja
/// jest no-opem.
/// Kasuje flowy addona, ktorych NIE MA juz w jego manifescie: wiersz flow,
/// wiazanie modelu i klucz KV z published-name. Best-effort — blad sprzatania nie
/// moze wywrocic instalacji, wiec kazdy krok tylko ostrzega.
fn prune_stale_engine_flows(db: &DbPool, manifest: &AddonManifest) {
    let addon_id = &manifest.addon_id;
    let desired: std::collections::HashSet<String> = manifest
        .engine_flows
        .iter()
        .map(|spec| engine_flow_published_name(addon_id, &spec.id))
        .collect();
    let existing = match crate::db::repository::list_engine_flow_published_names(db, addon_id) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("engine_flow prune: lista flow addona '{addon_id}': {e}");
            return;
        }
    };
    let store = crate::addon::state_store::AddonStateStore::global();
    for published_name in existing {
        if desired.contains(&published_name) {
            continue;
        }
        match crate::db::repository::get_flow_id_by_published_model_name(db, &published_name) {
            Ok(Some(flow_id)) => {
                if let Err(e) = crate::db::repository::delete_flow_model_binding_for_pattern(
                    db,
                    &published_name,
                ) {
                    tracing::warn!("engine_flow prune: wiązanie '{published_name}': {e}");
                }
                if let Err(e) = crate::db::repository::delete_flow(db, &flow_id) {
                    tracing::warn!("engine_flow prune: flow '{flow_id}': {e}");
                }
                tracing::info!("engine_flow prune: usunieto '{published_name}' (poza manifestem)");
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("engine_flow prune: lookup '{published_name}': {e}"),
        }
        // KV trzyma published-name pod `engine_flow_model:<local_id>`; local_id to
        // czesc po dwukropku.
        if let Some(local) = published_name.split_once(':').map(|(_, l)| l) {
            store.delete(addon_id, &format!("{ENGINE_FLOW_STATE_KEY_PREFIX}{local}"));
        }
        // Legacy skrot wskazujacy PIERWSZY flow — jesli wskazywal usuwany, znika.
        if store
            .get(addon_id, ENGINE_FLOW_STATE_KEY)
            .and_then(|b| String::from_utf8(b).ok())
            .is_some_and(|v| v == published_name)
        {
            store.delete(addon_id, ENGINE_FLOW_STATE_KEY);
        }
    }
}

fn unregister_engine_flows(db: &DbPool, manifest: &AddonManifest) {
    let addon_id = &manifest.addon_id;
    for spec in &manifest.engine_flows {
        let published_name = engine_flow_published_name(addon_id, &spec.id);
        match crate::db::repository::get_flow_id_by_published_model_name(db, &published_name) {
            Ok(Some(flow_id)) => {
                if let Err(e) = crate::db::repository::delete_flow_model_binding_for_pattern(
                    db,
                    &published_name,
                ) {
                    tracing::warn!("engine_flow cleanup: wiązanie '{published_name}': {e}");
                }
                if let Err(e) = crate::db::repository::delete_flow(db, &flow_id) {
                    tracing::warn!("engine_flow cleanup: flow '{flow_id}': {e}");
                }
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("engine_flow cleanup: lookup '{published_name}': {e}"),
        }
    }
    crate::flow_engine::dispatcher::global_flow_dispatcher().map(|d| d.invalidate_cache());
}

fn parse_flow_templates(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::FlowTemplateSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[flow_template]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let path = item
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[flow_template]][{idx}] missing 'path'"))?
            .to_string();
        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::FlowTemplateSpec {
            id,
            display_name,
            path,
            description,
        });
    }
    Ok(out)
}

/// Parsuje sekcje `[[engine_flow]]` (flow silnika flow_engine). `id`, `path`,
/// `service_type` są wymagane; `description` opcjonalne. Sama treść flow.json
/// jest walidowana dopiero przy rejestracji (po stronie flow_engine registry).
fn parse_engine_flows(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::EngineFlowSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("[[engine_flow]][{idx}] missing 'id'"))?
            .to_string();
        // Bug 5 (LIKE-safe, druga linia obrony) — published-name to
        // `{addon_id}:{id}` i służy jako `model_pattern` w wiązaniu (LIKE).
        // Ograniczamy `id` do `[a-z0-9-]` (jak namespace), żeby ŻADEN znak
        // specjalny LIKE (`_`/`%`/`\`) ani `*` (glob-wildcard) nie wjechał do
        // wzorca z `id`. Resolver i tak escapuje literały, ale walidacja przy
        // instalacji odrzuca dwuznaczny `id` u źródła.
        if !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            bail!(
                "[[engine_flow]][{idx}] 'id'='{id}' ma niedozwolone znaki (dozwolone: a-z 0-9 -)"
            );
        }
        let path = item
            .get("path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("[[engine_flow]][{idx}] missing 'path'"))?
            .to_string();
        let service_type = item
            .get("service_type")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("[[engine_flow]][{idx}] missing 'service_type'"))?
            .to_string();
        let description = item
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(crate::addon::manifest::EngineFlowSpec {
            id,
            path,
            service_type,
            description,
        });
    }
    Ok(out)
}

fn parse_ui_components(
    val: Option<&toml::Value>,
) -> Result<Vec<crate::addon::manifest::UiComponentSpec>> {
    let Some(arr) = val.and_then(|v| v.as_array()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for (idx, item) in arr.iter().enumerate() {
        let id = item
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'id'"))?
            .to_string();
        let display_name = item
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string();
        let slot = item
            .get("slot")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'slot'"))?
            .to_string();
        let src = item
            .get("src")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'src'"))?
            .to_string();
        let signature = item
            .get("signature")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("[[ui_component]][{idx}] missing 'signature'"))?
            .to_string();
        let risk = item
            .get("risk")
            .and_then(|v| v.as_str())
            .unwrap_or("low")
            .to_string();
        let host_permissions = item
            .get("host_permissions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        out.push(crate::addon::manifest::UiComponentSpec {
            id,
            display_name,
            slot,
            src,
            signature,
            risk,
            host_permissions,
        });
    }
    Ok(out)
}

fn parse_gpu_section(val: Option<&toml::Value>) -> Option<crate::addon::manifest::GpuInfo> {
    let tbl = val?.as_table()?;
    Some(crate::addon::manifest::GpuInfo {
        recommended_vram_mb: tbl
            .get("recommended_vram_mb")
            .and_then(|v| v.as_integer())
            .map(|v| v as u32),
        notes: tbl.get("notes").and_then(|v| v.as_str()).map(String::from),
    })
}

/// Builds a JSON Schema `object` from `[[tool.parameter]]` entries. Keeps the
/// `parameters_schema` field shape that existing tool_dispatch/host code expects.
fn build_parameters_schema(params: &[ManifestToolParameter]) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for p in params {
        if p.name.is_empty() {
            continue;
        }
        let mut prop = serde_json::Map::new();
        prop.insert(
            "type".to_string(),
            serde_json::Value::String(p.param_type.clone()),
        );
        if !p.description.is_empty() {
            prop.insert(
                "description".to_string(),
                serde_json::Value::String(p.description.clone()),
            );
        }
        properties.insert(p.name.clone(), serde_json::Value::Object(prop));
        if p.required {
            required.push(serde_json::Value::String(p.name.clone()));
        }
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

// =============================================================================
// Synchronizacja regul sieciowych (upgrade)
// =============================================================================

/// Synchronizuje reguly sieciowe przy upgrade addonu.
///
/// Logika:
/// - Istniejace reguly (ten sam rule_id): zachowaj approved/approved_by/approved_at,
///   zaktualizuj host/port/protocol/description/required
/// - Nowe reguly (nie istnieja w DB): dodaj z approved=0
/// - Usuniete reguly (nie istnieja w nowym manifescie): usun z DB
fn sync_network_rules(
    conn: &rusqlite::Connection,
    addon_id: &str,
    new_rules: &[ManifestNetworkRule],
) -> Result<()> {
    // Pobierz istniejace rule_id z DB
    let mut stmt = conn.prepare("SELECT rule_id FROM addon_network_rules WHERE addon_id = ?1")?;
    let existing_ids: Vec<String> = stmt
        .query_map(rusqlite::params![addon_id], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .collect();

    let new_ids: Vec<&str> = new_rules.iter().map(|r| r.id.as_str()).collect();

    // Usun reguly ktore nie istnieja w nowym manifescie
    for old_id in &existing_ids {
        if !new_ids.contains(&old_id.as_str()) {
            conn.execute(
                "DELETE FROM addon_network_rules WHERE addon_id = ?1 AND rule_id = ?2",
                rusqlite::params![addon_id, old_id],
            )?;
            info!(
                "upgrade: usunieto regule sieciowa '{}' addonu '{}'",
                old_id, addon_id
            );
        }
    }

    // Upsert: zaktualizuj istniejace, dodaj nowe (approved=0)
    // VULN-042: Jesli host/port/protocol sie zmienil — reset approved=0
    for rule in new_rules {
        if existing_ids.contains(&rule.id) {
            // Sprawdz czy cel polaczenia sie zmienil (host, port, protocol)
            let (old_host, old_port, old_proto): (String, i64, String) = conn
                .query_row(
                    "SELECT host, port, protocol FROM addon_network_rules \
                 WHERE addon_id = ?1 AND rule_id = ?2",
                    rusqlite::params![addon_id, &rule.id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap_or_default();

            let target_changed =
                old_host != rule.host || old_port != rule.port as i64 || old_proto != rule.protocol;

            if target_changed {
                // Cel polaczenia sie zmienil — wymagaj ponownego zatwierdzenia
                conn.execute(
                    "UPDATE addon_network_rules \
                     SET protocol = ?1, host = ?2, port = ?3, description = ?4, required = ?5, \
                         approved = 0, approved_by = NULL, approved_at = NULL \
                     WHERE addon_id = ?6 AND rule_id = ?7",
                    rusqlite::params![
                        &rule.protocol,
                        &rule.host,
                        rule.port,
                        rule.description.as_deref().unwrap_or(""),
                        rule.required as i32,
                        addon_id,
                        &rule.id,
                    ],
                )?;
                info!(
                    "upgrade: regula '{}' addonu '{}' — cel zmieniony ({}:{} -> {}:{}), reset approved",
                    rule.id, addon_id, old_host, old_port, rule.host, rule.port
                );
            } else {
                // Cel nie zmieniony — zachowaj approved status
                conn.execute(
                    "UPDATE addon_network_rules \
                     SET description = ?1, required = ?2 \
                     WHERE addon_id = ?3 AND rule_id = ?4",
                    rusqlite::params![
                        rule.description.as_deref().unwrap_or(""),
                        rule.required as i32,
                        addon_id,
                        &rule.id,
                    ],
                )?;
            }
        } else {
            // Nowa regula — wymaga zatwierdzenia admina
            conn.execute(
                "INSERT INTO addon_network_rules \
                 (addon_id, rule_id, protocol, host, port, description, required, approved) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                rusqlite::params![
                    addon_id,
                    &rule.id,
                    &rule.protocol,
                    &rule.host,
                    rule.port,
                    rule.description.as_deref().unwrap_or(""),
                    rule.required as i32,
                ],
            )?;
            info!(
                "upgrade: dodano nowa regule sieciowa '{}' addonu '{}' (wymaga zatwierdzenia)",
                rule.id, addon_id
            );
        }
    }

    Ok(())
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// rewrite_manifest_for_instance nadpisuje [addon].id i [addon].name, a manifest
    /// dalej parsuje sie poprawnie z nowym addon_id == id instancji.
    #[test]
    fn rewrite_manifest_identity_sets_instance_id_and_name() {
        let pkg = "[addon]\nid = \"company-lookup\"\nname = \"Company Lookup\"\nversion = \"1.2.0\"\nwasm_file = \"addon.wasm\"\n";
        let rewritten = rewrite_manifest_for_instance(
            pkg,
            "company-lookup-ab12cd34",
            "Prod Lookup",
            &std::collections::BTreeMap::new(),
        )
        .unwrap();
        let manifest = parse_manifest_toml(&rewritten).unwrap();
        assert_eq!(manifest.addon_id, "company-lookup-ab12cd34");
        assert_eq!(manifest.display_name, "Prod Lookup");
        // Wersja i wasm_file pakietu zostaja nietkniete.
        assert_eq!(manifest.version, "1.2.0");
        assert_eq!(manifest.wasm_file, "addon.wasm");
    }

    /// ${ip} w hoscie reguly sieciowej jest podstawiany konkretnym adresem z
    /// connection-paramow przy tworzeniu manifestu instancji.
    #[test]
    fn rewrite_substitutes_ip_placeholder_in_network_rule_host() {
        let pkg = "[addon]\nid = \"go2\"\nname = \"Go2\"\nversion = \"0.0.1\"\nwasm_file = \"addon.wasm\"\n\
                   [[network_rule]]\nid = \"sig\"\nhost = \"${ip}\"\nport = 9991\nprotocol = \"tcp\"\ndescription = \"sig\"\n";
        let mut config = std::collections::BTreeMap::new();
        config.insert("ip".to_string(), "10.0.0.5".to_string());
        let rewritten =
            rewrite_manifest_for_instance(pkg, "go2-ab12cd34", "Robot A", &config).unwrap();
        let manifest = parse_manifest_toml(&rewritten).unwrap();
        assert_eq!(manifest.network_rules.len(), 1);
        assert_eq!(manifest.network_rules[0].host, "10.0.0.5");
    }

    /// Brak wartosci dla ${ip} jest bledem — zaden niepodstawiony placeholder
    /// nie moze trafic do persistowanego manifestu.
    #[test]
    fn rewrite_fails_on_missing_placeholder_value() {
        let pkg = "[addon]\nid = \"go2\"\nname = \"Go2\"\nversion = \"0.0.1\"\nwasm_file = \"addon.wasm\"\n\
                   [[network_rule]]\nid = \"sig\"\nhost = \"${ip}\"\nport = 9991\nprotocol = \"tcp\"\ndescription = \"sig\"\n";
        let config = std::collections::BTreeMap::new();
        let err = rewrite_manifest_for_instance(pkg, "go2-ab12cd34", "Robot A", &config)
            .expect_err("missing ip must error");
        assert!(err.to_string().contains("ip"), "blad wskazuje na ${{ip}}");
    }

    /// Connection-param wartosci podstawiane do hosta reguly sieciowej musza byc
    /// czystym tokenem hosta — bare IP/DNS przechodzi, wszystko z portem/schematem/
    /// sciezka/userinfo/spacja jest odrzucane (gate bezpieczenstwa SSRF/injection).
    #[test]
    fn host_token_validation_accepts_ip_and_hostname_rejects_dirty() {
        // Akceptowane: czysta nazwa DNS oraz literal IP (v4 i v6).
        assert!(validate_host_token("ip", "evil.com").is_ok());
        assert!(validate_host_token("ip", "10.0.0.5").is_ok());
        assert!(validate_host_token("ip", "robot-1.lan").is_ok());
        assert!(validate_host_token("ip", "fe80::1").is_ok());

        // Odrzucane: schemat, port, sciezka, spacja, userinfo, znak kontrolny.
        assert!(validate_host_token("ip", "http://x").is_err());
        assert!(validate_host_token("ip", "1.2.3.4:80").is_err());
        assert!(validate_host_token("ip", "a/b").is_err());
        assert!(validate_host_token("ip", "a b").is_err());
        assert!(validate_host_token("ip", "a@b").is_err());
        assert!(validate_host_token("ip", "").is_err());
        assert!(validate_host_token("ip", "-bad.com").is_err());

        // Blad nazywa klucz parametru, zeby operator wiedzial co poprawic.
        let err = validate_host_token("robot_ip", "http://x").unwrap_err();
        assert!(err.to_string().contains("robot_ip"));
    }

    /// Brudna wartosc connection-param w hoscie reguly jest odrzucana podczas
    /// przepisywania manifestu instancji (pelna sciezka, nie tylko helper).
    #[test]
    fn rewrite_rejects_dirty_host_value() {
        let pkg = "[addon]\nid = \"go2\"\nname = \"Go2\"\nversion = \"0.0.1\"\nwasm_file = \"addon.wasm\"\n\
                   [[network_rule]]\nid = \"sig\"\nhost = \"${ip}\"\nport = 9991\nprotocol = \"tcp\"\ndescription = \"sig\"\n";
        let mut config = std::collections::BTreeMap::new();
        config.insert("ip".to_string(), "1.2.3.4:80".to_string());
        let err = rewrite_manifest_for_instance(pkg, "go2-ab12cd34", "Robot A", &config)
            .expect_err("dirty host must error");
        assert!(err.to_string().contains("ip"));
    }

    fn minimal_wasm_bytes() -> Vec<u8> {
        // Minimal valid WASM module header: magic "\0asm" + version 1.
        vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00]
    }

    /// Świeża baza in-memory z migracjami + seedem — do testów rejestracji flow.
    fn fresh_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        crate::db::seed::seed_defaults(&conn).unwrap();
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Minimalny, walidny DAG flow_engine (trigger -> output) jako string JSON.
    /// Wystarcza do testu rejestracji (parse + create_flow + binding) bez
    /// zależności od konkretnych aliasów modeli.
    fn minimal_engine_flow_json(id: &str) -> String {
        format!(
            r#"{{"id":"{id}","nodes":[
                {{"id":"t","type":"trigger","config":{{}}}},
                {{"id":"o","type":"output","config":{{}}}}
            ],"edges":[
                {{"from_node":"t","to_node":"o","from_port":"text","to_port":"text","data_type":"text"}}
            ]}}"#
        )
    }

    /// `[[engine_flow]]` parsuje się do `EngineFlowSpec` (id/path/service_type
    /// wymagane, description opcjonalne).
    #[test]
    fn engine_flow_section_parses() {
        let toml = r#"
[addon]
id = "rag-inst-1"
name = "RAG"
version = "0.1.0"
wasm_file = "addon.wasm"

[[engine_flow]]
id = "query"
path = "flows/query.flow.json"
service_type = "chat"
description = "Retrieval -> LLM"
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        assert_eq!(m.engine_flows.len(), 1);
        let ef = &m.engine_flows[0];
        assert_eq!(ef.id, "query");
        assert_eq!(ef.path, "flows/query.flow.json");
        assert_eq!(ef.service_type, "chat");
        assert_eq!(ef.description, "Retrieval -> LLM");
        // Deterministyczna nazwa published model = "{addon_id}:{id}".
        assert_eq!(
            engine_flow_published_name(&m.addon_id, &ef.id),
            "rag-inst-1:query"
        );
    }

    /// Brak wymaganych pól `[[engine_flow]]` jest błędem parsowania.
    #[test]
    fn engine_flow_section_requires_fields() {
        let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "addon.wasm"

[[engine_flow]]
id = "query"
service_type = "chat"
"#;
        let err = parse_manifest_toml(toml).expect_err("missing path must error");
        assert!(err.to_string().contains("path"), "got: {err}");
    }

    /// Realny manifest addona RAG parsuje się z nową sekcją `[[engine_flow]]`
    /// (po `rewrite_manifest_for_instance`, bo pakietowe `[addon].id` to "rag").
    /// Addon RAG nie jest wlascicielem flow ani aliasow — obie rzeczy naleza do
    /// platformy (`seed_platform_rag_flows` / `seed_platform_rag_aliases`), zeby
    /// zatrzymanie addona nie gasilo bazy wiedzy Projektow ani Code Studio, i zeby
    /// istnial jeden flow ingestu zamiast dwoch implementacji.
    #[test]
    fn rag_manifest_owns_no_flows_and_no_aliases() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/addons/rag/manifest.toml");
        let toml = std::fs::read_to_string(path).expect("manifest.toml istnieje");
        let rewritten = rewrite_manifest_for_instance(
            &toml,
            "rag-deadbeef",
            "RAG Prod",
            &std::collections::BTreeMap::new(),
        )
        .expect("rewrite instancji");
        let m = parse_manifest_toml(&rewritten).expect("parse manifestu instancji RAG");
        assert!(
            m.engine_flows.is_empty(),
            "flowy RAG jada z platformy; [[engine_flow]] przywrocilby drugi komplet"
        );
        assert!(
            m.aliases.is_empty(),
            "aliasy rag-* sa platformowe; [[alias]] oddalby je z powrotem addonowi \
             i przywrocil deaktywacje przy jego zatrzymaniu"
        );
        // Konsument nadal deklaruje, czego uzywa — to zostaje.
        let used: Vec<&str> = m.uses_aliases.iter().map(|u| u.id.as_str()).collect();
        assert!(used.contains(&"rag-embeddings"), "uses_alias: {used:?}");
        // Przestrzeń 'passages' niesie pola text + collection_id (retrieval grounding).
        let passages = m
            .vector_namespaces
            .iter()
            .find(|n| n.name == "passages")
            .expect("namespace passages");
        let field_names: Vec<&str> = passages.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(field_names.contains(&"text"), "fields: {field_names:?}");
        assert!(
            field_names.contains(&"collection_id"),
            "fields: {field_names:?}"
        );
    }

    /// register_engine_flows tworzy flow z unikalną published-name + wiązanie
    /// modelu (resolver znajdzie flow po tej nazwie); unregister kasuje oba.
    /// Inwariant izolacji: druga instancja tego samego pakietu ma własny flow,
    /// którego uninstall pierwszej NIE rusza.
    #[test]
    fn register_then_unregister_engine_flow_roundtrip() {
        let db = fresh_db();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("flows")).unwrap();
        std::fs::write(
            dir.path().join("flows/query.flow.json"),
            minimal_engine_flow_json("query"),
        )
        .unwrap();

        let manifest_toml = |id: &str| {
            format!(
                "[addon]\nid = \"{id}\"\nname = \"RAG\"\nversion = \"0.1.0\"\nwasm_file = \"addon.wasm\"\n\
                 [[engine_flow]]\nid = \"query\"\npath = \"flows/query.flow.json\"\nservice_type = \"chat\"\n"
            )
        };
        let m1 = parse_manifest_toml(&manifest_toml("rag-aaaa")).unwrap();
        let m2 = parse_manifest_toml(&manifest_toml("rag-bbbb")).unwrap();

        register_engine_flows(&db, &m1, dir.path()).unwrap();
        register_engine_flows(&db, &m2, dir.path()).unwrap();

        // Oba published modele istnieją i są rozwiązywalne przez resolver.
        let name1 = "rag-aaaa:query";
        let name2 = "rag-bbbb:query";
        let id1 = crate::db::repository::get_flow_id_by_published_model_name(&db, name1)
            .unwrap()
            .expect("flow instancji 1");
        assert!(
            crate::db::repository::get_flow_id_by_published_model_name(&db, name2)
                .unwrap()
                .is_some()
        );
        // get_flow_for_model rozwiązuje WYŁĄCZNIE przez wiązanie (bez fallbacku do
        // default flow service_type), więc testuje dokładnie ścieżkę publish->bind.
        let resolved = crate::db::repository::get_flow_for_model(&db, name1)
            .unwrap()
            .expect("resolver znajduje flow po published name");
        assert_eq!(resolved.id, id1);

        // Idempotencja: ponowna rejestracja m1 nie duplikuje (UNIQUE) i podmienia.
        register_engine_flows(&db, &m1, dir.path()).unwrap();
        assert!(
            crate::db::repository::get_flow_id_by_published_model_name(&db, name1)
                .unwrap()
                .is_some()
        );

        // Uninstall instancji 1 kasuje JEJ flow + wiązanie, nie rusza instancji 2.
        unregister_engine_flows(&db, &m1);
        assert!(
            crate::db::repository::get_flow_id_by_published_model_name(&db, name1)
                .unwrap()
                .is_none(),
            "flow instancji 1 skasowany"
        );
        assert!(
            crate::db::repository::get_flow_for_model(&db, name1)
                .unwrap()
                .is_none(),
            "wiązanie instancji 1 skasowane"
        );
        assert!(
            crate::db::repository::get_flow_id_by_published_model_name(&db, name2)
                .unwrap()
                .is_some(),
            "flow instancji 2 nietknięty"
        );
        assert!(
            crate::db::repository::get_flow_for_model(&db, name2)
                .unwrap()
                .is_some(),
            "wiązanie instancji 2 nietknięte"
        );
    }

    /// Flow USUNIETY z manifestu znika razem z wiazaniem. Bez tego upgrade tylko
    /// dodawal: skasowany `[[engine_flow]]` zostawal na zawsze, a jego wiazanie
    /// nadal rozwiazywalo nazwe modelu na nieaktualny graf.
    #[test]
    fn engine_flow_dropped_from_manifest_is_pruned() {
        let db = fresh_db();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("flows")).unwrap();
        std::fs::write(
            dir.path().join("flows/query.flow.json"),
            minimal_engine_flow_json("query"),
        )
        .unwrap();
        let head = "[addon]\nid = \"rag-bbbb\"\nname = \"RAG\"\nversion = \"0.1.0\"\n\
                    wasm_file = \"addon.wasm\"\n";
        let with_flow = format!(
            "{head}[[engine_flow]]\nid = \"query\"\npath = \"flows/query.flow.json\"\n\
             service_type = \"chat\"\n"
        );
        let name = "rag-bbbb:query";

        let m1 = parse_manifest_toml(&with_flow).unwrap();
        register_engine_flows(&db, &m1, dir.path()).unwrap();
        assert!(
            crate::db::repository::get_flow_for_model(&db, name)
                .unwrap()
                .is_some(),
            "flow musi istniec po rejestracji"
        );

        // Nowa wersja manifestu juz go nie deklaruje.
        let m2 = parse_manifest_toml(head).unwrap();
        register_engine_flows(&db, &m2, dir.path()).unwrap();
        assert!(
            crate::db::repository::get_flow_for_model(&db, name)
                .unwrap()
                .is_none(),
            "flow poza manifestem musi zniknac razem z wiazaniem"
        );
        let bindings = crate::db::repository::list_flow_model_bindings(&db).unwrap();
        assert!(
            !bindings.iter().any(|b| b.model_pattern == name),
            "osierocone wiazanie zostalo"
        );
    }

    /// Bug 3 — rejestracja atomowa: po (re)rejestracji model jest ZAWSZE
    /// rozwiązywalny (flow + wiązanie razem), a re-rejestracja aktualizuje komplet
    /// w jednej transakcji — brak okna „model niedostępny".
    #[test]
    fn engine_flow_registration_is_atomic_model_always_resolvable() {
        let db = fresh_db();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("flows")).unwrap();
        std::fs::write(
            dir.path().join("flows/query.flow.json"),
            minimal_engine_flow_json("query"),
        )
        .unwrap();
        let manifest_toml =
            "[addon]\nid = \"rag-aaaa\"\nname = \"RAG\"\nversion = \"0.1.0\"\nwasm_file = \"addon.wasm\"\n\
             [[engine_flow]]\nid = \"query\"\npath = \"flows/query.flow.json\"\nservice_type = \"chat\"\n";
        let m = parse_manifest_toml(manifest_toml).unwrap();
        let name = "rag-aaaa:query";

        register_engine_flows(&db, &m, dir.path()).unwrap();
        let id1 = crate::db::repository::get_flow_for_model(&db, name)
            .unwrap()
            .expect("model rozwiązywalny po 1. rejestracji")
            .id;

        // Re-rejestracja aktualizuje flow W MIEJSCU. Id MUSI zostać: `flow_executions`
        // ma FK bez CASCADE, więc podmiana id zerwałaby historię wykonań
        // (`register_engine_flow_atomic`, gałąź 1a).
        register_engine_flows(&db, &m, dir.path()).unwrap();
        let resolved2 = crate::db::repository::get_flow_for_model(&db, name)
            .unwrap()
            .expect("model rozwiązywalny po re-rejestracji");
        assert_eq!(resolved2.id, id1, "re-rejestracja zachowuje flow_id");

        // Dokładnie jedno wiązanie na ten wzorzec (brak duplikatów po podmianie).
        let bindings = crate::db::repository::list_flow_model_bindings(&db).unwrap();
        let count = bindings.iter().filter(|b| b.model_pattern == name).count();
        assert_eq!(count, 1, "po re-rejestracji jedno wiązanie, nie duplikaty");
    }

    /// Bug 4 — ZAWSZE waliduj: strukturalnie niepoprawny DAG (krawędź do
    /// nieistniejącego node'a) jest odrzucony przy rejestracji NAWET bez
    /// globalnego dispatchera; nic nie ląduje w bazie.
    #[test]
    fn invalid_engine_flow_rejected_without_dispatcher() {
        let db = fresh_db();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("flows")).unwrap();
        // Krawędź wskazuje na nieistniejący node "ghost" → R1.
        let bad = r#"{"id":"query","nodes":[
            {"id":"t","type":"trigger","config":{}}
        ],"edges":[
            {"from_node":"t","to_node":"ghost","from_port":"text","to_port":"text","data_type":"text"}
        ]}"#;
        std::fs::write(dir.path().join("flows/query.flow.json"), bad).unwrap();
        let m = parse_manifest_toml(
            "[addon]\nid = \"rag-cccc\"\nname = \"RAG\"\nversion = \"0.1.0\"\nwasm_file = \"addon.wasm\"\n\
             [[engine_flow]]\nid = \"query\"\npath = \"flows/query.flow.json\"\nservice_type = \"chat\"\n",
        )
        .unwrap();
        let err = register_engine_flows(&db, &m, dir.path()).unwrap_err();
        assert!(
            err.to_string().contains("walidacja"),
            "niepoprawny DAG musi paść na walidacji, był: {err}"
        );
        // Nic nie zostało zapisane.
        assert!(
            crate::db::repository::get_flow_id_by_published_model_name(&db, "rag-cccc:query")
                .unwrap()
                .is_none()
        );
    }

    /// Bug 5 — id z niedozwolonymi znakami LIKE (`_`/`%`) jest odrzucone przy
    /// parsowaniu manifestu, więc published-name nigdy nie niesie wildcardów.
    #[test]
    fn engine_flow_id_with_like_specials_rejected() {
        for bad_id in ["que_ry", "que%ry", "Query", "qu ery"] {
            let toml = format!(
                "[addon]\nid = \"x\"\nname = \"X\"\nversion = \"0.1.0\"\nwasm_file = \"addon.wasm\"\n\
                 [[engine_flow]]\nid = \"{bad_id}\"\npath = \"flows/q.json\"\nservice_type = \"chat\"\n"
            );
            let err = parse_manifest_toml(&toml)
                .expect_err(&format!("id '{bad_id}' powinien być odrzucony"));
            assert!(
                err.to_string().contains("niedozwolone znaki"),
                "id '{bad_id}' — błąd: {err}"
            );
        }
    }

    #[test]
    fn parses_service_section_with_tick_interval_and_fuel() {
        let toml = r#"
[addon]
id = "cam-watcher"
name = "Camera Watcher"
version = "0.1.0"
wasm_file = "addon.wasm"

[service]
enabled = true
tick_interval_ms = 500
tick_fuel_budget = 20000000
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let svc = m.service.expect("[service] sekcja wczytana");
        assert!(svc.enabled);
        assert_eq!(svc.tick_interval_ms, Some(500));
        assert_eq!(svc.tick_fuel_budget, Some(20_000_000));
    }

    #[test]
    fn application_section_parses_with_all_fields() {
        let toml = r#"
[addon]
id = "tentavision"
name = "TentaVision"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "main"
title = "TentaVision"
icon = "video"
description = "Live camera surveillance"
sort_order = 10
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let app = m.application.expect("[application] parsed");
        assert_eq!(app.entry_panel, "main");
        assert_eq!(app.title, "TentaVision");
        assert_eq!(app.icon, "video");
        assert_eq!(app.description, "Live camera surveillance");
        assert_eq!(app.sort_order, 10);
    }

    #[test]
    fn application_section_uses_defaults_when_optional_fields_omitted() {
        let toml = r#"
[addon]
id = "addon-x"
name = "X"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "panel_a"
title = "X"
icon = "camera"
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let app = m.application.expect("parsed");
        assert_eq!(app.description, "");
        assert_eq!(app.sort_order, 100);
    }

    #[test]
    fn application_section_absent_yields_none() {
        let toml = r#"
[addon]
id = "no-app"
name = "No App"
version = "0.1.0"
wasm_file = "addon.wasm"
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        assert!(m.application.is_none());
    }

    #[test]
    fn application_rejects_invalid_entry_panel() {
        let toml = r#"
[addon]
id = "bad"
name = "Bad"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "Main Panel!"
title = "Bad"
icon = "video"
"#;
        let err = parse_manifest_toml(toml).expect_err("must reject");
        assert!(
            err.to_string().contains("entry_panel"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn application_rejects_empty_title() {
        let toml = r#"
[addon]
id = "bad"
name = "Bad"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "main"
title = ""
icon = "video"
"#;
        let err = parse_manifest_toml(toml).expect_err("must reject");
        assert!(err.to_string().contains("title"), "unexpected error: {err}");
    }

    #[test]
    fn application_rejects_invalid_icon() {
        let toml = r#"
[addon]
id = "bad"
name = "Bad"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "main"
title = "OK"
icon = "Video Cam!"
"#;
        let err = parse_manifest_toml(toml).expect_err("must reject");
        assert!(err.to_string().contains("icon"), "unexpected error: {err}");
    }

    #[test]
    fn application_rejects_sort_order_out_of_range() {
        let toml = r#"
[addon]
id = "bad"
name = "Bad"
version = "0.1.0"
wasm_file = "addon.wasm"

[application]
entry_panel = "main"
title = "OK"
icon = "video"
sort_order = 999999
"#;
        let err = parse_manifest_toml(toml).expect_err("must reject");
        assert!(
            err.to_string().contains("sort_order"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn missing_service_section_yields_none() {
        let toml = r#"
[addon]
id = "no-service"
name = "No Service"
version = "0.1.0"
wasm_file = "addon.wasm"
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        assert!(m.service.is_none());
    }

    #[test]
    fn service_section_defaults_enabled_true_when_omitted() {
        let toml = r#"
[addon]
id = "default-enabled"
name = "Default Enabled"
version = "0.1.0"
wasm_file = "addon.wasm"

[service]
tick_interval_ms = 1000
"#;
        let m = parse_manifest_toml(toml).expect("parse");
        let svc = m.service.expect("[service] sekcja wczytana");
        assert!(svc.enabled, "enabled default to true when section present");
        assert_eq!(svc.tick_interval_ms, Some(1000));
        assert!(svc.tick_fuel_budget.is_none());
    }

    #[test]
    fn test_lifecycle_install_persists_wasm_size_and_ui_metadata() {
        let tmp = tempfile::tempdir().unwrap();
        let addon_dir = tmp.path();

        let manifest = r#"
[addon]
id = "size-test"
name = "Size Test"
version = "0.1.0"
description = "lifecycle install size/icon/runtime round-trip"
author = "tests"
platforms = ["linux"]
wasm_file = "addon.wasm"
category = "communication"
icon = "i-meeting"
runtime = "wasmtime"
"#;
        std::fs::write(addon_dir.join("manifest.toml"), manifest).unwrap();

        let wasm = minimal_wasm_bytes();
        let mut f = std::fs::File::create(addon_dir.join("addon.wasm")).unwrap();
        f.write_all(&wasm).unwrap();
        drop(f);

        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init in-memory db");
        let installed = install(addon_dir, &db).expect("install should succeed");
        assert_eq!(installed.icon.as_deref(), Some("i-meeting"));
        assert_eq!(installed.runtime.as_deref(), Some("wasmtime"));
        assert_eq!(installed.category.as_deref(), Some("communication"));

        let row = crate::db::repository::get_addon(&db, "size-test")
            .unwrap()
            .expect("addon row present");
        assert_eq!(row.icon, "i-meeting");
        assert_eq!(row.runtime, "wasmtime");
        assert_eq!(row.category, "communication");
        assert_eq!(row.wasm_size_bytes, wasm.len() as i64);
    }

    fn write_trusted_publisher(db: &crate::db::DbPool, key_b64: &str) {
        let conn = db.write().unwrap();
        conn.execute(
            "INSERT INTO trusted_publishers (key_b64, label, added_at) VALUES (?1, 'test', '2026-01-01T00:00:00Z')",
            rusqlite::params![key_b64],
        )
        .unwrap();
    }

    fn pub_pk_b64() -> (ed25519_dalek::SigningKey, String) {
        use base64::Engine;
        let sk = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pk_b64 =
            base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        (sk, pk_b64)
    }

    fn install_with_bad_src(component_src: &str) -> anyhow::Error {
        use base64::Engine;
        let tmp = tempfile::tempdir().unwrap();
        let addon_dir = tmp.path();
        let (_sk, pk_b64) = pub_pk_b64();
        let sig_b64 = format!(
            "ed25519:{}",
            base64::engine::general_purpose::STANDARD.encode([0u8; 64])
        );

        let manifest = format!(
            r#"
[addon]
id = "path-traversal-test"
name = "PT Test"
version = "0.1.0"
wasm_file = "addon.wasm"

[publisher]
label = "Test Publisher"
ed25519_public_key = "{pk_b64}"

[[ui_component]]
id = "evil"
src = "{component_src}"
signature = "{sig_b64}"
slot = "sidebar"
"#
        );
        std::fs::write(addon_dir.join("manifest.toml"), manifest).unwrap();
        let mut f = std::fs::File::create(addon_dir.join("addon.wasm")).unwrap();
        f.write_all(&minimal_wasm_bytes()).unwrap();

        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        write_trusted_publisher(&db, &pk_b64);
        install(addon_dir, &db).unwrap_err()
    }

    #[test]
    fn install_rejects_dotdot_in_component_src() {
        let err = install_with_bad_src("../../etc/passwd");
        let msg = format!("{err}");
        assert!(
            msg.contains("rejected") && msg.contains(".."),
            "expected rejection mentioning '..', got: {msg}"
        );
    }

    #[test]
    fn install_rejects_absolute_component_src() {
        let err = install_with_bad_src("/etc/passwd");
        let msg = format!("{err}");
        assert!(
            msg.contains("rejected") && msg.contains("absolute"),
            "expected absolute-path rejection, got: {msg}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn install_rejects_symlink_component_src() {
        use base64::Engine;
        let tmp = tempfile::tempdir().unwrap();
        let addon_dir = tmp.path();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("secret.html");
        std::fs::write(&target, b"<html>secret</html>").unwrap();
        std::os::unix::fs::symlink(&target, addon_dir.join("link.html")).unwrap();

        let (_sk, pk_b64) = pub_pk_b64();
        let manifest = format!(
            r#"
[addon]
id = "symlink-test"
name = "Sym Test"
version = "0.1.0"
wasm_file = "addon.wasm"

[publisher]
label = "Test Publisher"
ed25519_public_key = "{pk_b64}"

[[ui_component]]
id = "linked"
src = "link.html"
signature = "ed25519:{}"
slot = "sidebar"
"#,
            base64::engine::general_purpose::STANDARD.encode([0u8; 64])
        );
        std::fs::write(addon_dir.join("manifest.toml"), manifest).unwrap();
        let mut f = std::fs::File::create(addon_dir.join("addon.wasm")).unwrap();
        f.write_all(&minimal_wasm_bytes()).unwrap();

        let db = crate::db::init(std::path::Path::new(":memory:")).unwrap();
        write_trusted_publisher(&db, &pk_b64);
        let err = install(addon_dir, &db).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("rejected"),
            "expected symlink rejection, got: {msg}"
        );
    }

    #[test]
    fn migrate_addon_dirs_skips_symlink_entry() {
        // A symlink under the legacy root must NOT be moved (would leave the
        // target dangling or corrupt the operator's manual customisation).
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let legacy_root = home.join(".tentaflow").join("addons");
        std::fs::create_dir_all(&legacy_root).unwrap();

        // Real dir → migrates.
        let real = legacy_root.join("real-addon");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(real.join("manifest.toml"), "id=\"real\"").unwrap();

        // Symlink → skipped. Use `symlink_dir` on Windows, `symlink` on Unix.
        let link_target = tmp.path().join("external-tree");
        std::fs::create_dir(&link_target).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&link_target, legacy_root.join("linked-addon")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&link_target, legacy_root.join("linked-addon")).unwrap();

        let moved = migrate_addon_dirs_to_org_default(home).expect("migrate ok");
        assert_eq!(moved, 1, "only the real dir should migrate");

        let target_root = home
            .join(".tentaflow")
            .join("orgs")
            .join(crate::services::org::DEFAULT_ORG_ID)
            .join("addons");
        assert!(target_root.join("real-addon").exists());
        assert!(!target_root.join("linked-addon").exists());
        // The symlink stays at the legacy path for the operator to reconcile.
        assert!(legacy_root.join("linked-addon").exists());
    }

    #[test]
    fn migrate_addon_dirs_errors_on_collision() {
        // A pre-existing entry at the per-org target must abort migration so
        // the operator sees the inconsistency at boot.
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let legacy_root = home.join(".tentaflow").join("addons");
        std::fs::create_dir_all(&legacy_root).unwrap();
        let target_root = home
            .join(".tentaflow")
            .join("orgs")
            .join(crate::services::org::DEFAULT_ORG_ID)
            .join("addons");
        std::fs::create_dir_all(&target_root).unwrap();

        // Same name on both sides → collision.
        std::fs::create_dir(legacy_root.join("dup")).unwrap();
        std::fs::create_dir(target_root.join("dup")).unwrap();

        let err = migrate_addon_dirs_to_org_default(home).expect_err("must err");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(format!("{err}").contains("dup"));
    }

    #[test]
    fn addon_skill_id_is_deterministic_per_addon() {
        let first = addon_skill_id("memory");
        let second = addon_skill_id("memory");
        assert_eq!(first, second, "same addon id must yield the same UUIDv5");
        assert_ne!(
            first,
            addon_skill_id("embeddings-chunker"),
            "different addons must yield different ids"
        );
        let parsed = uuid::Uuid::parse_str(&first).expect("valid uuid");
        assert_eq!(parsed.get_version(), Some(uuid::Version::Sha1));
    }

    #[test]
    fn frontmatter_absent_returns_body_verbatim() {
        let fm = parse_skill_frontmatter("# Title\n\nBody text.\n");
        assert!(fm.name.is_none());
        assert!(fm.description.is_none());
        assert!(fm.tags.is_empty());
        assert_eq!(fm.body, "# Title\n\nBody text.\n");
    }

    #[test]
    fn frontmatter_parses_known_keys_and_strips_block() {
        let raw = "---\nname: web-research\ndescription: \"How to research\"\ntags: [research, web]\nunknown: ignored\n---\n\n# Body\n";
        let fm = parse_skill_frontmatter(raw);
        assert_eq!(fm.name.as_deref(), Some("web-research"));
        assert_eq!(fm.description.as_deref(), Some("How to research"));
        assert_eq!(fm.tags, vec!["research".to_string(), "web".to_string()]);
        assert_eq!(fm.body, "# Body\n");
    }

    #[test]
    fn frontmatter_parses_block_list_tags_and_quotes() {
        let raw =
            "---\nname: 'quoted-name'\ntags:\n  - alpha\n  - \"beta\"\ndescription: plain\n---\nBody";
        let fm = parse_skill_frontmatter(raw);
        assert_eq!(fm.name.as_deref(), Some("quoted-name"));
        assert_eq!(fm.description.as_deref(), Some("plain"));
        assert_eq!(fm.tags, vec!["alpha".to_string(), "beta".to_string()]);
        assert_eq!(fm.body, "Body");
    }

    #[test]
    fn frontmatter_unterminated_block_is_treated_as_body() {
        let raw = "---\nname: broken\nno closing fence";
        let fm = parse_skill_frontmatter(raw);
        assert!(fm.name.is_none());
        assert_eq!(fm.body, raw);
    }

    #[test]
    fn frontmatter_handles_crlf_and_comma_tags() {
        let raw = "---\r\nname: crlf-skill\r\ntags: a, b\r\n---\r\nBody\r\n";
        let fm = parse_skill_frontmatter(raw);
        assert_eq!(fm.name.as_deref(), Some("crlf-skill"));
        assert_eq!(fm.tags, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(fm.body, "Body\r\n");
    }

    fn skill_test_manifest(addon_id: &str) -> AddonManifest {
        let toml = format!(
            "[addon]\nid = \"{addon_id}\"\nname = \"Skill Test\"\nversion = \"0.1.0\"\nwasm_file = \"addon.wasm\"\ndescription = \"Manifest description\"\n"
        );
        parse_manifest_toml(&toml).expect("manifest")
    }

    #[test]
    fn materialize_addon_skill_upserts_deterministic_readonly_row() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: memory-usage\ntags: [memory]\n---\n# Using memory\n",
        )
        .unwrap();
        let manifest = skill_test_manifest("skill-test-addon");

        materialize_addon_skill(&db, &manifest, tmp.path());
        let expected_id = addon_skill_id("skill-test-addon");
        let skill = crate::db::repository::get_skill(&db, &expected_id)
            .expect("get")
            .expect("materialized");
        assert_eq!(skill.name, "memory-usage");
        assert_eq!(skill.source, "addon");
        assert_eq!(skill.source_ref.as_deref(), Some("skill-test-addon"));
        assert_eq!(skill.description, "Manifest description");
        assert_eq!(skill.content, "# Using memory\n");
        assert_eq!(skill.tags_json, r#"["memory"]"#);

        // Re-materialization (addon update) converges on the SAME row.
        materialize_addon_skill(&db, &manifest, tmp.path());
        let all =
            crate::db::repository::list_skills(&db, &crate::db::models::SkillListFilter::default())
                .expect("list");
        assert_eq!(all.len(), 1, "deterministic id must prevent duplicates");

        // An admin-disabled addon skill stays disabled across package updates.
        let params = crate::db::models::SkillParams {
            id: &expected_id,
            name: &skill.name,
            display_name: skill.display_name.as_deref(),
            description: &skill.description,
            content: &skill.content,
            tags_json: &skill.tags_json,
            category: skill.category.as_deref(),
            source: "addon",
            source_ref: Some("skill-test-addon"),
            status: "disabled",
            created_by: None,
            actor_user_id: None,
        };
        crate::db::repository::upsert_skill(&db, &params).expect("disable");
        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &expected_id)
            .expect("get")
            .expect("exists");
        assert_eq!(skill.status, "disabled");

        // A package without SKILL.md removes the materialized row.
        std::fs::remove_file(tmp.path().join("SKILL.md")).unwrap();
        materialize_addon_skill(&db, &manifest, tmp.path());
        assert!(crate::db::repository::get_skill(&db, &expected_id)
            .expect("get")
            .is_none());
    }

    #[test]
    fn materialize_addon_skill_falls_back_to_addon_id_on_bad_frontmatter_name() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: Not Valid Name\n---\nBody\n",
        )
        .unwrap();
        let manifest = skill_test_manifest("fallback-addon");
        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &addon_skill_id("fallback-addon"))
            .expect("get")
            .expect("materialized");
        assert_eq!(skill.name, "fallback-addon");
        assert_eq!(skill.content, "Body\n");
    }

    #[test]
    fn fallback_skill_name_sanitizes_valid_addon_ids() {
        assert_eq!(fallback_skill_name("company_lookup"), "company-lookup");
        assert_eq!(fallback_skill_name("My.Addon"), "my-addon");
        assert_eq!(fallback_skill_name("_.-Edge--Case-_."), "edge-case");
        assert_eq!(fallback_skill_name("already-kebab"), "already-kebab");
        let long = "a".repeat(63) + "_tail";
        let derived = fallback_skill_name(&long);
        assert!(derived.len() <= crate::db::repository::SKILL_NAME_MAX_CHARS);
        assert!(!derived.ends_with('-'));
    }

    #[test]
    fn materialize_addon_skill_sanitizes_fallback_name_from_addon_id() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("SKILL.md"), "Body only\n").unwrap();
        let manifest = skill_test_manifest("Company_Lookup.v2");
        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &addon_skill_id("Company_Lookup.v2"))
            .expect("get")
            .expect("materialized");
        assert_eq!(skill.name, "company-lookup-v2");
    }

    #[test]
    fn materialize_addon_skill_preserves_admin_tags_and_skips_noop_upserts() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: tagged-skill\ntags: [from-package]\n---\nBody\n",
        )
        .unwrap();
        let manifest = skill_test_manifest("tag-test-addon");
        let skill_id = addon_skill_id("tag-test-addon");

        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &skill_id)
            .expect("get")
            .expect("materialized");
        assert_eq!(skill.tags_json, r#"["from-package"]"#);

        let count_skill_captures = || -> i64 {
            let conn = db.read().expect("db lock");
            conn.query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures WHERE resource_type = 'core.skill'",
                [],
                |r| r.get(0),
            )
            .expect("count captures")
        };
        let after_first = count_skill_captures();

        // Admin edits the tags (the only frontmatter-seeded field the upsert
        // handler lets admins change on addon skills, next to status).
        let params = crate::db::models::SkillParams {
            id: &skill_id,
            name: &skill.name,
            display_name: skill.display_name.as_deref(),
            description: &skill.description,
            content: &skill.content,
            tags_json: r#"["admin-tag"]"#,
            category: skill.category.as_deref(),
            source: "addon",
            source_ref: Some("tag-test-addon"),
            status: &skill.status,
            created_by: None,
            actor_user_id: None,
        };
        crate::db::repository::upsert_skill(&db, &params).expect("admin tag edit");
        let after_edit = count_skill_captures();

        // A reconcile with an unchanged package keeps the admin tags AND emits
        // no new sync capture (no-op detection).
        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &skill_id)
            .expect("get")
            .expect("exists");
        assert_eq!(skill.tags_json, r#"["admin-tag"]"#);
        assert_eq!(count_skill_captures(), after_edit);
        assert!(after_edit > after_first);

        // A real package change still rematerializes — and keeps the tags.
        std::fs::write(
            tmp.path().join("SKILL.md"),
            "---\nname: tagged-skill\ntags: [from-package]\n---\nNew body\n",
        )
        .unwrap();
        materialize_addon_skill(&db, &manifest, tmp.path());
        let skill = crate::db::repository::get_skill(&db, &skill_id)
            .expect("get")
            .expect("exists");
        assert_eq!(skill.content, "New body\n");
        assert_eq!(skill.tags_json, r#"["admin-tag"]"#);
        assert!(count_skill_captures() > after_edit);
    }

    /// An addon exposing tools gets the synthetic "llm" catalog entry — the only
    /// thing that makes the grant clickable in the admin matrix, which renders
    /// catalog entries and nothing else. Declared permissions are untouched and
    /// NO grant is created: the entry is an offer, not a decision.
    #[test]
    fn tool_addon_gets_synthetic_llm_permission_in_catalog() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let toml = "[addon]\nid = \"deep-research-043b6b64\"\nname = \"Deep Research\"\n\
                    version = \"1.0.0\"\nwasm_file = \"addon.wasm\"\n\
                    [[permission]]\nid = \"web.research\"\ndisplay_name = \"Web research\"\n\
                    description = \"Reads public pages.\"\nrisk = \"critical\"\n\
                    [[tool]]\nid = \"search_web\"\ndescription = \"Search the public web.\"\n\
                    [[tool]]\nid = \"read_url\"\ndescription = \"Read one public page.\"\n";
        let manifest = parse_manifest_toml(toml).expect("manifest");
        assert_eq!(manifest.tools.len(), 2);

        sync_manifest_metadata(&db, &manifest).expect("sync");

        let catalog = crate::db::repository::list_permission_catalog(&db, &manifest.addon_id)
            .expect("catalog");
        let ids: Vec<&str> = catalog.iter().map(|e| e.permission_id.as_str()).collect();
        assert_eq!(ids, vec!["web.research", "llm"]);
        let llm = catalog
            .iter()
            .find(|e| e.permission_id == crate::addon::permissions::LLM_PERMISSION_ID)
            .expect("synthetic llm entry");
        assert!(!llm.display_name.is_empty());
        assert!(llm.description.contains("search_web"));
        assert!(llm.description.contains("read_url"));
        assert_eq!(llm.risk, "high");

        // Deny-by-default survives: cataloguing grants nothing.
        let granted: i64 = db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM addon_permissions WHERE addon_id = ?1",
                rusqlite::params![manifest.addon_id],
                |r| r.get(0),
            )
            .expect("count grants");
        assert_eq!(granted, 0);

        // Re-running the sync (upgrade path) keeps the entry — the diff-delete
        // must not treat it as a permission the manifest dropped.
        sync_manifest_metadata(&db, &manifest).expect("resync");
        let ids: Vec<String> =
            crate::db::repository::list_permission_catalog(&db, &manifest.addon_id)
                .expect("catalog")
                .into_iter()
                .map(|e| e.permission_id)
                .collect();
        assert_eq!(ids, vec!["web.research".to_string(), "llm".to_string()]);
    }

    /// No `[[tool]]` → no synthetic entry: an addon that exposes nothing to a
    /// model must not offer the admin a permission that grants nothing.
    #[test]
    fn addon_without_tools_gets_no_synthetic_llm_permission() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let toml = "[addon]\nid = \"quiet\"\nname = \"Quiet\"\nversion = \"1.0.0\"\n\
                    wasm_file = \"addon.wasm\"\n\
                    [[permission]]\nid = \"storage.read\"\ndisplay_name = \"Read storage\"\n\
                    description = \"Reads KV.\"\nrisk = \"low\"\n";
        let manifest = parse_manifest_toml(toml).expect("manifest");
        sync_manifest_metadata(&db, &manifest).expect("sync");
        let ids: Vec<String> = crate::db::repository::list_permission_catalog(&db, "quiet")
            .expect("catalog")
            .into_iter()
            .map(|e| e.permission_id)
            .collect();
        assert_eq!(ids, vec!["storage.read".to_string()]);
    }

    /// A manifest that declares "llm" itself keeps ITS wording — the synthetic
    /// entry must not overwrite an author's declaration.
    #[test]
    fn declared_llm_permission_is_not_replaced_by_the_synthetic_entry() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("db");
        let toml = "[addon]\nid = \"talker\"\nname = \"Talker\"\nversion = \"1.0.0\"\n\
                    wasm_file = \"addon.wasm\"\n\
                    [[permission]]\nid = \"llm\"\ndisplay_name = \"Author wording\"\n\
                    description = \"Declared by the package author.\"\nrisk = \"medium\"\n\
                    [[tool]]\nid = \"say\"\ndescription = \"Say something.\"\n";
        let manifest = parse_manifest_toml(toml).expect("manifest");
        sync_manifest_metadata(&db, &manifest).expect("sync");
        let catalog =
            crate::db::repository::list_permission_catalog(&db, "talker").expect("catalog");
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].permission_id, "llm");
        assert_eq!(catalog[0].display_name, "Author wording");
        assert_eq!(catalog[0].risk, "medium");
    }
}
