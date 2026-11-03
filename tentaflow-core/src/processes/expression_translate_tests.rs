// ===== File: processes/expression_translate_tests.rs — golden, differential, refusal and hostile-input tests for the JUEL/FEEL to CEL translator =====

use super::expression_translate::*;
use crate::flow_engine::envelope::FlowValue;
use crate::flow_engine::expr::{self, ExprScope};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const C: &str = EXPR_TYPE_COERCION;
const D: &str = EXPR_DIVISION_SEMANTICS;
const E: &str = EXPR_EMPTY_SEMANTICS;

fn juel(source: &str) -> Result<Translation, TranslationRefusal> {
    translate(source, SourceLanguage::Juel)
}

fn codes(translation: &Translation) -> Vec<&'static str> {
    translation.warnings.iter().map(|w| w.code).collect()
}

fn refusal(source: &str, language: SourceLanguage) -> TranslationRefusal {
    match translate(source, language) {
        Ok(t) => panic!(
            "`{source}` was translated to `{}` but must be refused",
            t.cel
        ),
        Err(refusal) => refusal,
    }
}

// ---- (a) golden table ----

#[test]
fn juel_golden_table() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("${true}", "true", &[]),
        ("${false}", "false", &[]),
        ("${null}", "null", &[]),
        ("${42}", "42", &[]),
        ("${-5}", "-5", &[]),
        ("${1.5}", "1.5", &[]),
        ("${'abc'}", "\"abc\"", &[]),
        ("${\"abc\"}", "\"abc\"", &[]),
        ("#{x}", "vars.x", &[]),
        ("${x}", "vars.x", &[]),
        ("  ${x}  ", "vars.x", &[]),
        ("${ x }", "vars.x", &[]),
        ("${a.b}", "vars.a.b", &[]),
        ("${a['b']}", "vars.a.b", &[]),
        ("${a['b c']}", "vars.a[\"b c\"]", &[]),
        ("${a[0]}", "vars.a[0]", &[]),
        ("${a[i]}", "vars.a[vars.i]", &[]),
        ("${if}", "vars[\"if\"]", &[]),
        ("${in}", "vars[\"in\"]", &[]),
        ("${a.in}", "vars.a[\"in\"]", &[]),
        ("${é}", "vars[\"é\"]", &[]),
        ("${日本語.名前}", "vars[\"日本語\"][\"名前\"]", &[]),
        ("${execution.getVariable('x')}", "vars.x", &[]),
        ("${execution.getVariable('x').y}", "vars.x.y", &[]),
        (
            "${execution.getVariable('my var')}",
            "vars[\"my var\"]",
            &[],
        ),
        ("${variables.x}", "vars.x", &[]),
        ("${execution.variables.x}", "vars.x", &[]),
        ("${variables['x y']}", "vars[\"x y\"]", &[]),
        ("${execution.variables['x']}", "vars.x", &[]),
        ("${1 == 1}", "1 == 1", &[]),
        ("${true == false}", "true == false", &[]),
        ("${'a' == 'a'}", "\"a\" == \"a\"", &[]),
        ("${x == 1}", "vars.x == 1", &[C]),
        ("${x == null}", "vars.x == null", &[]),
        ("${null != x}", "null != vars.x", &[]),
        ("${x eq 1}", "vars.x == 1", &[C]),
        ("${x ne 1}", "vars.x != 1", &[C]),
        ("${1 < 2}", "1 < 2", &[]),
        ("${1 > 2}", "1 > 2", &[]),
        ("${x lt 2}", "vars.x < 2", &[C]),
        ("${x gt 2}", "vars.x > 2", &[C]),
        ("${x le 2}", "vars.x <= 2", &[C]),
        ("${x ge 2}", "vars.x >= 2", &[C]),
        ("${1 >= 1}", "1 >= 1", &[]),
        ("${1 <= 1}", "1 <= 1", &[]),
        ("${1 == 'a'}", "1 == \"a\"", &[C]),
        ("${1 == 1.0}", "1 == 1.0", &[C]),
        ("${true < false}", "true < false", &[C]),
        ("${a == b}", "vars.a == vars.b", &[C]),
        ("${x.y.z == 'q'}", "vars.x.y.z == \"q\"", &[C]),
        (
            "${x == 'a' || x == 'b'}",
            "(vars.x == \"a\") || (vars.x == \"b\")",
            &[C],
        ),
        ("${a && b}", "vars.a && vars.b", &[]),
        ("${a and b}", "vars.a && vars.b", &[]),
        ("${a || b}", "vars.a || vars.b", &[]),
        ("${a or b}", "vars.a || vars.b", &[]),
        ("${!a}", "!vars.a", &[]),
        ("${not a}", "!vars.a", &[]),
        ("${!true}", "!true", &[]),
        ("${!(a && b)}", "!(vars.a && vars.b)", &[]),
        ("${a || b && c}", "vars.a || (vars.b && vars.c)", &[]),
        ("${(a || b) && c}", "(vars.a || vars.b) && vars.c", &[]),
        ("${a && b || c}", "(vars.a && vars.b) || vars.c", &[]),
        ("${1 > 2 || 3 >= 3}", "(1 > 2) || (3 >= 3)", &[]),
        ("${!1}", "!1", &[C]),
        ("${!!a}", "!(!vars.a)", &[]),
        ("${1 + 2 * 3}", "1 + (2 * 3)", &[]),
        ("${(1 + 2) * 3}", "(1 + 2) * 3", &[]),
        ("${1 - 2 - 3}", "(1 - 2) - 3", &[]),
        ("${1 - (2 - 3)}", "1 - (2 - 3)", &[]),
        ("${8 mod 3}", "8 % 3", &[]),
        ("${8 % 3}", "8 % 3", &[]),
        ("${2 * 3 + 4 * 5}", "(2 * 3) + (4 * 5)", &[]),
        ("${1.5 * 2.0}", "1.5 * 2.0", &[]),
        ("${1.0 % 2.0}", "1.0 % 2.0", &[C]),
        ("${x + 1}", "vars.x + 1", &[C]),
        ("${x * 2 + 1}", "(vars.x * 2) + 1", &[C, C]),
        ("${'a' + 'b'}", "\"a\" + \"b\"", &[C]),
        ("${1 + 'b'}", "1 + \"b\"", &[C]),
        ("${7 / 2}", "7 / 2", &[D]),
        ("${7 div 2}", "7 / 2", &[D]),
        ("${7.0 / 2.0}", "7.0 / 2.0", &[]),
        ("${7.0 / 2}", "7.0 / 2", &[D]),
        ("${x / 2}", "vars.x / 2", &[D, C]),
        ("${-x}", "(-vars.x)", &[C]),
        ("${- -x}", "(-(-vars.x))", &[C]),
        ("${-(1 + 2)}", "(-(1 + 2))", &[]),
        ("${a ? 1 : 2}", "vars.a ? 1 : 2", &[]),
        (
            "${a ? b : c ? d : e}",
            "vars.a ? vars.b : (vars.c ? vars.d : vars.e)",
            &[],
        ),
        (
            "${(a ? b : c) ? d : e}",
            "(vars.a ? vars.b : vars.c) ? vars.d : vars.e",
            &[],
        ),
        (
            "${x > 1 ? 'big' : 'small'}",
            "(vars.x > 1) ? \"big\" : \"small\"",
            &[C],
        ),
        ("${1 ? 2 : 3}", "1 ? 2 : 3", &[C]),
        ("${empty x}", "(size(vars.x) == 0)", &[E]),
        ("${!empty x}", "!(size(vars.x) == 0)", &[E]),
        ("${not empty x}", "!(size(vars.x) == 0)", &[E]),
        ("${empty x && y}", "(size(vars.x) == 0) && vars.y", &[E]),
        ("${empty a.b}", "(size(vars.a.b) == 0)", &[E]),
        ("${\"a\\\"b\"}", "\"a\\\"b\"", &[]),
        ("${'it\\'s'}", "\"it's\"", &[]),
        ("${'a\\\\b'}", "\"a\\\\b\"", &[]),
        ("${'a\nb'}", "\"a\\nb\"", &[]),
        ("${'😀 é'}", "\"😀 é\"", &[]),
    ];
    assert!(cases.len() >= 60);
    for (source, expected, expected_codes) in cases {
        let translation = juel(source).unwrap_or_else(|r| panic!("`{source}` refused: {r}"));
        assert_eq!(&translation.cel, expected, "cel for `{source}`");
        assert_eq!(
            &codes(&translation),
            expected_codes,
            "warnings for `{source}`"
        );
    }
}

