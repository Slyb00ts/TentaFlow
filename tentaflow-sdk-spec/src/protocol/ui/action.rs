// =============================================================================
// File: protocol/ui/action.rs — Action + ActionAck (§6.5)
// Purpose: typed user-action submission and ack reply. Action carries an
// idempotency client_action_id; ActionAck carries a status discriminated union
// covering ok / rejected / permission_denied / rate_limited / validation_failed
// / error / redirected outcomes.
// =============================================================================

use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::protocol::control::CborMap;
use crate::protocol::ids::ClientActionId;
use crate::protocol::ui::typed_field::assert_no_dup_tstr;
use crate::protocol::value::Value;

/// `FormFieldValue` (§6.5). Tuple of value + locally-validated flag.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct FormFieldValue {
    #[n(0)]
    pub value: Value,
    #[n(1)]
    pub validated_locally: bool,
}

/// `map<tstr, FormFieldValue>` ordered + canonical-sorted on encode.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormFieldMap(pub Vec<(String, FormFieldValue)>);

impl<C> Encode<C> for FormFieldMap {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Canonical key order (§2.1): bytewise-lex on full CBOR encoding of tstr key.
        let mut indexed: Vec<(Vec<u8>, &(String, FormFieldValue))> =
            Vec::with_capacity(self.0.len());
        for entry in &self.0 {
            let mut key_bytes = Vec::with_capacity(entry.0.len() + 9);
            let mut key_enc = Encoder::new(&mut key_bytes);
            key_enc.str(&entry.0).expect("Vec writer is infallible");
            indexed.push((key_bytes, entry));
        }
        indexed.sort_by(|a, b| a.0.cmp(&b.0));
        e.map(self.0.len() as u64)?;
        for (_, (k, v)) in &indexed {
            e.str(k)?;
            v.encode(e, ctx)?;
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for FormFieldMap {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut entries = Vec::with_capacity(len as usize);
        for _ in 0..len {
            let k = d.str()?.to_string();
            let v = FormFieldValue::decode(d, ctx)?;
            entries.push((k, v));
        }
        Ok(FormFieldMap(entries))
    }
}

/// `Action` (0x0130). Frontend→Core→Addon.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct Action {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub action_id: String,
    #[n(4)]
    pub params: CborMap,
    #[n(5)]
    pub form_values: Option<FormFieldMap>,
    #[n(6)]
    pub user_gesture: bool,
    #[n(7)]
    pub client_action_id: ClientActionId,
}

/// `FieldError` (§6.5). Per-field validation failure.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct FieldError {
    #[n(0)]
    pub field_id: String,
    /// Raw u16 — addon-defined error codes may live outside the §16 whitelist.
    #[n(1)]
    pub error_code: u16,
    #[n(2)]
    pub message: String,
}

/// `ParamEntry` (§6.5 ActionStatus::redirected.params).
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ParamEntry {
    #[n(0)]
    pub key: String,
    #[n(1)]
    pub value: Value,
}

/// `ActionStatus` (§6.5). Discriminated outcome of an Action.
#[derive(Debug, Clone, PartialEq)]
pub enum ActionStatus {
    Ok,
    Rejected {
        reason: String,
        error_code: u16,
    },
    PermissionDenied {
        required_permission: String,
    },
    RateLimited {
        retry_after_ms: u32,
    },
    ValidationFailed {
        field_errors: Vec<FieldError>,
    },
    Error {
        error_code: u16,
        message: String,
    },
    Redirected {
        to_action_id: String,
        params: Vec<ParamEntry>,
    },
}

