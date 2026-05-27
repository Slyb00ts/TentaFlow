// =============================================================================
// File: flow_runtime/operators/branch.rs — predicate router
// =============================================================================
//
// Routes every inbound record to either the `"true"` or `"false"` outbound
// port based on a compiled comparison expression of the form:
//
//     <dot_path> <op> <literal>
//
// where `<op>` ∈ `== != < <= > >=` and `<literal>` is a number or a single-
// or double-quoted string. Expressions are compiled once on operator start;
// `BadParams` is returned for unparseable input so the flow is failed
// before any records are pulled.
//
// Evaluation errors (missing field, type mismatch) route to the optional
// `"error"` outbound port when present, otherwise honor `on_error`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    close_outbound, emit_op_audit, next_record, read_param_string, record_field_dot, send_to_port,
    OnError, OperatorContext, OperatorError, OutboundEdge,
};
use crate::flow_runtime::bounded_drop_oldest::BoundedDropOldest;
use crate::flow_runtime::scheduler::FlowMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone)]
enum Literal {
    Num(f64),
    Str(String),
}

#[derive(Debug, Clone)]
struct CompiledExpr {
    field: String,
    op: CompareOp,
    value: Literal,
}

fn compile_expr(raw: &str) -> Result<CompiledExpr, OperatorError> {
    let s = raw.trim();
    // Operator detection scans char-by-char while tracking quote state so
    // a literal like `name == "abc<def"` is not split on the inner `<`.
    // Two-char ops (==,!=,<=,>=) take priority over single-char (<,>) at
    // the same position; the first operator found outside a string wins.
    let bytes = s.as_bytes();
    let mut in_string: Option<u8> = None;
    let mut found: Option<(usize, &'static str)> = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = in_string {
            // Backslash escapes the next byte (typical for \" inside "..." or
            // \' inside '...'). Without this, `name == "abc\"<def"` would
            // close the string at the escaped quote and parse `<` as an op.
            if c == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if c == q {
                in_string = None;
            }
            i += 1;
            continue;
        }
        if c == b'\'' || c == b'"' {
            in_string = Some(c);
            i += 1;
            continue;
        }
        // Two-char first.
        if i + 1 < bytes.len() {
            let two = &bytes[i..i + 2];
            let op2: Option<&'static str> = match two {
                b"==" => Some("=="),
                b"!=" => Some("!="),
                b"<=" => Some("<="),
                b">=" => Some(">="),
                _ => None,
            };
            if let Some(op) = op2 {
                found = Some((i, op));
                break;
            }
        }
        if c == b'<' {
            found = Some((i, "<"));
            break;
        }
        if c == b'>' {
            found = Some((i, ">"));
            break;
        }
        i += 1;
    }
    if in_string.is_some() {
        return Err(OperatorError::BadParams(format!(
            "branch: unterminated string literal in '{raw}'"
        )));
    }
    let (idx, op_str) = found.ok_or_else(|| {
        OperatorError::BadParams(format!("branch: no comparison operator in '{raw}'"))
    })?;
    let op = match op_str {
        "==" => CompareOp::Eq,
        "!=" => CompareOp::Ne,
        "<" => CompareOp::Lt,
        "<=" => CompareOp::Le,
        ">" => CompareOp::Gt,
        ">=" => CompareOp::Ge,
        _ => unreachable!(),
    };
    let field = s[..idx].trim().to_string();
    let rhs = s[idx + op_str.len()..].trim();
    if field.is_empty() {
        return Err(OperatorError::BadParams("branch: empty field".to_string()));
    }
    if rhs.is_empty() {
        return Err(OperatorError::BadParams("branch: empty rhs".to_string()));
    }
    let value = parse_literal(rhs)?;
    Ok(CompiledExpr { field, op, value })
}

fn parse_literal(s: &str) -> Result<Literal, OperatorError> {
    let s = s.trim();
    if (s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')) {
        if s.len() < 2 {
            return Err(OperatorError::BadParams(
                "branch: malformed quoted literal".to_string(),
            ));
        }
        // Unescape: \" → ", \' → ', \\ → \. Lexer above treats `\<quote>` as
        // a literal quote inside the string; this routine collapses the
        // escape sequence into the actual character.
        let inner = &s[1..s.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else {
                out.push(c);
            }
        }
        return Ok(Literal::Str(out));
    }
    if let Ok(i) = s.parse::<i64>() {
        return Ok(Literal::Num(i as f64));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Ok(Literal::Num(f));
    }
    Err(OperatorError::BadParams(format!(
        "branch: unrecognized literal '{s}'"
    )))
}

