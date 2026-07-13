// =============================================================================
// File: protocol/ui/form/wrappers.rs — FormField/FormGroup/FormSection/Form + FormValidator (catalog §5)
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use super::super::super::control::CborMap;
use super::super::super::value::Value;
use super::super::bind::BindRef;
use super::super::component::{Component, FieldMap};
use super::super::tokens::{FormFieldLayout, FormLayout, Spacing};
use super::super::typed_field::{
    decode_from_value, encode_to_value, ensure_no_duplicate_keys, ensure_tag, missing_field,
    unknown_field, IntoComponentError,
};

#[inline]
fn component(tag: u16, id: impl Into<String>, fields: Vec<(u8, Value)>) -> Component {
    Component {
        tag,
        id: id.into(),
        fields: FieldMap(fields),
        handlers: None,
        bind: None,
        a11y: None,
        visibility: None,
        test_id: None,
    }
}

// -----------------------------------------------------------------------------
// FormValidator — discriminated union (catalog §5 0x031D)
// -----------------------------------------------------------------------------

/// Form-level cross-field validator (catalog §5 `FormValidator`).
#[derive(Debug, Clone, PartialEq)]
pub enum FormValidator {
    AllRequired {
        field_ids: Vec<String>,
    },
    AnyRequired {
        field_ids: Vec<String>,
        error_message: BindRef,
    },
    Match {
        field_a: String,
        field_b: String,
    },
    Custom {
        id: String,
        params: Option<CborMap>,
    },
}

impl<C> Encode<C> for FormValidator {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical tstr key order is bytewise on the full encoded form (length
        // prefix included). For tstr major type 3 the length byte = 0x60 + len.
        // Used keys, with their length-byte prefix:
        //   "id"             (0x62 …)
        //   "kind"           (0x64 …)
        //   "params"         (0x66 …)
        //   "field_a"        (0x67 …)
        //   "field_b"        (0x67 …)        ('a' < 'b')
        //   "field_ids"      (0x69 …)
        //   "error_message"  (0x6d …)
        // Per variant we only emit a subset, in this exact global order.
        match self {
            FormValidator::AllRequired { field_ids } => {
                e.map(2)?;
                e.str("kind")?.str("all_required")?;
                e.str("field_ids")?;
                e.array(field_ids.len() as u64)?;
                for s in field_ids {
                    e.str(s)?;
                }
            }
            FormValidator::AnyRequired {
                field_ids,
                error_message,
            } => {
                e.map(3)?;
                // "kind"(0x64) < "field_ids"(0x69) < "error_message"(0x6d).
                e.str("kind")?.str("any_required")?;
                e.str("field_ids")?;
                e.array(field_ids.len() as u64)?;
                for s in field_ids {
                    e.str(s)?;
                }
                e.str("error_message")?;
                error_message.encode(e, ctx)?;
            }
            FormValidator::Match { field_a, field_b } => {
                e.map(3)?;
                // "kind"(0x64) < "field_a"(0x67) < "field_b"(0x67).
                e.str("kind")?.str("match")?;
                e.str("field_a")?.str(field_a)?;
                e.str("field_b")?.str(field_b)?;
            }
            FormValidator::Custom { id, params } => {
                let n = if params.is_some() { 3 } else { 2 };
                e.map(n)?;
                // "id"(0x62) < "kind"(0x64) < "params"(0x66 70).
                e.str("id")?.str(id)?;
                e.str("kind")?.str("custom")?;
                if let Some(p) = params {
                    e.str("params")?;
                    p.encode(e, ctx)?;
                }
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FormValidator {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut field_ids: Option<Vec<String>> = None;
        let mut error_message: Option<BindRef> = None;
        let mut field_a: Option<String> = None;
        let mut field_b: Option<String> = None;
        let mut id: Option<String> = None;
        let mut params: Option<CborMap> = None;
        let mut seen_kind = false;
        let mut seen_field_ids = false;
        let mut seen_error_message = false;
        let mut seen_field_a = false;
        let mut seen_field_b = false;
        let mut seen_id = false;
        let mut seen_params = false;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    if seen_kind {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'kind'",
                        ));
                    }
                    seen_kind = true;
                    kind = Some(d.str()?.to_string());
                }
                "field_ids" => {
                    if seen_field_ids {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'field_ids'",
                        ));
                    }
                    seen_field_ids = true;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut out = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        out.push(d.str()?.to_string());
                    }
                    field_ids = Some(out);
                }
                "error_message" => {
                    if seen_error_message {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'error_message'",
                        ));
                    }
                    seen_error_message = true;
                    error_message = Some(BindRef::decode(d, ctx)?);
                }
                "field_a" => {
                    if seen_field_a {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'field_a'",
                        ));
                    }
                    seen_field_a = true;
                    field_a = Some(d.str()?.to_string());
                }
                "field_b" => {
                    if seen_field_b {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'field_b'",
                        ));
                    }
                    seen_field_b = true;
                    field_b = Some(d.str()?.to_string());
                }
                "id" => {
                    if seen_id {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'id'",
                        ));
                    }
                    seen_id = true;
                    id = Some(d.str()?.to_string());
                }
                "params" => {
                    if seen_params {
                        return Err(minicbor::decode::Error::message(
                            "FormValidator: duplicate key 'params'",
                        ));
                    }
                    seen_params = true;
                    params = Some(CborMap::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown FormValidator key: {other}"
                    )));
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("FormValidator: missing 'kind'"))?;
        match kind.as_str() {
            "all_required" => {
                if seen_error_message || seen_field_a || seen_field_b || seen_id || seen_params {
                    return Err(minicbor::decode::Error::message(
                        "FormValidator::all_required: unexpected key",
                    ));
                }
                Ok(FormValidator::AllRequired {
                    field_ids: field_ids.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "FormValidator::all_required: missing 'field_ids'",
                        )
                    })?,
                })
            }
            "any_required" => {
                if seen_field_a || seen_field_b || seen_id || seen_params {
                    return Err(minicbor::decode::Error::message(
                        "FormValidator::any_required: unexpected key",
                    ));
                }
                Ok(FormValidator::AnyRequired {
                    field_ids: field_ids.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "FormValidator::any_required: missing 'field_ids'",
                        )
                    })?,
                    error_message: error_message.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "FormValidator::any_required: missing 'error_message'",
                        )
                    })?,
                })
            }
            "match" => {
                if seen_field_ids || seen_error_message || seen_id || seen_params {
                    return Err(minicbor::decode::Error::message(
                        "FormValidator::match: unexpected key",
                    ));
                }
                Ok(FormValidator::Match {
                    field_a: field_a.ok_or_else(|| {
                        minicbor::decode::Error::message("FormValidator::match: missing 'field_a'")
                    })?,
                    field_b: field_b.ok_or_else(|| {
                        minicbor::decode::Error::message("FormValidator::match: missing 'field_b'")
                    })?,
                })
            }
            "custom" => {
                if seen_field_ids || seen_error_message || seen_field_a || seen_field_b {
                    return Err(minicbor::decode::Error::message(
                        "FormValidator::custom: unexpected key",
                    ));
                }
                Ok(FormValidator::Custom {
                    id: id.ok_or_else(|| {
                        minicbor::decode::Error::message("FormValidator::custom: missing 'id'")
                    })?,
                    params,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "FormValidator: unknown kind '{other}'"
            ))),
        }
    }
}