#[test]
fn feel_golden_table() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("= x", "vars.x", &[]),
        ("=x", "vars.x", &[]),
        ("= x = 1", "vars.x == 1", &[C]),
        ("= x != 1", "vars.x != 1", &[C]),
        ("= x < 5", "vars.x < 5", &[C]),
        ("= x >= 5", "vars.x >= 5", &[C]),
        ("= 1 < 5", "1 < 5", &[]),
        ("= a and b", "vars.a && vars.b", &[]),
        ("= a or b and c", "vars.a || (vars.b && vars.c)", &[]),
        ("= not(a)", "!vars.a", &[]),
        ("= a.b.c", "vars.a.b.c", &[]),
        ("= \"abc\" = \"abc\"", "\"abc\" == \"abc\"", &[]),
        ("= if a then 1 else 2", "vars.a ? 1 : 2", &[]),
        (
            "= if a then b else if c then d else e",
            "vars.a ? vars.b : (vars.c ? vars.d : vars.e)",
            &[],
        ),
        ("= x in [1..5]", "vars.x >= 1 && vars.x <= 5", &[C]),
        ("= x in (1..5]", "vars.x > 1 && vars.x <= 5", &[C]),
        ("= x in [1..5)", "vars.x >= 1 && vars.x < 5", &[C]),
        ("= x in (1..5)", "vars.x > 1 && vars.x < 5", &[C]),
        ("= 3 in [1..5]", "3 >= 1 && 3 <= 5", &[]),
        (
            "= x in [a..b]",
            "vars.x >= vars.a && vars.x <= vars.b",
            &[C],
        ),
        ("= x in [1,2,3]", "vars.x in [1, 2, 3]", &[C]),
        ("= 2 in [1,2,3]", "2 in [1, 2, 3]", &[]),
        ("= x = null", "vars.x == null", &[]),
        ("= x > -1", "vars.x > -1", &[C]),
        ("= null", "null", &[]),
        ("= true", "true", &[]),
        ("= \"a\\\"b\"", "\"a\\\"b\"", &[]),
        ("= (a or b) and c", "(vars.a || vars.b) && vars.c", &[]),
    ];
    for (source, expected, expected_codes) in cases {
        let translation = translate(source, SourceLanguage::Feel)
            .unwrap_or_else(|r| panic!("`{source}` refused: {r}"));
        assert_eq!(&translation.cel, expected, "cel for `{source}`");
        assert_eq!(
            &codes(&translation),
            expected_codes,
            "warnings for `{source}`"
        );
        let auto = translate(source, SourceLanguage::Auto).expect("auto detects a leading =");
        assert_eq!(auto, translation, "auto detection for `{source}`");
    }
    assert_eq!(
        translate("x = 1", SourceLanguage::Feel).unwrap().cel,
        "vars.x == 1"
    );
}

