// ===== File: addon/install_steps.rs — app steps of a native package's install wizard =====
//
// A native package may declare `[[install_step]]` blocks. The Addons install UI
// renders them after the instance exists and runs each through
// `NativeAppHooks::install_step`.
//
// Semantics, all of them deliberate:
//   * A step runs ONLY against an existing instance. The instance row, its
//     permission catalog and its `init` hook are done before the first step is
//     offered, so a step can use the instance's database and data directory.
//   * A failed step never rolls the instance back and is never reported as
//     success: the wizard shows it as `failed` with the hook's message and a
//     re-run button, and the instance stays installed. Nothing here persists a
//     "steps done" flag — the outcome belongs to the run that produced it, so a
//     stale green can never describe a node that has since changed.
//   * Steps are idempotent and re-runnable. A hook must leave the instance in
//     the same state whether it runs once or five times, and a re-run after a
//     fix is the supported recovery path.
//   * `warning` means the step ran but the result deserves an operator's
//     attention (the instance is still usable); `failed` means the thing the
//     step verifies does not hold.
//   * A hook returning `Err` is an infrastructure failure of the step itself
//     and is reported as `failed` with the error text; only a malformed request
//     (unknown instance/step, values the form would never send) is refused as a
//     protocol error.
//   * Steps run on THIS node only. They are an install-time check of the node
//     the admin is talking to, not a fleet-wide operation.

use std::collections::{BTreeMap, HashSet};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::addon::native_apps::NativeAppContext;
use crate::addon::AddonManifest;
use crate::db::DbPool;

const MAX_KEY_LEN: usize = 128;
const MAX_TEXT_VALUE_LEN: usize = 256;
const MAX_OPTION_VALUE_LEN: usize = 64;

/// How one field of a step form is presented and validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallFieldKind {
    Select,
    Multiselect,
    Checkbox,
    Text,
}

impl InstallFieldKind {
    /// The wire/UI spelling of the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Multiselect => "multiselect",
            Self::Checkbox => "checkbox",
            Self::Text => "text",
        }
    }

    fn has_options(self) -> bool {
        matches!(self, Self::Select | Self::Multiselect)
    }
}

/// One choice of a `select`/`multiselect` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallStepOption {
    pub value: String,
    pub label_key: String,
}

/// One input of a step's form (`[[install_step.field]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallStepField {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: InstallFieldKind,
    pub label_key: String,
    #[serde(default)]
    pub required: bool,
    /// Initial value in the field's wire spelling (see `resolve_values`).
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default, rename = "option")]
    pub options: Vec<InstallStepOption>,
}

/// One step of the install wizard (`[[install_step]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallStepSpec {
    pub id: String,
    pub title_key: String,
    #[serde(default)]
    pub description_key: Option<String>,
    #[serde(default, rename = "field")]
    pub fields: Vec<InstallStepField>,
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && id.len() <= 48
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn check_key(what: &str, key: &str) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_LEN {
        bail!("{what}: i18n key length out of range 1..={MAX_KEY_LEN}");
    }
    Ok(())
}

impl InstallStepField {
    fn validate(&self, step_id: &str) -> Result<()> {
        let at = format!("install_step '{step_id}' field '{}'", self.id);
        if !valid_id(&self.id) {
            bail!(
                "install_step '{step_id}': field id '{}' must match [a-z][a-z0-9_-]{{0,47}}",
                self.id
            );
        }
        check_key(&at, &self.label_key)?;
        if self.kind.has_options() {
            if self.options.is_empty() {
                bail!(
                    "{at}: a {} field needs at least one [[option]]",
                    self.kind.as_str()
                );
            }
            let mut seen = HashSet::new();
            for option in &self.options {
                if option.value.is_empty()
                    || option.value.len() > MAX_OPTION_VALUE_LEN
                    || option.value.contains(',')
                {
                    bail!("{at}: option value '{}' must be 1..={MAX_OPTION_VALUE_LEN} chars without a comma", option.value);
                }
                if !seen.insert(option.value.as_str()) {
                    bail!("{at}: duplicate option value '{}'", option.value);
                }
                check_key(&at, &option.label_key)?;
            }
        } else if !self.options.is_empty() {
            bail!("{at}: only select and multiselect fields take options");
        }
        if let Some(default) = &self.default {
            self.normalize(default)
                .map_err(|e| anyhow::anyhow!("{at}: default is invalid: {e}"))?;
        }
        Ok(())
    }