impl<C> Encode<C> for ActionStatus {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        // Keys involved across variants (sort by bytewise CBOR encoding of tstr):
        //   "kind"                 0x64 6b..
        //   "params"               0x66 70..
        //   "reason"               0x66 72..
        //   "message"              0x67 6d..
        //   "error_code"           0x6a 65..
        //   "field_errors"         0x6c 66..
        //   "to_action_id"         0x6c 74..
        //   "retry_after_ms"       0x6e 72..
        //   "required_permission"  0x73 72..
        match self {
            ActionStatus::Ok => {
                e.map(1)?;
                e.str("kind")?.str("ok")?;
            }
            ActionStatus::Rejected { reason, error_code } => {
                e.map(3)?;
                e.str("kind")?.str("rejected")?;
                e.str("reason")?.str(reason)?;
                e.str("error_code")?.u16(*error_code)?;
            }
            ActionStatus::PermissionDenied {
                required_permission,
            } => {
                e.map(2)?;
                e.str("kind")?.str("permission_denied")?;
                e.str("required_permission")?.str(required_permission)?;
            }
            ActionStatus::RateLimited { retry_after_ms } => {
                e.map(2)?;
                e.str("kind")?.str("rate_limited")?;
                e.str("retry_after_ms")?.u32(*retry_after_ms)?;
            }
            ActionStatus::ValidationFailed { field_errors } => {
                e.map(2)?;
                e.str("kind")?.str("validation_failed")?;
                e.str("field_errors")?;
                e.array(field_errors.len() as u64)?;
                for fe in field_errors {
                    fe.encode(e, ctx)?;
                }
            }
            ActionStatus::Error {
                error_code,
                message,
            } => {
                e.map(3)?;
                e.str("kind")?.str("error")?;
                e.str("message")?.str(message)?;
                e.str("error_code")?.u16(*error_code)?;
            }
            ActionStatus::Redirected {
                to_action_id,
                params,
            } => {
                e.map(3)?;
                e.str("kind")?.str("redirected")?;
                e.str("params")?;
                e.array(params.len() as u64)?;
                for p in params {
                    p.encode(e, ctx)?;
                }
                e.str("to_action_id")?.str(to_action_id)?;
            }
        }
        Ok(())
    }
}