#[test]
fn auto_detection_picks_juel_for_wrappers() {
    for source in ["${a && b}", "#{a && b}"] {
        assert_eq!(
            translate(source, SourceLanguage::Auto).unwrap(),
            juel(source).unwrap()
        );
    }
}

#[test]
fn warnings_carry_stable_codes_and_explanations() {
    let t = juel("${x / 2 == 3 && empty y}").unwrap();
    assert_eq!(codes(&t), vec![D, C, C, E]);
    assert!(t.warnings[0].message.contains("floating-point"));
    assert!(t.warnings[3].message.contains("null"));
}

// ---- (b) differential test against a reference JUEL interpreter ----

#[derive(Debug, Clone, PartialEq)]
enum V {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<V>),
    Map(BTreeMap<String, V>),
}

impl V {
    fn json(&self) -> Value {
        match self {
            V::Null => Value::Null,
            V::Bool(b) => json!(b),
            V::Int(n) => json!(n),
            V::Str(s) => json!(s),
            V::List(items) => Value::Array(items.iter().map(V::json).collect()),
            V::Map(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), v.json())).collect()),
        }
    }
}

/// JUEL semantics for the coercion-free subset; `Err` models a JUEL runtime
/// error (the CEL side must fail too).
fn reference(expr: &Expr, vars: &BTreeMap<String, V>) -> Result<V, ()> {
    use crate::processes::expression_translate::BinOp::*;
    Ok(match expr {
        Expr::Int(n) => V::Int(*n),
        Expr::Str(s) => V::Str(s.clone()),
        Expr::Bool(b) => V::Bool(*b),
        Expr::Null => V::Null,
        Expr::Float(_) => return Err(()),
        Expr::Var(name) => vars.get(name).cloned().unwrap_or(V::Null),
        Expr::Member(base, name) => match reference(base, vars)? {
            V::Map(map) => map.get(name).cloned().ok_or(())?,
            _ => return Err(()),
        },
        Expr::Index(base, key) => match (reference(base, vars)?, reference(key, vars)?) {
            (V::List(items), V::Int(i)) => usize::try_from(i)
                .ok()
                .and_then(|i| items.get(i).cloned())
                .ok_or(())?,
            (V::Map(map), V::Str(k)) => map.get(&k).cloned().ok_or(())?,
            _ => return Err(()),
        },
        Expr::Not(inner) => match reference(inner, vars)? {
            V::Bool(b) => V::Bool(!b),
            _ => return Err(()),
        },
        Expr::Neg(inner) => match reference(inner, vars)? {
            V::Int(n) => V::Int(-n),
            _ => return Err(()),
        },
        Expr::Empty(inner) => V::Bool(match reference(inner, vars)? {
            V::Null => true,
            V::Str(s) => s.is_empty(),
            V::List(l) => l.is_empty(),
            V::Map(m) => m.is_empty(),
            _ => return Err(()),
        }),
        Expr::Ternary(c, t, e) => match reference(c, vars)? {
            V::Bool(true) => reference(t, vars)?,
            V::Bool(false) => reference(e, vars)?,
            _ => return Err(()),
        },
        Expr::Binary(op, l, r) => {
            let left = reference(l, vars)?;
            match (op, &left) {
                (And, V::Bool(false)) => return Ok(V::Bool(false)),
                (Or, V::Bool(true)) => return Ok(V::Bool(true)),
                _ => {}
            }
            let right = reference(r, vars)?;
            match (op, left, right) {
                (And | Or, V::Bool(_), V::Bool(b)) => V::Bool(b),
                (Eq, a, b) => V::Bool(a == b),
                (Ne, a, b) => V::Bool(a != b),
                (Lt | Gt | Le | Ge, V::Int(a), V::Int(b)) => V::Bool(ordering(op, a.cmp(&b))),
                (Lt | Gt | Le | Ge, V::Str(a), V::Str(b)) => V::Bool(ordering(op, a.cmp(&b))),
                (Add, V::Int(a), V::Int(b)) => V::Int(a + b),
                (Sub, V::Int(a), V::Int(b)) => V::Int(a - b),
                (Mul, V::Int(a), V::Int(b)) => V::Int(a * b),
                (Mod, V::Int(a), V::Int(b)) if b != 0 => V::Int(a % b),
                _ => return Err(()),
            }
        }
        Expr::Range { .. } | Expr::InList(..) => return Err(()),
    })
}