    /// Canonical wire spelling of one submitted value, or why it is refused.
    /// Checkbox: `true`/`false`. Multiselect: the chosen option values joined
    /// by commas in the order the options are declared. Text: trimmed.
    fn normalize(&self, raw: &str) -> Result<String> {
        match self.kind {
            InstallFieldKind::Checkbox => match raw {
                "true" | "false" => Ok(raw.to_string()),
                other => bail!("'{other}' is not true/false"),
            },
            InstallFieldKind::Text => {
                let value = raw.trim();
                if value.chars().count() > MAX_TEXT_VALUE_LEN {
                    bail!("longer than {MAX_TEXT_VALUE_LEN} characters");
                }
                Ok(value.to_string())
            }
            InstallFieldKind::Select => {
                if raw.is_empty() || self.options.iter().any(|o| o.value == raw) {
                    Ok(raw.to_string())
                } else {
                    bail!("'{raw}' is not one of the options")
                }
            }
            InstallFieldKind::Multiselect => {
                let chosen: HashSet<&str> = raw.split(',').filter(|v| !v.is_empty()).collect();
                if let Some(unknown) = chosen
                    .iter()
                    .find(|v| !self.options.iter().any(|o| &o.value == *v))
                {
                    bail!("'{unknown}' is not one of the options");
                }
                Ok(self
                    .options
                    .iter()
                    .filter(|o| chosen.contains(o.value.as_str()))
                    .map(|o| o.value.as_str())
                    .collect::<Vec<_>>()
                    .join(","))
            }
        }
    }

    fn is_empty_value(&self, value: &str) -> bool {
        match self.kind {
            InstallFieldKind::Checkbox => value != "true",
            _ => value.is_empty(),
        }
    }
}

impl InstallStepSpec {
    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            bail!(
                "install_step id '{}' must match [a-z][a-z0-9_-]{{0,47}}",
                self.id
            );
        }
        let at = format!("install_step '{}'", self.id);
        check_key(&at, &self.title_key)?;
        if let Some(key) = &self.description_key {
            check_key(&at, key)?;
        }
        let mut seen = HashSet::new();
        for field in &self.fields {
            if !seen.insert(field.id.as_str()) {
                bail!("{at}: duplicate field id '{}'", field.id);
            }
            field.validate(&self.id)?;
        }
        Ok(())
    }

    /// The form's answers as the hook receives them: every declared field
    /// present in canonical spelling (defaults filled in), nothing undeclared,
    /// every required field answered.
    pub fn resolve_values(
        &self,
        supplied: &[(String, String)],
    ) -> Result<BTreeMap<String, String>> {
        let mut given: BTreeMap<&str, &str> = BTreeMap::new();
        for (key, value) in supplied {
            if !self.fields.iter().any(|f| &f.id == key) {
                bail!("step '{}' has no field '{key}'", self.id);
            }
            if given.insert(key, value).is_some() {
                bail!("field '{key}' was sent twice");
            }
        }
        let mut out = BTreeMap::new();
        for field in &self.fields {
            let raw = given
                .get(field.id.as_str())
                .copied()
                .or(field.default.as_deref())
                .unwrap_or(if field.kind == InstallFieldKind::Checkbox {
                    "false"
                } else {
                    ""
                });
            let value = field
                .normalize(raw)
                .map_err(|e| anyhow::anyhow!("field '{}': {e}", field.id))?;
            if field.required && field.is_empty_value(&value) {
                bail!("field '{}' is required", field.id);
            }
            out.insert(field.id.clone(), value);
        }
        Ok(out)
    }
}

/// Parses and validates the manifest's `[[install_step]]` array. Duplicate step
/// ids, unknown field types and unknown keys are refused here, at manifest
/// load, so a broken declaration can never reach an install wizard.
pub fn parse_install_steps(value: Option<&toml::Value>) -> Result<Vec<InstallStepSpec>> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let steps: Vec<InstallStepSpec> = value
        .clone()
        .try_into()
        .map_err(|e| anyhow::anyhow!("[[install_step]] is invalid: {e}"))?;
    let mut seen = HashSet::new();
    for step in &steps {
        step.validate()?;
        if !seen.insert(step.id.as_str()) {
            bail!("install_step id '{}' is declared twice", step.id);
        }
    }
    Ok(steps)
}

/// What the wizard shows for one finished step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStepStatus {
    Ok,
    Warning,
    Failed,
}

/// Result of one step run: English text for the operator plus measured facts
/// (name, value) the wizard lists under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStepOutcome {
    pub status: InstallStepStatus,
    pub message: String,
    pub details: Vec<(String, String)>,
}

impl InstallStepOutcome {
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            status: InstallStepStatus::Failed,
            message: message.into(),
            details: Vec::new(),
        }
    }
}

/// One request to a step hook: which step and the resolved form answers.
pub struct InstallStepRun<'a> {
    pub step_id: &'a str,
    pub values: &'a BTreeMap<String, String>,
}

/// Why a step request was refused before any hook ran.
#[derive(Debug, PartialEq, Eq)]
pub struct InstallStepRefusal(pub String);