fn eval(expr: &CompiledExpr, record: &toml::Value) -> Option<bool> {
    let raw = record_field_dot(record, &expr.field)?;
    match &expr.value {
        Literal::Num(target) => {
            let lhs = if let Some(i) = raw.as_integer() {
                i as f64
            } else if let Some(f) = raw.as_float() {
                f
            } else {
                return None;
            };
            Some(match expr.op {
                CompareOp::Eq => (lhs - *target).abs() < f64::EPSILON,
                CompareOp::Ne => (lhs - *target).abs() >= f64::EPSILON,
                CompareOp::Lt => lhs < *target,
                CompareOp::Le => lhs <= *target,
                CompareOp::Gt => lhs > *target,
                CompareOp::Ge => lhs >= *target,
            })
        }
        Literal::Str(target) => {
            let lhs = raw.as_str()?;
            Some(match expr.op {
                CompareOp::Eq => lhs == target.as_str(),
                CompareOp::Ne => lhs != target.as_str(),
                // Ordering on strings is allowed (lexicographic) — consistent
                // with serde_json semantics in similar predicate engines.
                CompareOp::Lt => lhs < target.as_str(),
                CompareOp::Le => lhs <= target.as_str(),
                CompareOp::Gt => lhs > target.as_str(),
                CompareOp::Ge => lhs >= target.as_str(),
            })
        }
    }
}

pub async fn run(
    ctx: OperatorContext,
    inbound: Vec<Arc<BoundedDropOldest<FlowMessage>>>,
    outbound: Vec<OutboundEdge>,
    cancel: CancellationToken,
) -> Result<(), OperatorError> {
    let raw_expr = read_param_string(&ctx.params, "expr")
        .ok_or_else(|| OperatorError::BadParams("branch: 'expr' required".to_string()))?;
    let expr = compile_expr(&raw_expr)?;
    let on_error = OnError::from_params(&ctx.params, OnError::Fail);
    let has_error_port = outbound.iter().any(|(p, _)| p.as_deref() == Some("error"));

    let mut eof_received = vec![false; inbound.len()];
    let mut t_count: u64 = 0;
    let mut f_count: u64 = 0;
    let mut e_count: u64 = 0;
    loop {
        let msg = next_record(&inbound, &mut eof_received, &cancel).await;
        match msg {
            None => break,
            Some(Err(())) => {
                close_outbound(&outbound);
                return Ok(());
            }
            Some(Ok(record)) => match eval(&expr, &record) {
                Some(true) => {
                    send_to_port(&outbound, "true", record);
                    t_count += 1;
                }
                Some(false) => {
                    send_to_port(&outbound, "false", record);
                    f_count += 1;
                }
                None => {
                    if has_error_port {
                        send_to_port(&outbound, "error", record);
                        e_count += 1;
                        continue;
                    }
                    match on_error {
                        OnError::Skip => {
                            e_count += 1;
                            continue;
                        }
                        OnError::EmitNull => {
                            send_to_port(
                                &outbound,
                                "false",
                                toml::Value::Table(toml::value::Table::new()),
                            );
                            e_count += 1;
                        }
                        OnError::Fail => {
                            emit_op_audit(
                                &ctx.db,
                                &ctx.addon_id,
                                &ctx.flow_id,
                                &ctx.invocation_id,
                                &ctx.operator_id,
                                "branch",
                                "error",
                                "error",
                                Some(
                                    serde_json::json!({"reason": "eval_failed", "field": expr.field}),
                                ),
                                ctx.org_id.as_deref(),
                            );
                            close_outbound(&outbound);
                            return Err(OperatorError::ExpressionFailed(format!(
                                "branch: eval failed on field '{}'",
                                expr.field
                            )));
                        }
                    }
                }
            },
        }
    }

    close_outbound(&outbound);
    emit_op_audit(
        &ctx.db,
        &ctx.addon_id,
        &ctx.flow_id,
        &ctx.invocation_id,
        &ctx.operator_id,
        "branch",
        "completed",
        "ok",
        Some(serde_json::json!({"true": t_count, "false": f_count, "error": e_count})),
        ctx.org_id.as_deref(),
    );
    Ok(())
}

#[cfg(test)]
pub fn test_compile_expr(s: &str) -> Result<(), String> {
    compile_expr(s).map(|_| ()).map_err(|e| e.to_string())
}