fn ordering(op: &BinOp, ord: std::cmp::Ordering) -> bool {
    use std::cmp::Ordering::*;
    match op {
        BinOp::Lt => ord == Less,
        BinOp::Gt => ord == Greater,
        BinOp::Le => ord != Greater,
        _ => ord != Less,
    }
}

fn grid() -> Vec<BTreeMap<String, V>> {
    let ints = [-3, 0, 2, 7];
    let strings = ["", "x", "abc"];
    let mut grid = Vec::new();
    for (i, a) in ints.iter().enumerate() {
        for (j, b) in [0, 3, 7].iter().enumerate() {
            for p in [false, true] {
                for (k, s) in strings.iter().enumerate() {
                    for t in ["x", "b"] {
                        let n = i + j + k + usize::from(p);
                        let list = if n % 2 == 0 {
                            vec![]
                        } else {
                            vec![V::Int(1), V::Int(2)]
                        };
                        let map = if n % 3 == 0 {
                            BTreeMap::from([("k".to_owned(), V::Int(1))])
                        } else {
                            BTreeMap::from([
                                ("k".to_owned(), V::Int(0)),
                                ("z".to_owned(), V::Int(5)),
                            ])
                        };
                        grid.push(BTreeMap::from([
                            ("a".to_owned(), V::Int(*a)),
                            ("b".to_owned(), V::Int(*b)),
                            ("p".to_owned(), V::Bool(p)),
                            ("q".to_owned(), V::Bool(n % 2 == 1)),
                            ("s".to_owned(), V::Str((*s).to_owned())),
                            ("t".to_owned(), V::Str(t.to_owned())),
                            ("l".to_owned(), V::List(list)),
                            ("m".to_owned(), V::Map(map)),
                        ]));
                    }
                }
            }
        }
    }
    grid
}