impl std::fmt::Display for InstallStepRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn refuse(message: impl Into<String>) -> InstallStepRefusal {
    InstallStepRefusal(message.into())
}

/// Runs one declared step of an installed instance through its package's hook.
/// The step set comes from the package manifest of the instance's own version.
pub fn run_instance_step(
    db: &DbPool,
    addon_id: &str,
    step_id: &str,
    supplied: &[(String, String)],
) -> Result<InstallStepOutcome, InstallStepRefusal> {
    let (package_id, version) = crate::db::repository::get_addon_instance_package_ref(db, addon_id)
        .map_err(|e| refuse(format!("instance lookup failed: {e}")))?
        .ok_or_else(|| refuse(format!("instance '{addon_id}' does not exist")))?;
    let package = crate::db::repository::get_addon_package(db, &package_id, &version)
        .map_err(|e| refuse(format!("package lookup failed: {e}")))?
        .ok_or_else(|| {
            refuse(format!(
                "package '{package_id}' v{version} is not in the catalog"
            ))
        })?;
    let manifest: AddonManifest =
        crate::addon::lifecycle::parse_manifest_toml(&package.manifest_json)
            .map_err(|e| refuse(format!("package manifest is invalid: {e}")))?;
    let step = manifest
        .install_steps
        .iter()
        .find(|s| s.id == step_id)
        .ok_or_else(|| {
            refuse(format!(
                "package '{package_id}' declares no install step '{step_id}'"
            ))
        })?;
    let hook = crate::addon::native_apps::hooks_for(&package_id)
        .and_then(|h| h.install_step)
        .ok_or_else(|| {
            refuse(format!(
                "package '{package_id}' has no install step hook in this build"
            ))
        })?;
    let values = step
        .resolve_values(supplied)
        .map_err(|e| refuse(e.to_string()))?;

    let org_id = crate::services::org::DEFAULT_ORG_ID;
    let data_dir = crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id)
        .map_err(|e| refuse(format!("instance data directory: {e:?}")))?;
    let ctx = NativeAppContext {
        db,
        addon_id,
        org_id,
        data_dir,
    };
    let outcome = match hook(
        &ctx,
        &InstallStepRun {
            step_id,
            values: &values,
        },
    ) {
        Ok(outcome) => outcome,
        Err(e) => InstallStepOutcome::failed(format!("{e:#}")),
    };
    // The instance stays installed whatever this says; the wizard owns the
    // re-run, so the log is the only other record of a step that did not pass.
    if outcome.status != InstallStepStatus::Ok {
        tracing::warn!(
            addon_id,
            step_id,
            status = ?outcome.status,
            "install step did not pass: {}",
            outcome.message
        );
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_src: &str) -> Result<Vec<InstallStepSpec>> {
        let table: toml::Value = toml::from_str(toml_src).expect("toml");
        parse_install_steps(table.get("install_step"))
    }

    const FORM: &str = r#"
[[install_step]]
id = "pick"
title_key = "t.pick"
description_key = "t.pick.desc"

[[install_step.field]]
id = "mode"
type = "select"
label_key = "t.mode"
required = true
default = "fast"
[[install_step.field.option]]
value = "fast"
label_key = "t.fast"
[[install_step.field.option]]
value = "slow"
label_key = "t.slow"

[[install_step.field]]
id = "nodes"
type = "multiselect"
label_key = "t.nodes"
[[install_step.field.option]]
value = "a"
label_key = "t.a"
[[install_step.field.option]]
value = "b"
label_key = "t.b"

[[install_step.field]]
id = "agree"
type = "checkbox"
label_key = "t.agree"
required = true

