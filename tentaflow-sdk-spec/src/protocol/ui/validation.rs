// =============================================================================
// File: protocol/ui/validation.rs — ValidationRule + StateCondition + FailurePolicy
// Purpose: leaf primitives used by Handler/LocalAction (no recursive nesting
// inside these types). FailurePolicy::Custom does carry a LocalAction reference
// — defined in handler.rs and avoided here to keep the import graph acyclic.
// Schemas: catalog §1.5 (ValidationRule), protocol §10.3 (StateCondition + FailurePolicy).
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;
use crate::protocol::value::Value;

use super::bind::StatePath;
use crate::protocol::ui::typed_field::assert_no_dup_tstr;

/// Field-level validation rule (catalog §1.5 `ValidationRule`).
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationRule {
    Required,
    MinLength {
        value: u16,
    },
    MaxLength {
        value: u16,
    },
    Min {
        value: f64,
    },
    Max {
        value: f64,
    },
    Pattern {
        regex: String,
    },
    Email,
    Url {
        schemes: Vec<String>,
    },
    Iban,
    Phone {
        region: Option<String>,
    },
    Uuid,
    DateRange {
        min: Option<String>,
        max: Option<String>,
    },
    Custom {
        id: String,
        params: Option<CborMap>,
    },
}

impl<C> Encode<C> for ValidationRule {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order across involved tstr keys:
        //   "id"      (0x62 ..)
        //   "max"     (0x63 ..)
        //   "min"     (0x63 ..)
        //   "kind"    (0x64 ..)
        //   "regex"   (0x65 ..)
        //   "value"   (0x65 ..)
        //   "params"  (0x66 70 ..)
        //   "region"  (0x66 72 ..)
        //   "schemes" (0x67 ..)
        // We emit per-variant explicitly.
        match self {
            ValidationRule::Required => {
                e.map(1)?;
                e.str("kind")?.str("required")?;
            }
            ValidationRule::MinLength { value } => {
                e.map(2)?;
                e.str("kind")?.str("min_length")?;
                e.str("value")?.u16(*value)?;
            }
            ValidationRule::MaxLength { value } => {
                e.map(2)?;
                e.str("kind")?.str("max_length")?;
                e.str("value")?.u16(*value)?;
            }
            ValidationRule::Min { value } => {
                e.map(2)?;
                e.str("kind")?.str("min")?;
                e.str("value")?.f64(*value)?;
            }
            ValidationRule::Max { value } => {
                e.map(2)?;
                e.str("kind")?.str("max")?;
                e.str("value")?.f64(*value)?;
            }
            ValidationRule::Pattern { regex } => {
                e.map(2)?;
                e.str("kind")?.str("pattern")?;
                e.str("regex")?.str(regex)?;
            }
            ValidationRule::Email => {
                e.map(1)?;
                e.str("kind")?.str("email")?;
            }
            ValidationRule::Url { schemes } => {
                e.map(2)?;
                e.str("kind")?.str("url")?;
                e.str("schemes")?;
                e.array(schemes.len() as u64)?;
                for s in schemes {
                    e.str(s)?;
                }
            }
            ValidationRule::Iban => {
                e.map(1)?;
                e.str("kind")?.str("iban")?;
            }
            ValidationRule::Phone { region } => {
                let n = if region.is_some() { 2 } else { 1 };
                e.map(n)?;
                e.str("kind")?.str("phone")?;
                if let Some(r) = region {
                    e.str("region")?.str(r)?;
                }
            }
            ValidationRule::Uuid => {
                e.map(1)?;
                e.str("kind")?.str("uuid")?;
            }
            ValidationRule::DateRange { min, max } => {
                let mut n = 1;
                if max.is_some() {
                    n += 1;
                }
                if min.is_some() {
                    n += 1;
                }
                e.map(n)?;
                // Canonical: kind(0x64..) < max(0x63..) wait — 0x63 < 0x64.
                // Recompute: "max"=0x63, "min"=0x63, "kind"=0x64. So max,min < kind.
                // And "max" vs "min": both start 0x63; second bytes 'm'(0x6d) for both,
                // third byte 'a'(0x61) vs 'i'(0x69). 'a' < 'i' → "max" < "min".
                if let Some(m) = max {
                    e.str("max")?.str(m)?;
                }
                if let Some(m) = min {
                    e.str("min")?.str(m)?;
                }
                e.str("kind")?.str("date_range")?;
            }
            ValidationRule::Custom { id, params } => {
                let n = if params.is_some() { 3 } else { 2 };
                e.map(n)?;
                // "id"=0x62, "kind"=0x64, "params"=0x66 → id < kind < params.
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

impl<'b, C> Decode<'b, C> for ValidationRule {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut value_u16: Option<u16> = None;
        let mut value_f64: Option<f64> = None;
        let mut regex: Option<String> = None;
        let mut schemes: Option<Vec<String>> = None;
        let mut region: Option<String> = None;
        let mut min_s: Option<String> = None;
        let mut max_s: Option<String> = None;
        let mut id: Option<String> = None;
        let mut params: Option<CborMap> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "ValidationRule", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "value" => {
                    if value_u16.is_some() || value_f64.is_some() {
                        return Err(minicbor::decode::Error::message(
                            "ValidationRule: duplicate key 'value'",
                        ));
                    }
                    match kind.as_deref() {
                        Some("min_length") | Some("max_length") => value_u16 = Some(d.u16()?),
                        Some("min") | Some("max") => value_f64 = Some(d.f64()?),
                        _ => {
                            // Type-peek fallback.
                            match d.datatype()? {
                                minicbor::data::Type::F16
                                | minicbor::data::Type::F32
                                | minicbor::data::Type::F64 => value_f64 = Some(d.f64()?),
                                _ => value_u16 = Some(d.u16()?),
                            }
                        }
                    }
                }
                "regex" => {
                    assert_no_dup_tstr(&regex, "ValidationRule", "regex")?;
                    regex = Some(d.str()?.to_string());
                }
                "schemes" => {
                    assert_no_dup_tstr(&schemes, "ValidationRule", "schemes")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(d.str()?.to_string());
                    }
                    schemes = Some(v);
                }
                "region" => {
                    assert_no_dup_tstr(&region, "ValidationRule", "region")?;
                    region = Some(d.str()?.to_string());
                }
                "min" => {
                    assert_no_dup_tstr(&min_s, "ValidationRule", "min")?;
                    min_s = Some(d.str()?.to_string());
                }
                "max" => {
                    assert_no_dup_tstr(&max_s, "ValidationRule", "max")?;
                    max_s = Some(d.str()?.to_string());
                }
                "id" => {
                    assert_no_dup_tstr(&id, "ValidationRule", "id")?;
                    id = Some(d.str()?.to_string());
                }
                "params" => {
                    assert_no_dup_tstr(&params, "ValidationRule", "params")?;
                    params = Some(CborMap::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown ValidationRule key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("ValidationRule missing kind"))?;
        let allowed = |val_u16: bool,
                       val_f64: bool,
                       allow_regex: bool,
                       allow_schemes: bool,
                       allow_region: bool,
                       allow_min: bool,
                       allow_max: bool,
                       allow_id: bool,
                       allow_params: bool|
         -> Result<(), minicbor::decode::Error> {
            if !val_u16 && value_u16.is_some()
                || !val_f64 && value_f64.is_some()
                || !allow_regex && regex.is_some()
                || !allow_schemes && schemes.is_some()
                || !allow_region && region.is_some()
                || !allow_min && min_s.is_some()
                || !allow_max && max_s.is_some()
                || !allow_id && id.is_some()
                || !allow_params && params.is_some()
            {
                return Err(minicbor::decode::Error::message(
                    "ValidationRule variant carries fields not allowed by its kind",
                ));
            }
            Ok(())
        };
        match kind.as_str() {
            "required" => {
                allowed(
                    false, false, false, false, false, false, false, false, false,
                )?;
                Ok(ValidationRule::Required)
            }
            "min_length" => {
                allowed(true, false, false, false, false, false, false, false, false)?;
                Ok(ValidationRule::MinLength {
                    value: value_u16.ok_or_else(|| {
                        minicbor::decode::Error::message("min_length missing value")
                    })?,
                })
            }
            "max_length" => {
                allowed(true, false, false, false, false, false, false, false, false)?;
                Ok(ValidationRule::MaxLength {
                    value: value_u16.ok_or_else(|| {
                        minicbor::decode::Error::message("max_length missing value")
                    })?,
                })
            }
            "min" => {
                allowed(false, true, false, false, false, false, false, false, false)?;
                Ok(ValidationRule::Min {
                    value: value_f64
                        .ok_or_else(|| minicbor::decode::Error::message("min missing value"))?,
                })
            }
            "max" => {
                allowed(false, true, false, false, false, false, false, false, false)?;
                Ok(ValidationRule::Max {
                    value: value_f64
                        .ok_or_else(|| minicbor::decode::Error::message("max missing value"))?,
                })
            }
            "pattern" => {
                allowed(false, false, true, false, false, false, false, false, false)?;
                Ok(ValidationRule::Pattern {
                    regex: regex
                        .ok_or_else(|| minicbor::decode::Error::message("pattern missing regex"))?,
                })
            }
            "email" => {
                allowed(
                    false, false, false, false, false, false, false, false, false,
                )?;
                Ok(ValidationRule::Email)
            }
            "url" => {
                allowed(false, false, false, true, false, false, false, false, false)?;
                Ok(ValidationRule::Url {
                    schemes: schemes
                        .ok_or_else(|| minicbor::decode::Error::message("url missing schemes"))?,
                })
            }
            "iban" => {
                allowed(
                    false, false, false, false, false, false, false, false, false,
                )?;
                Ok(ValidationRule::Iban)
            }
            "phone" => {
                allowed(false, false, false, false, true, false, false, false, false)?;
                Ok(ValidationRule::Phone { region })
            }
            "uuid" => {
                allowed(
                    false, false, false, false, false, false, false, false, false,
                )?;
                Ok(ValidationRule::Uuid)
            }
            "date_range" => {
                allowed(false, false, false, false, false, true, true, false, false)?;
                Ok(ValidationRule::DateRange {
                    min: min_s,
                    max: max_s,
                })
            }
            "custom" => {
                allowed(false, false, false, false, false, false, false, true, true)?;
                Ok(ValidationRule::Custom {
                    id: id.ok_or_else(|| minicbor::decode::Error::message("custom missing id"))?,
                    params,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown ValidationRule.kind: {other}"
            ))),
        }
    }
}

/// Simple boolean condition over state. Non-recursive (no And/Or/Not — kept
/// flat so the renderer evaluation is O(1) per condition).
#[derive(Debug, Clone, PartialEq)]
pub enum StateCondition {
    IsTruthy { path: StatePath },
    IsFalsy { path: StatePath },
    Equals { path: StatePath, value: Value },
    NotEquals { path: StatePath, value: Value },
}

impl<C> Encode<C> for StateCondition {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical: kind(0x64..) < path(0x64..) < value(0x65..).
        // "kind"=0x64 6b, "path"=0x64 70 → kind < path. value=0x65... after.
        match self {
            StateCondition::IsTruthy { path } => {
                e.map(2)?;
                e.str("kind")?.str("is_truthy")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
            StateCondition::IsFalsy { path } => {
                e.map(2)?;
                e.str("kind")?.str("is_falsy")?;
                e.str("path")?;
                path.encode(e, ctx)?;
            }
            StateCondition::Equals { path, value } => {
                e.map(3)?;
                e.str("kind")?.str("equals")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
            StateCondition::NotEquals { path, value } => {
                e.map(3)?;
                e.str("kind")?.str("not_equals")?;
                e.str("path")?;
                path.encode(e, ctx)?;
                e.str("value")?;
                value.encode(e, ctx)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for StateCondition {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut path: Option<StatePath> = None;
        let mut value: Option<Value> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "StateCondition", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "path" => {
                    assert_no_dup_tstr(&path, "StateCondition", "path")?;
                    path = Some(StatePath::decode(d, ctx)?);
                }
                "value" => {
                    assert_no_dup_tstr(&value, "StateCondition", "value")?;
                    value = Some(Value::decode(d, ctx)?);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown StateCondition key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("StateCondition missing kind"))?;
        let path =
            path.ok_or_else(|| minicbor::decode::Error::message("StateCondition missing path"))?;
        match kind.as_str() {
            "is_truthy" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "StateCondition.is_truthy must not carry value",
                    ));
                }
                Ok(StateCondition::IsTruthy { path })
            }
            "is_falsy" => {
                if value.is_some() {
                    return Err(minicbor::decode::Error::message(
                        "StateCondition.is_falsy must not carry value",
                    ));
                }
                Ok(StateCondition::IsFalsy { path })
            }
            "equals" => Ok(StateCondition::Equals {
                path,
                value: value.ok_or_else(|| {
                    minicbor::decode::Error::message("StateCondition.equals missing value")
                })?,
            }),
            "not_equals" => Ok(StateCondition::NotEquals {
                path,
                value: value.ok_or_else(|| {
                    minicbor::decode::Error::message("StateCondition.not_equals missing value")
                })?,
            }),
            other => Err(minicbor::decode::Error::message(format!(
                "unknown StateCondition.kind: {other}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ui::bind::{PathSegment, StatePath};

    fn rt<T>(v: T)
    where
        T: minicbor::Encode<()> + for<'b> minicbor::Decode<'b, ()> + PartialEq + core::fmt::Debug,
    {
        let mut b1 = Vec::new();
        minicbor::encode(&v, &mut b1).unwrap();
        let d: T = minicbor::decode(&b1).unwrap();
        assert_eq!(d, v);
        let mut b2 = Vec::new();
        minicbor::encode(&d, &mut b2).unwrap();
        assert_eq!(b1, b2);
    }

    fn p(seg: &str) -> StatePath {
        StatePath::new(vec![PathSegment::Key(seg.into())])
    }

    #[test]
    fn validation_rule_variants_roundtrip() {
        rt(ValidationRule::Required);
        rt(ValidationRule::MinLength { value: 3 });
        rt(ValidationRule::MaxLength { value: 255 });
        rt(ValidationRule::Min { value: 0.0 });
        rt(ValidationRule::Max { value: 100.5 });
        rt(ValidationRule::Pattern {
            regex: "^[A-Z]+$".into(),
        });
        rt(ValidationRule::Email);
        rt(ValidationRule::Url {
            schemes: vec!["https".into()],
        });
        rt(ValidationRule::Iban);
        rt(ValidationRule::Phone {
            region: Some("PL".into()),
        });
        rt(ValidationRule::Phone { region: None });
        rt(ValidationRule::Uuid);
        rt(ValidationRule::DateRange {
            min: Some("2026-01-01".into()),
            max: Some("2026-12-31".into()),
        });
        rt(ValidationRule::DateRange {
            min: None,
            max: None,
        });
        rt(ValidationRule::Custom {
            id: "ssn".into(),
            params: Some(CborMap(vec![("country".into(), Value::Text("PL".into()))])),
        });
    }

    #[test]
    fn validation_rule_required_with_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("required")
            .unwrap()
            .str("value")
            .unwrap()
            .u16(1)
            .unwrap();
        let res: Result<ValidationRule, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    #[test]
    fn validation_rule_duplicate_kind_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("required")
            .unwrap()
            .str("kind")
            .unwrap()
            .str("required")
            .unwrap();
        let err = minicbor::decode::<ValidationRule>(&buf).unwrap_err();
        assert!(format!("{err}").contains("duplicate key 'kind'"));
    }

    #[test]
    fn validation_rule_duplicate_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("min_length")
            .unwrap()
            .str("value")
            .unwrap()
            .u16(2)
            .unwrap()
            .str("value")
            .unwrap()
            .u16(5)
            .unwrap();
        let err = minicbor::decode::<ValidationRule>(&buf).unwrap_err();
        assert!(format!("{err}").contains("duplicate"));
    }

    #[test]
    fn state_condition_roundtrip() {
        rt(StateCondition::IsTruthy { path: p("open") });
        rt(StateCondition::IsFalsy { path: p("hidden") });
        rt(StateCondition::Equals {
            path: p("status"),
            value: Value::Text("ready".into()),
        });
        rt(StateCondition::NotEquals {
            path: p("count"),
            value: Value::U64(0),
        });
    }

    #[test]
    fn state_condition_is_truthy_with_value_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(3)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("is_truthy")
            .unwrap()
            .str("path")
            .unwrap()
            .array(0)
            .unwrap()
            .str("value")
            .unwrap()
            .u8(1)
            .unwrap();
        let res: Result<StateCondition, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }
}