fn evaluate_cel(cel: &str, vars: &BTreeMap<String, V>) -> Result<Value, String> {
    let flow_vars: BTreeMap<String, FlowValue> = vars
        .iter()
        .map(|(k, v)| (k.clone(), FlowValue::Json(v.json())))
        .collect();
    let payload = FlowValue::Empty;
    let artifacts = HashMap::new();
    let meta = BTreeMap::new();
    let scope = ExprScope {
        vars: &flow_vars,
        payload: &payload,
        artifacts: &artifacts,
        meta: &meta,
        extras: &[],
    };
    expr::evaluate(cel, &scope, None).map_err(|e| e.cause)
}

#[test]
fn generated_cel_agrees_with_the_reference_interpreter() {
    let sources = [
        "${a == b}",
        "${a != b}",
        "${a < b}",
        "${a <= b}",
        "${a > b}",
        "${a >= b}",
        "${s == t}",
        "${s != 'x'}",
        "${s < t}",
        "${s >= 'abc'}",
        "${p == q}",
        "${p && q}",
        "${p || q}",
        "${!p}",
        "${not p}",
        "${p and q or !p}",
        "${p || q && !p}",
        "${(p || q) && !(p && q)}",
        "${!!p}",
        "${p ? a : b}",
        "${p ? (q ? 1 : 2) : 3}",
        "${a > b ? s : t}",
        "${a + b}",
        "${a - b - 1}",
        "${a * b + 2}",
        "${a + b * 2}",
        "${(a + b) * 2}",
        "${-a}",
        "${a - -b}",
        "${a % 3}",
        "${a mod 3}",
        "${a % b}",
        "${empty s}",
        "${!empty s}",
        "${empty l}",
        "${not empty l}",
        "${empty m}",
        "${empty s || empty l}",
        "${l[0]}",
        "${l[1] == 2}",
        "${m.k}",
        "${m['k'] + 1}",
        "${m.k == 1 && !empty m}",
        "${a == 2 || s == 'x'}",
        "${p && a > 0 ? 'pos' : 'other'}",
        "${a eq b}",
        "${execution.getVariable('a') + variables.b}",
        "${a == null}",
        "${s != null && p}",
        "${p || l[5] == 1}",
        "${!p && l[5] == 1}",
    ];
    let grid = grid();
    let mut compared = 0usize;
    let mut error_cases = 0usize;
    for source in sources {
        let translation = juel(source).unwrap_or_else(|r| panic!("`{source}` refused: {r}"));
        let parsed = parse(source, SourceLanguage::Juel).unwrap();
        for vars in &grid {
            let expected = reference(&parsed.expr, vars);
            let actual = evaluate_cel(&translation.cel, vars);
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => {
                    assert_eq!(
                        expected.json(),
                        actual,
                        "`{source}` -> `{}` with {vars:?}",
                        translation.cel
                    );
                    compared += 1;
                }
                (Err(()), Err(_)) => error_cases += 1,
                (expected, actual) => panic!(
                    "`{source}` -> `{}` with {vars:?}: reference {expected:?}, cel {actual:?}",
                    translation.cel
                ),
            }
        }
    }
    assert!(compared > 3000, "only {compared} comparisons ran");
    assert!(error_cases > 0, "no error-agreement cases were exercised");
}

#[test]
fn string_literals_round_trip_through_cel() {
    let samples = [
        "plain",
        "say \"hi\"",
        "back\\slash",
        "it's",
        "line\nbreak",
        "tab\there",
        "cr\rhere",
        "nul\u{1}ctl\u{7f}",
        "sep\u{2028}ator",
        "emoji 😀 and é",
        "",
        "\\n literal",
        "\"\\\"",
    ];
    let scope_vars = BTreeMap::new();
    for sample in samples {
        let escaped = sample.replace('\\', "\\\\").replace('\'', "\\'");
        let source = format!("${{'{escaped}'}}");
        let translation = juel(&source).unwrap_or_else(|r| panic!("{sample:?}: {r}"));
        assert_eq!(
            evaluate_cel(&translation.cel, &scope_vars).unwrap(),
            json!(sample),
            "{sample:?} -> {}",
            translation.cel
        );
    }
}