[[install_step.field]]
id = "note"
type = "text"
label_key = "t.note"
"#;

    #[test]
    fn a_manifest_without_steps_has_none() {
        assert!(parse("[addon]\nid = \"x\"\n").unwrap().is_empty());
    }

    #[test]
    fn a_full_step_form_parses_in_declaration_order() {
        let steps = parse(FORM).expect("parses");
        assert_eq!(steps.len(), 1);
        let step = &steps[0];
        assert_eq!(step.id, "pick");
        assert_eq!(step.description_key.as_deref(), Some("t.pick.desc"));
        let kinds: Vec<_> = step.fields.iter().map(|f| f.kind.as_str()).collect();
        assert_eq!(kinds, ["select", "multiselect", "checkbox", "text"]);
        assert_eq!(step.fields[0].options.len(), 2);
        assert_eq!(step.fields[0].default.as_deref(), Some("fast"));
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let err = parse("[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n")
            .unwrap_err();
        assert!(err.to_string().contains("declared twice"), "{err}");
    }

    #[test]
    fn duplicate_field_ids_are_rejected() {
        let src = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
                   [[install_step.field]]\nid = \"f\"\ntype = \"text\"\nlabel_key = \"l\"\n\
                   [[install_step.field]]\nid = \"f\"\ntype = \"text\"\nlabel_key = \"l\"\n";
        assert!(parse(src)
            .unwrap_err()
            .to_string()
            .contains("duplicate field id"));
    }

    #[test]
    fn an_unknown_field_type_is_rejected() {
        let src = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
                   [[install_step.field]]\nid = \"f\"\ntype = \"slider\"\nlabel_key = \"l\"\n";
        assert!(parse(src).is_err());
    }

    #[test]
    fn unknown_keys_and_missing_i18n_keys_are_rejected() {
        assert!(
            parse("[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\naction = \"x\"\n").is_err()
        );
        assert!(
            parse("[[install_step]]\nid = \"a\"\n").is_err(),
            "title_key is required"
        );
        assert!(
            parse("[[install_step]]\nid = \"a\"\ntitle_key = \"\"\n").is_err(),
            "empty key"
        );
        let no_label = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
                        [[install_step.field]]\nid = \"f\"\ntype = \"text\"\n";
        assert!(parse(no_label).is_err(), "label_key is required");
    }

    #[test]
    fn ids_must_be_lowercase_slugs() {
        assert!(parse("[[install_step]]\nid = \"Bad Id\"\ntitle_key = \"t\"\n").is_err());
        assert!(parse("[[install_step]]\nid = \"ok-id_2\"\ntitle_key = \"t\"\n").is_ok());
    }

    #[test]
    fn options_belong_to_choice_fields_only_and_must_be_sound() {
        let text_with_option = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
            [[install_step.field]]\nid = \"f\"\ntype = \"text\"\nlabel_key = \"l\"\n\
            [[install_step.field.option]]\nvalue = \"x\"\nlabel_key = \"l\"\n";
        assert!(parse(text_with_option).is_err());
        let empty_select = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
            [[install_step.field]]\nid = \"f\"\ntype = \"select\"\nlabel_key = \"l\"\n";
        assert!(parse(empty_select).is_err());
        let comma = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
            [[install_step.field]]\nid = \"f\"\ntype = \"multiselect\"\nlabel_key = \"l\"\n\
            [[install_step.field.option]]\nvalue = \"x,y\"\nlabel_key = \"l\"\n";
        assert!(
            parse(comma).is_err(),
            "a comma would break the multiselect wire spelling"
        );
        let dup = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
            [[install_step.field]]\nid = \"f\"\ntype = \"select\"\nlabel_key = \"l\"\n\
            [[install_step.field.option]]\nvalue = \"x\"\nlabel_key = \"l\"\n\
            [[install_step.field.option]]\nvalue = \"x\"\nlabel_key = \"l\"\n";
        assert!(parse(dup).is_err());
    }

    #[test]
    fn a_default_outside_the_options_is_rejected() {
        let src = "[[install_step]]\nid = \"a\"\ntitle_key = \"t\"\n\
            [[install_step.field]]\nid = \"f\"\ntype = \"select\"\nlabel_key = \"l\"\ndefault = \"zzz\"\n\
            [[install_step.field.option]]\nvalue = \"x\"\nlabel_key = \"l\"\n";
        assert!(parse(src).is_err());
    }

    fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn resolve_fills_defaults_and_canonicalizes_values() {
        let step = parse(FORM).unwrap().remove(0);
        let values = step
            .resolve_values(&pairs(&[
                ("agree", "true"),
                ("nodes", "b,a"),
                ("note", "  hi  "),
            ]))
            .expect("resolves");
        assert_eq!(values["mode"], "fast", "the default fills an omitted field");
        assert_eq!(
            values["nodes"], "a,b",
            "multiselect values come back in option order"
        );
        assert_eq!(values["note"], "hi");
        assert_eq!(values["agree"], "true");
    }

    #[test]
    fn resolve_refuses_what_the_form_would_never_send() {
        let step = parse(FORM).unwrap().remove(0);
        for bad in [
            pairs(&[("agree", "true"), ("ghost", "1")]),
            pairs(&[("agree", "true"), ("mode", "turbo")]),
            pairs(&[("agree", "true"), ("nodes", "a,zzz")]),
            pairs(&[("agree", "yes")]),
            pairs(&[("agree", "true"), ("agree", "true")]),
        ] {
            assert!(step.resolve_values(&bad).is_err(), "{bad:?}");
        }
        let err = step.resolve_values(&pairs(&[])).unwrap_err();
        assert!(err.to_string().contains("'agree' is required"), "{err}");
        let err = step
            .resolve_values(&pairs(&[("agree", "false")]))
            .unwrap_err();
        assert!(
            err.to_string().contains("'agree' is required"),
            "a required checkbox must be ticked: {err}"
        );
    }
}