// -----------------------------------------------------------------------------
// 0x031A — FormField
// -----------------------------------------------------------------------------

/// Wrapper for any form input with label/hint/error (catalog §5 0x031A).
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    pub label: BindRef,
    pub hint: Option<BindRef>,
    pub error: Option<BindRef>,
    pub required: bool,
    pub child: Component,
    pub layout: FormFieldLayout,
}

impl FormField {
    pub const TAG: u16 = 0x031A;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.label)?));
        if let Some(v) = &self.hint {
            e.push((1, encode_to_value(v)?));
        }
        if let Some(v) = &self.error {
            e.push((2, encode_to_value(v)?));
        }
        e.push((3, encode_to_value(&self.required)?));
        e.push((4, encode_to_value(&self.child)?));
        e.push((5, encode_to_value(&self.layout)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FormField")?;
        ensure_no_duplicate_keys("FormField", &c.fields.0)?;
        let mut label = None;
        let mut hint = None;
        let mut error = None;
        let mut required = None;
        let mut child = None;
        let mut layout = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => label = Some(decode_from_value(v)?),
                1 => hint = Some(decode_from_value(v)?),
                2 => error = Some(decode_from_value(v)?),
                3 => required = Some(decode_from_value(v)?),
                4 => child = Some(decode_from_value(v)?),
                5 => layout = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FormField", *other)),
            }
        }
        Ok(FormField {
            label: label.ok_or_else(|| missing_field("FormField", "label"))?,
            hint,
            error,
            required: required.ok_or_else(|| missing_field("FormField", "required"))?,
            child: child.ok_or_else(|| missing_field("FormField", "child"))?,
            layout: layout.ok_or_else(|| missing_field("FormField", "layout"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x031B — FormGroup
// -----------------------------------------------------------------------------

/// Group of FormFields with optional collapsible header (catalog §5 0x031B).
#[derive(Debug, Clone, PartialEq)]
pub struct FormGroup {
    pub title: Option<BindRef>,
    pub description: Option<BindRef>,
    pub collapsible: bool,
    pub expanded: Option<BindRef>,
    pub children: Vec<Component>,
    pub spacing: Spacing,
}

impl FormGroup {
    pub const TAG: u16 = 0x031B;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        if let Some(v) = &self.title {
            e.push((0, encode_to_value(v)?));
        }
        if let Some(v) = &self.description {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.collapsible)?));
        if let Some(v) = &self.expanded {
            e.push((3, encode_to_value(v)?));
        }
        e.push((4, encode_to_value(&self.children)?));
        e.push((5, encode_to_value(&self.spacing)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FormGroup")?;
        ensure_no_duplicate_keys("FormGroup", &c.fields.0)?;
        let mut title = None;
        let mut description = None;
        let mut collapsible = None;
        let mut expanded = None;
        let mut children = None;
        let mut spacing = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => description = Some(decode_from_value(v)?),
                2 => collapsible = Some(decode_from_value(v)?),
                3 => expanded = Some(decode_from_value(v)?),
                4 => children = Some(decode_from_value(v)?),
                5 => spacing = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FormGroup", *other)),
            }
        }
        Ok(FormGroup {
            title,
            description,
            collapsible: collapsible.ok_or_else(|| missing_field("FormGroup", "collapsible"))?,
            expanded,
            children: children.ok_or_else(|| missing_field("FormGroup", "children"))?,
            spacing: spacing.ok_or_else(|| missing_field("FormGroup", "spacing"))?,
        })
    }
}

// -----------------------------------------------------------------------------
// 0x031C — FormSection
// -----------------------------------------------------------------------------

/// Form section with divider and heavier heading (catalog §5 0x031C).
#[derive(Debug, Clone, PartialEq)]
pub struct FormSection {
    pub title: BindRef,
    pub description: Option<BindRef>,
    pub children: Vec<Component>,
    pub spacing: Spacing,
    pub divider_top: bool,
}

impl FormSection {
    pub const TAG: u16 = 0x031C;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(5);
        e.push((0, encode_to_value(&self.title)?));
        if let Some(v) = &self.description {
            e.push((1, encode_to_value(v)?));
        }
        e.push((2, encode_to_value(&self.children)?));
        e.push((3, encode_to_value(&self.spacing)?));
        e.push((4, encode_to_value(&self.divider_top)?));
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "FormSection")?;
        ensure_no_duplicate_keys("FormSection", &c.fields.0)?;
        let mut title = None;
        let mut description = None;
        let mut children = None;
        let mut spacing = None;
        let mut divider_top = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => title = Some(decode_from_value(v)?),
                1 => description = Some(decode_from_value(v)?),
                2 => children = Some(decode_from_value(v)?),
                3 => spacing = Some(decode_from_value(v)?),
                4 => divider_top = Some(decode_from_value(v)?),
                other => return Err(unknown_field("FormSection", *other)),
            }
        }
        Ok(FormSection {
            title: title.ok_or_else(|| missing_field("FormSection", "title"))?,
            description,
            children: children.ok_or_else(|| missing_field("FormSection", "children"))?,
            // §5 0x031C default: spacing = Lg.
            spacing: spacing.unwrap_or(Spacing::Lg),
            // §5 0x031C default: divider_top = true.
            divider_top: divider_top.unwrap_or(true),
        })
    }
}