#[test]
fn reserved_and_unusual_identifiers_use_the_bracket_form() {
    let vars = BTreeMap::from([
        ("if".to_owned(), V::Int(4)),
        ("a b".to_owned(), V::Int(5)),
        ("é".to_owned(), V::Int(6)),
    ]);
    assert_eq!(
        evaluate_cel(&juel("${if}").unwrap().cel, &vars).unwrap(),
        json!(4)
    );
    assert_eq!(
        evaluate_cel(&juel("${variables['a b']}").unwrap().cel, &vars).unwrap(),
        json!(5)
    );
    assert_eq!(
        evaluate_cel(&juel("${é}").unwrap().cel, &vars).unwrap(),
        json!(6)
    );
    for word in [
        "true_", "null_", "in", "as", "var", "while", "function", "package", "let",
    ] {
        let cel = juel(&format!("${{{word}}}")).unwrap().cel;
        assert_eq!(
            cel,
            if word.ends_with('_') {
                format!("vars.{word}")
            } else {
                format!("vars[\"{word}\"]")
            }
        );
    }
}

// ---- (c) refusals ----

#[test]
fn unsupported_constructs_are_refused_with_name_and_position() {
    let cases: &[(&str, &str, usize)] = &[
        ("${bean.method()}", "bean.method", 7),
        ("${a.b.method(1)}", "a.b.method", 6),
        ("${execution.setVariable('a', 1)}", "setVariable", 12),
        ("${execution.getVariableLocal('a')}", "getVariableLocal", 12),
        ("${execution.getVariable(x)}", "not a string literal", 24),
        ("${execution.id}", "execution.id", 12),
        ("${execution}", "context", 2),
        ("${variables}", "context", 2),
        ("${execution.variables}", "context", 2),
        ("${variables[x]}", "computed name", 11),
        ("${execution['x']}", "indexed access", 11),
        ("${fn:length(x)}", "fn:length", 2),
        ("${foo(1)}", "foo(", 5),
        ("${x -> x}", "lambda", 4),
        ("${x = 1}", "assignment", 4),
        ("${a; b}", "sequence", 3),
        ("${[1, 2]}", "list literal", 2),
        ("${{1}}", "set or map literal", 2),
        ("${x instanceof y}", "instanceof", 4),
        ("${'a'.length}", "literal", 6),
        ("${5.x}", "literal", 4),
        ("${a.and}", "reserved word", 4),
        ("${x.y(1)}", "x.y", 4),
    ];
    for (source, needle, position) in cases {
        let r = refusal(source, SourceLanguage::Juel);
        assert_eq!(r.code, EXPR_UNSUPPORTED_CONSTRUCT, "{source}: {r}");
        assert!(r.message.contains(needle), "{source}: {r}");
        assert_eq!(r.position, *position, "{source}: {r}");
    }
}

#[test]
fn feel_outside_the_subset_is_refused() {
    for (source, needle) in [
        ("= 1 + 2", "arithmetic"),
        ("= a - 1", "arithmetic"),
        ("= f(x)", "function call"),
        ("= x[1]", "filter or index"),
        ("= [1, 2]", "list literal"),
        ("= {a: 1}", "context literal"),
        ("= for x in y return x", "for"),
        ("= some x in y satisfies x", "some"),
        ("= x between 1 and 2", "between"),
        ("= a b", "multi-word"),
        ("= x in 5", "range or a list"),
        ("= x in (1, 2)", "parenthesised"),
        ("= function(x) x", "function"),
        ("= x instance of y", "instance"),
    ] {
        let r = refusal(source, SourceLanguage::Auto);
        assert_eq!(r.code, EXPR_UNSUPPORTED_CONSTRUCT, "{source}: {r}");
        assert!(r.message.contains(needle), "{source}: {r}");
    }
    assert_eq!(refusal("= x == 1", SourceLanguage::Auto).code, EXPR_SYNTAX);
    assert_eq!(
        refusal("= x in [1..2", SourceLanguage::Auto).code,
        EXPR_SYNTAX
    );
}