impl<'b, C> Decode<'b, C> for ActionStatus {
    fn decode(d: &mut Decoder<'b>, ctx: &mut C) -> Result<Self, minicbor::decode::Error> {
        let len = d
            .map()?
            .ok_or_else(|| minicbor::decode::Error::message("indefinite-length map forbidden"))?;
        let mut kind: Option<String> = None;
        let mut reason: Option<String> = None;
        let mut error_code: Option<u16> = None;
        let mut required_permission: Option<String> = None;
        let mut retry_after_ms: Option<u32> = None;
        let mut field_errors: Option<Vec<FieldError>> = None;
        let mut message: Option<String> = None;
        let mut to_action_id: Option<String> = None;
        let mut params: Option<Vec<ParamEntry>> = None;
        for _ in 0..len {
            let k = d.str()?;
            match k {
                "kind" => {
                    assert_no_dup_tstr(&kind, "ActionStatus", "kind")?;
                    kind = Some(d.str()?.to_string());
                }
                "reason" => {
                    assert_no_dup_tstr(&reason, "ActionStatus", "reason")?;
                    reason = Some(d.str()?.to_string());
                }
                "error_code" => {
                    assert_no_dup_tstr(&error_code, "ActionStatus", "error_code")?;
                    error_code = Some(d.u16()?);
                }
                "required_permission" => {
                    assert_no_dup_tstr(
                        &required_permission,
                        "ActionStatus",
                        "required_permission",
                    )?;
                    required_permission = Some(d.str()?.to_string());
                }
                "retry_after_ms" => {
                    assert_no_dup_tstr(&retry_after_ms, "ActionStatus", "retry_after_ms")?;
                    retry_after_ms = Some(d.u32()?);
                }
                "field_errors" => {
                    assert_no_dup_tstr(&field_errors, "ActionStatus", "field_errors")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(FieldError::decode(d, ctx)?);
                    }
                    field_errors = Some(v);
                }
                "message" => {
                    assert_no_dup_tstr(&message, "ActionStatus", "message")?;
                    message = Some(d.str()?.to_string());
                }
                "to_action_id" => {
                    assert_no_dup_tstr(&to_action_id, "ActionStatus", "to_action_id")?;
                    to_action_id = Some(d.str()?.to_string());
                }
                "params" => {
                    assert_no_dup_tstr(&params, "ActionStatus", "params")?;
                    let n = d.array()?.ok_or_else(|| {
                        minicbor::decode::Error::message("indefinite-length array forbidden")
                    })?;
                    let mut v = Vec::with_capacity(n as usize);
                    for _ in 0..n {
                        v.push(ParamEntry::decode(d, ctx)?);
                    }
                    params = Some(v);
                }
                other => {
                    return Err(minicbor::decode::Error::message(format!(
                        "unknown ActionStatus key: {other}"
                    )))
                }
            }
        }
        let kind =
            kind.ok_or_else(|| minicbor::decode::Error::message("ActionStatus missing kind"))?;
        // Fixed-width per-variant whitelist.
        const FIELD_COUNT: usize = 8;
        let present: [bool; FIELD_COUNT] = [
            reason.is_some(),
            error_code.is_some(),
            required_permission.is_some(),
            retry_after_ms.is_some(),
            field_errors.is_some(),
            message.is_some(),
            to_action_id.is_some(),
            params.is_some(),
        ];
        let want_only = |allowed: &[bool; FIELD_COUNT]| -> Result<(), minicbor::decode::Error> {
            for i in 0..FIELD_COUNT {
                if !allowed[i] && present[i] {
                    return Err(minicbor::decode::Error::message(
                        "ActionStatus variant carries a field not allowed by its kind",
                    ));
                }
            }
            Ok(())
        };
        // Indices: [reason, error_code, required_permission, retry_after_ms,
        //           field_errors, message, to_action_id, params].
        match kind.as_str() {
            "ok" => {
                want_only(&[false, false, false, false, false, false, false, false])?;
                Ok(ActionStatus::Ok)
            }
            "rejected" => {
                want_only(&[true, true, false, false, false, false, false, false])?;
                Ok(ActionStatus::Rejected {
                    reason: reason.ok_or_else(|| {
                        minicbor::decode::Error::message("rejected missing reason")
                    })?,
                    error_code: error_code.ok_or_else(|| {
                        minicbor::decode::Error::message("rejected missing error_code")
                    })?,
                })
            }
            "permission_denied" => {
                want_only(&[false, false, true, false, false, false, false, false])?;
                Ok(ActionStatus::PermissionDenied {
                    required_permission: required_permission.ok_or_else(|| {
                        minicbor::decode::Error::message(
                            "permission_denied missing required_permission",
                        )
                    })?,
                })
            }
            "rate_limited" => {
                want_only(&[false, false, false, true, false, false, false, false])?;
                Ok(ActionStatus::RateLimited {
                    retry_after_ms: retry_after_ms.ok_or_else(|| {
                        minicbor::decode::Error::message("rate_limited missing retry_after_ms")
                    })?,
                })
            }
            "validation_failed" => {
                want_only(&[false, false, false, false, true, false, false, false])?;
                Ok(ActionStatus::ValidationFailed {
                    field_errors: field_errors.ok_or_else(|| {
                        minicbor::decode::Error::message("validation_failed missing field_errors")
                    })?,
                })
            }
            "error" => {
                want_only(&[false, true, false, false, false, true, false, false])?;
                Ok(ActionStatus::Error {
                    error_code: error_code.ok_or_else(|| {
                        minicbor::decode::Error::message("error missing error_code")
                    })?,
                    message: message
                        .ok_or_else(|| minicbor::decode::Error::message("error missing message"))?,
                })
            }
            "redirected" => {
                want_only(&[false, false, false, false, false, false, true, true])?;
                Ok(ActionStatus::Redirected {
                    to_action_id: to_action_id.ok_or_else(|| {
                        minicbor::decode::Error::message("redirected missing to_action_id")
                    })?,
                    params: params.ok_or_else(|| {
                        minicbor::decode::Error::message("redirected missing params")
                    })?,
                })
            }
            other => Err(minicbor::decode::Error::message(format!(
                "unknown ActionStatus.kind: {other}"
            ))),
        }
    }
}

/// `ActionAck` (0x0131). Addon→Frontend.
#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cbor(map)]
pub struct ActionAck {
    #[n(0)]
    pub addon_id: String,
    #[n(1)]
    pub panel_id: String,
    #[n(2)]
    pub panel_epoch: u64,
    #[n(3)]
    pub action_id: String,
    #[n(4)]
    pub client_action_id: ClientActionId,
    #[n(5)]
    pub status: ActionStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn cid() -> ClientActionId {
        crate::protocol::ids::ClientActionId::from_bytes([7; 16])
    }

