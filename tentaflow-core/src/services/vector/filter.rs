// ============ File: services/vector/filter.rs — universal Filter → backend expr ============
//
// Translates the backend-agnostic `Filter` AST (from tentaflow-sdk-spec, built by
// addons) into the native filter expression of each backend. zvec and Milvus both
// use SQL-like expressions over typed columns but differ in details (equality is
// `=` in zvec vs `==` in Milvus), which is exactly why addons build a structured
// tree instead of writing backend syntax. Field names are validated to block
// expression injection; string literals are escaped.

use tentaflow_sdk_spec::{FieldValue, Filter};

use super::error::{Result, VectorError};

fn invalid(msg: impl Into<String>) -> VectorError {
    VectorError::InvalidFilter(msg.into())
}

/// Field names must be plain identifiers — blocks injecting operators/quotes
/// through a crafted field name.
fn validate_field(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()));
    if ok {
        Ok(())
    } else {
        Err(invalid(format!(
            "field name '{name}' must match ^[A-Za-z_][A-Za-z0-9_]{{0,63}}$"
        )))
    }
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn render_value(v: &FieldValue) -> Result<String> {
    Ok(match v {
        FieldValue::Str(s) => format!("'{}'", escape_str(s)),
        FieldValue::Int(i) => i.to_string(),
        FieldValue::Float(f) => {
            if !f.is_finite() {
                return Err(invalid("float value must be finite"));
            }
            format!("{f}")
        }
        FieldValue::Bool(b) => b.to_string(),
    })
}

fn render(f: &Filter, eq: &str, ne: &str) -> Result<String> {
    let cmp = |name: &str, op: &str, v: &FieldValue| -> Result<String> {
        validate_field(name)?;
        Ok(format!("{name} {op} {}", render_value(v)?))
    };
    Ok(match f {
        Filter::Eq(name, v) => cmp(name, eq, v)?,
        Filter::Ne(name, v) => cmp(name, ne, v)?,
        Filter::Gt(name, v) => cmp(name, ">", v)?,
        Filter::Gte(name, v) => cmp(name, ">=", v)?,
        Filter::Lt(name, v) => cmp(name, "<", v)?,
        Filter::Lte(name, v) => cmp(name, "<=", v)?,
        Filter::In(name, vs) => {
            validate_field(name)?;
            if vs.is_empty() {
                return Err(invalid(format!("IN list for '{name}' is empty")));
            }
            let items: Result<Vec<String>> = vs.iter().map(render_value).collect();
            format!("{name} in [{}]", items?.join(", "))
        }
        Filter::And(fs) => join(fs, "and", eq, ne)?,
        Filter::Or(fs) => join(fs, "or", eq, ne)?,
        Filter::Not(inner) => format!("not ({})", render(inner, eq, ne)?),
    })
}

fn join(fs: &[Filter], op: &str, eq: &str, ne: &str) -> Result<String> {
    if fs.is_empty() {
        return Err(invalid(format!("empty '{op}' group")));
    }
    let parts: Result<Vec<String>> = fs
        .iter()
        .map(|f| Ok(format!("({})", render(f, eq, ne)?)))
        .collect();
    Ok(parts?.join(&format!(" {op} ")))
}

/// zvec filter syntax (equality `=`).
pub fn to_zvec(filter: &Filter) -> Result<String> {
    render(filter, "=", "!=")
}

/// Milvus filter expression (equality `==`).
pub fn to_milvus(filter: &Filter) -> Result<String> {
    render(filter, "==", "!=")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> FieldValue {
        FieldValue::Str(v.to_string())
    }

    #[test]
    fn eq_differs_per_backend() {
        let f = Filter::Eq("source".into(), s("inbox"));
        assert_eq!(to_zvec(&f).unwrap(), "source = 'inbox'");
        assert_eq!(to_milvus(&f).unwrap(), "source == 'inbox'");
    }

    #[test]
    fn numeric_and_bool() {
        assert_eq!(
            to_milvus(&Filter::Gte("age".into(), FieldValue::Int(18))).unwrap(),
            "age >= 18"
        );
        assert_eq!(
            to_zvec(&Filter::Lt("score".into(), FieldValue::Float(0.5))).unwrap(),
            "score < 0.5"
        );
        assert_eq!(
            to_milvus(&Filter::Eq("flag".into(), FieldValue::Bool(true))).unwrap(),
            "flag == true"
        );
    }

    #[test]
    fn in_list() {
        let f = Filter::In("kind".into(), vec![s("a"), s("b")]);
        assert_eq!(to_zvec(&f).unwrap(), "kind in ['a', 'b']");
    }

    #[test]
    fn and_or_not_nested() {
        let f = Filter::And(vec![
            Filter::Eq("source".into(), s("web")),
            Filter::Or(vec![
                Filter::Gt("score".into(), FieldValue::Float(0.9)),
                Filter::Not(Box::new(Filter::Eq("lang".into(), s("pl")))),
            ]),
        ]);
        assert_eq!(
            to_milvus(&f).unwrap(),
            "(source == 'web') and ((score > 0.9) or (not (lang == 'pl')))"
        );
    }

    #[test]
    fn string_escaping_blocks_injection() {
        let f = Filter::Eq("name".into(), s("o'brien"));
        assert_eq!(to_zvec(&f).unwrap(), "name = 'o\\'brien'");
    }

    #[test]
    fn bad_field_name_rejected() {
        let f = Filter::Eq("name = 'x' or 1".into(), s("y"));
        assert!(matches!(to_zvec(&f), Err(VectorError::InvalidFilter(_))));
    }

    #[test]
    fn empty_groups_rejected() {
        assert!(to_zvec(&Filter::And(vec![])).is_err());
        assert!(to_zvec(&Filter::In("k".into(), vec![])).is_err());
    }
}