#[test]
fn composite_and_unrecognised_sources_are_refused() {
    for source in [
        "a ${x}",
        "${a} ${b}",
        "${a}text",
        "${${x}}",
        "${a}${b}",
        "#{a} and ${b}",
    ] {
        assert_eq!(
            refusal(source, SourceLanguage::Auto).code,
            EXPR_COMPOSITE_TEMPLATE,
            "{source}"
        );
        assert_eq!(
            refusal(source, SourceLanguage::Juel).code,
            EXPR_COMPOSITE_TEMPLATE,
            "{source}"
        );
    }
    assert_eq!(
        refusal("hello", SourceLanguage::Auto).code,
        EXPR_UNKNOWN_LANGUAGE
    );
    assert_eq!(
        refusal("$", SourceLanguage::Auto).code,
        EXPR_UNKNOWN_LANGUAGE
    );
    assert_eq!(
        refusal("hello", SourceLanguage::Juel).code,
        EXPR_UNSUPPORTED_CONSTRUCT
    );
    assert_eq!(
        refusal("=x", SourceLanguage::Juel).code,
        EXPR_UNSUPPORTED_CONSTRUCT
    );
}

#[test]
fn syntax_errors_name_their_position() {
    for (source, position) in [
        ("${1 +}", 5),
        ("${)}", 2),
        ("${a ? b}", 7),
        ("${a b}", 4),
        ("${a & b}", 4),
        ("${1abc}", 3),
    ] {
        let r = refusal(source, SourceLanguage::Juel);
        assert_eq!(r.code, EXPR_SYNTAX, "{source}: {r}");
        assert_eq!(r.position, position, "{source}: {r}");
    }
    assert_eq!(
        refusal("${99999999999999999999}", SourceLanguage::Juel).code,
        EXPR_INVALID_LITERAL
    );
    assert_eq!(
        refusal("${1e999}", SourceLanguage::Juel).code,
        EXPR_INVALID_LITERAL
    );
    assert_eq!(
        refusal("${'a\\nb'}", SourceLanguage::Juel).code,
        EXPR_SYNTAX
    );
}

// ---- (d) hostile input ----

#[test]
fn hostile_inputs_are_refused_without_panicking() {
    let long = format!("${{{}}}", "1+".repeat(300));
    assert_eq!(refusal(&long, SourceLanguage::Juel).code, EXPR_TOO_LONG);
    let exact = format!("${{'{}'}}", "a".repeat(512 - 5));
    assert!(juel(&exact).is_ok(), "512 characters are still accepted");
    let over = format!("${{'{}'}}", "a".repeat(512 - 4));
    assert_eq!(refusal(&over, SourceLanguage::Juel).code, EXPR_TOO_LONG);
    let non_ascii = format!("${{'{}'}}", "é".repeat(520));
    assert_eq!(
        refusal(&non_ascii, SourceLanguage::Juel).code,
        EXPR_TOO_LONG,
        "the cap counts characters"
    );

    let parens = "(".repeat(5000);
    assert_eq!(
        refusal(&format!("${{{parens}x}}"), SourceLanguage::Juel).code,
        EXPR_TOO_LONG
    );
    assert_eq!(refusal(&parens, SourceLanguage::Auto).code, EXPR_TOO_LONG);

    let deep = |n: usize| format!("${{{}x{}}}", "(".repeat(n), ")".repeat(n));
    assert_eq!(juel(&deep(63)).unwrap().cel, "vars.x");
    assert_eq!(refusal(&deep(64), SourceLanguage::Juel).code, EXPR_TOO_DEEP);
    assert_eq!(
        refusal(&deep(200), SourceLanguage::Juel).code,
        EXPR_TOO_DEEP
    );
    let bangs = |n: usize| format!("${{{}x}}", "!".repeat(n));
    assert_eq!(juel(&bangs(2)).unwrap().cel, "!(!vars.x)");
    assert_eq!(
        refusal(&bangs(63), SourceLanguage::Juel).code,
        EXPR_INVALID_RESULT
    );
    assert_eq!(
        refusal(&bangs(300), SourceLanguage::Juel).code,
        EXPR_TOO_DEEP
    );
    let feel_deep = format!("={}x{}", "(".repeat(100), ")".repeat(100));
    assert_eq!(
        refusal(&feel_deep, SourceLanguage::Auto).code,
        EXPR_TOO_DEEP
    );

    // Within the parser bound but beyond the CEL nesting cap: the final
    // validation refuses instead of handing the evaluator a deep program.
    let ternaries = format!("${{{}z}}", "a?b:".repeat(20));
    assert_eq!(
        refusal(&ternaries, SourceLanguage::Juel).code,
        EXPR_INVALID_RESULT
    );
    let chain = format!("${{{}1}}", "1+".repeat(40));
    assert_eq!(
        refusal(&chain, SourceLanguage::Juel).code,
        EXPR_INVALID_RESULT
    );
}