// -----------------------------------------------------------------------------
// 0x031D — Form
// -----------------------------------------------------------------------------

/// Explicit form container with submit scope (catalog §5 0x031D).
#[derive(Debug, Clone, PartialEq)]
pub struct Form {
    pub children: Vec<Component>,
    pub scope_id: String,
    pub validators: Vec<FormValidator>,
    pub prevent_default_submit: bool,
    pub layout: FormLayout,
    pub disabled: Option<BindRef>,
}

impl Form {
    pub const TAG: u16 = 0x031D;

    pub fn into_component(self, id: impl Into<String>) -> Result<Component, IntoComponentError> {
        let mut e: Vec<(u8, Value)> = Vec::with_capacity(6);
        e.push((0, encode_to_value(&self.children)?));
        e.push((1, encode_to_value(&self.scope_id)?));
        e.push((2, encode_to_value(&self.validators)?));
        e.push((3, encode_to_value(&self.prevent_default_submit)?));
        e.push((4, encode_to_value(&self.layout)?));
        if let Some(v) = &self.disabled {
            e.push((5, encode_to_value(v)?));
        }
        Ok(component(Self::TAG, id, e))
    }

    pub fn try_from_component(c: &Component) -> Result<Self, minicbor::decode::Error> {
        ensure_tag(c.tag, Self::TAG, "Form")?;
        ensure_no_duplicate_keys("Form", &c.fields.0)?;
        let mut children = None;
        let mut scope_id = None;
        let mut validators = None;
        let mut prevent_default_submit = None;
        let mut layout = None;
        let mut disabled = None;
        for (k, v) in &c.fields.0 {
            match k {
                0 => children = Some(decode_from_value(v)?),
                1 => scope_id = Some(decode_from_value(v)?),
                2 => validators = Some(decode_from_value(v)?),
                3 => prevent_default_submit = Some(decode_from_value(v)?),
                4 => layout = Some(decode_from_value(v)?),
                5 => disabled = Some(decode_from_value(v)?),
                other => return Err(unknown_field("Form", *other)),
            }
        }
        Ok(Form {
            children: children.ok_or_else(|| missing_field("Form", "children"))?,
            scope_id: scope_id.ok_or_else(|| missing_field("Form", "scope_id"))?,
            validators: validators.ok_or_else(|| missing_field("Form", "validators"))?,
            prevent_default_submit: prevent_default_submit
                .ok_or_else(|| missing_field("Form", "prevent_default_submit"))?,
            layout: layout.ok_or_else(|| missing_field("Form", "layout"))?,
            disabled,
        })
    }
}