    #[test]
    fn form_field_value_roundtrip() {
        rt(FormFieldValue {
            value: Value::Text("hello".into()),
            validated_locally: true,
        });
    }

    #[test]
    fn form_field_map_canonical_sort() {
        let m = FormFieldMap(vec![
            (
                "zzz".into(),
                FormFieldValue {
                    value: Value::Bool(true),
                    validated_locally: false,
                },
            ),
            (
                "a".into(),
                FormFieldValue {
                    value: Value::U64(1),
                    validated_locally: true,
                },
            ),
        ]);
        let mut buf = Vec::new();
        minicbor::encode(&m, &mut buf).unwrap();
        let d: FormFieldMap = minicbor::decode(&buf).unwrap();
        let keys: Vec<&str> = d.0.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["a", "zzz"]);
    }

    #[test]
    fn action_roundtrip() {
        rt(Action {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "save".into(),
            params: CborMap::default(),
            form_values: None,
            user_gesture: true,
            client_action_id: cid(),
        });
    }

    #[test]
    fn action_status_all_variants_roundtrip() {
        rt(ActionStatus::Ok);
        rt(ActionStatus::Rejected {
            reason: "denied".into(),
            error_code: 0x1305,
        });
        rt(ActionStatus::PermissionDenied {
            required_permission: "cameras.read".into(),
        });
        rt(ActionStatus::RateLimited {
            retry_after_ms: 5000,
        });
        rt(ActionStatus::ValidationFailed {
            field_errors: vec![FieldError {
                field_id: "email".into(),
                error_code: 0x1501,
                message: "not an email".into(),
            }],
        });
        rt(ActionStatus::Error {
            error_code: 0x1F01,
            message: "boom".into(),
        });
        rt(ActionStatus::Redirected {
            to_action_id: "mfa".into(),
            params: vec![ParamEntry {
                key: "challenge_id".into(),
                value: Value::Text("abc".into()),
            }],
        });
    }

    #[test]
    fn action_status_ok_with_extra_field_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2)
            .unwrap()
            .str("kind")
            .unwrap()
            .str("ok")
            .unwrap()
            .str("reason")
            .unwrap()
            .str("nope")
            .unwrap();
        let res: Result<ActionStatus, _> = minicbor::decode(&buf);
        assert!(res.is_err());
    }

    fn assert_extra_field_rejected(
        kind: &str,
        extra_key: &str,
        extra_value_emit: impl FnOnce(&mut minicbor::Encoder<&mut Vec<u8>>),
    ) {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.map(2).unwrap().str("kind").unwrap().str(kind).unwrap();
        enc.str(extra_key).unwrap();
        extra_value_emit(&mut enc);
        let res: Result<ActionStatus, _> = minicbor::decode(&buf);
        assert!(
            res.is_err(),
            "ActionStatus.{kind} must reject foreign field {extra_key}"
        );
    }

    #[test]
    fn action_status_each_variant_rejects_foreign_field() {
        // For each kind, send one valid-but-not-allowed field and assert decode fails.
        assert_extra_field_rejected("ok", "reason", |e| {
            e.str("x").unwrap();
        });
        assert_extra_field_rejected("rejected", "retry_after_ms", |e| {
            e.u32(1).unwrap();
        });
        assert_extra_field_rejected("permission_denied", "error_code", |e| {
            e.u16(1).unwrap();
        });
        assert_extra_field_rejected("rate_limited", "reason", |e| {
            e.str("x").unwrap();
        });
        assert_extra_field_rejected("validation_failed", "reason", |e| {
            e.str("x").unwrap();
        });
        assert_extra_field_rejected("error", "reason", |e| {
            e.str("x").unwrap();
        });
        assert_extra_field_rejected("redirected", "reason", |e| {
            e.str("x").unwrap();
        });
    }

    #[test]
    fn action_ack_roundtrip() {
        rt(ActionAck {
            addon_id: "a".into(),
            panel_id: "p".into(),
            panel_epoch: 1,
            action_id: "save".into(),
            client_action_id: cid(),
            status: ActionStatus::Ok,
        });
    }
}