#[test]
fn malformed_input_never_panics() {
    for source in [
        "",
        " ",
        "$",
        "#",
        "${",
        "#{",
        "}",
        "${}",
        "#{}",
        "${ }",
        "${'abc}",
        "${\"abc}",
        "${x",
        "$ {x}",
        "${'a\\",
        "${'\\",
        "${${x}}",
        "${#{x}}",
        "${😀}",
        "= ",
        "=",
        "=\"",
        "= 'a'",
        "${é +}",
        "${x.}",
        "${x[}",
        "${x[1}",
        "${(}",
        "${a ? }",
        "${a ? b :}",
        "${.}",
        "${..}",
        "${1..2}",
        "= 1..2",
        "= x in [..]",
        "= x in [1..]",
        "= x in",
        "= if",
        "= if a then",
        "= if a then b else",
        "= not",
        "= not(",
        "${empty}",
        "${!}",
        "${-}",
        "${1e}",
        "${1e+}",
        "${0x10}",
        "${\u{0}}",
        "\u{feff}${x}",
        "${x}\u{0}",
    ] {
        for language in [
            SourceLanguage::Juel,
            SourceLanguage::Feel,
            SourceLanguage::Auto,
        ] {
            let _ = translate(source, language);
        }
    }
    assert_eq!(refusal("${}", SourceLanguage::Juel).code, EXPR_EMPTY);
    assert_eq!(refusal("${ }", SourceLanguage::Juel).code, EXPR_EMPTY);
    assert_eq!(refusal("   ", SourceLanguage::Auto).code, EXPR_EMPTY);
    assert_eq!(refusal("${'abc}", SourceLanguage::Juel).code, EXPR_SYNTAX);
    assert_eq!(refusal("${", SourceLanguage::Juel).code, EXPR_SYNTAX);
    assert_eq!(refusal("${x", SourceLanguage::Juel).code, EXPR_SYNTAX);
    assert_eq!(
        refusal("$", SourceLanguage::Juel).code,
        EXPR_UNSUPPORTED_CONSTRUCT
    );
    assert_eq!(refusal("${😀}", SourceLanguage::Juel).code, EXPR_SYNTAX);
}

#[test]
fn pseudo_random_token_soup_never_panics_and_valid_output_is_valid_cel() {
    let pieces = [
        "${",
        "}",
        "#{",
        "(",
        ")",
        "[",
        "]",
        "'",
        "\"",
        "\\",
        "x",
        "y1",
        "é",
        "1",
        "2.5",
        "1e3",
        " ",
        ".",
        ",",
        "?",
        ":",
        "&&",
        "||",
        "!",
        "==",
        "!=",
        "<",
        ">=",
        "+",
        "-",
        "*",
        "/",
        "%",
        "div",
        "mod",
        "and",
        "or",
        "not",
        "empty",
        "eq",
        "null",
        "true",
        "execution",
        "getVariable",
        "..",
        "in",
        "if",
        "then",
        "else",
        "=",
        "$",
        "😀",
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut translated = 0usize;
    for _ in 0..4000 {
        let len = 1 + (next() % 12) as usize;
        let body: String = (0..len)
            .map(|_| pieces[(next() % pieces.len() as u64) as usize])
            .collect();
        for source in [format!("${{{body}}}"), format!("={body}"), body] {
            if let Ok(t) = translate(&source, SourceLanguage::Auto) {
                expr::validate_syntax(&t.cel, None)
                    .unwrap_or_else(|e| panic!("`{source}` -> `{}`: {}", t.cel, e.cause));
                translated += 1;
            }
        }
    }
    assert!(
        translated > 20,
        "token soup produced only {translated} translations"
    );
}
