// ===== File: processes/expression_translate_tests.rs — golden, differential, refusal and hostile-input tests for the JUEL/FEEL to CEL translator =====

use super::expression_translate::*;
use crate::flow_engine::envelope::FlowValue;
use crate::flow_engine::expr::{self, ExprScope};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const C: &str = EXPR_TYPE_COERCION;
const D: &str = EXPR_DIVISION_SEMANTICS;
const E: &str = EXPR_EMPTY_SEMANTICS;
const M: &str = EXPR_MISSING_VALUE;

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

fn assert_golden(source: &str, language: SourceLanguage, expected: &str, expected_codes: &[&str]) {
    let translation =
        translate(source, language).unwrap_or_else(|r| panic!("`{source}` refused: {r}"));
    assert_eq!(translation.cel, expected, "cel for `{source}`");
    let mut actual = codes(&translation);
    let mut wanted = expected_codes.to_vec();
    actual.sort_unstable();
    wanted.sort_unstable();
    assert_eq!(
        actual, wanted,
        "warnings for `{source}`: {:?}",
        translation.warnings
    );
}

#[test]
fn juel_golden_table() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("${true}", "true", &[]),
        ("${false}", "false", &[]),
        ("${null}", "null", &[]),
        ("${42}", "42", &[]),
        ("${-5}", "-5", &[]),
        ("${1.5}", "1.5", &[]),
        ("${1e3}", "1000.0", &[]),
        ("${'abc'}", "\"abc\"", &[]),
        ("${\"abc\"}", "\"abc\"", &[]),
        ("#{x}", "vars.x", &[]),
        ("${x}", "vars.x", &[]),
        ("  ${x}  ", "vars.x", &[]),
        ("\u{1}${x}\u{1f}", "vars.x", &[]),
        ("${ x }", "vars.x", &[]),
        ("${a.b}", "vars.a.b", &[M]),
        ("${a['b']}", "vars.a.b", &[M]),
        ("${a['b c']}", "vars.a[\"b c\"]", &[M]),
        ("${a[0]}", "vars.a[0]", &[M]),
        ("${a[i]}", "vars.a[vars.i]", &[M]),
        ("${if}", "vars[\"if\"]", &[]),
        ("${in}", "vars[\"in\"]", &[]),
        ("${a.in}", "vars.a[\"in\"]", &[M]),
        ("${é}", "vars[\"é\"]", &[]),
        ("${日本語.名前}", "vars[\"日本語\"][\"名前\"]", &[M]),
        ("${execution.getVariable('x')}", "vars.x", &[M]),
        ("${execution.getVariable('x').y}", "vars.x.y", &[M]),
        (
            "${execution.getVariable('my var')}",
            "vars[\"my var\"]",
            &[M],
        ),
        ("${variables}", "vars.variables", &[]),
        ("${variables.x}", "vars.variables.x", &[M]),
        ("${variables['x y']}", "vars.variables[\"x y\"]", &[M]),
        ("${execution.variables.x}", "vars.x", &[M]),
        ("${execution.variables['x']}", "vars.x", &[M]),
        ("${1 == 1}", "1 == 1", &[]),
        ("${true == false}", "true == false", &[]),
        ("${'a' == 'a'}", "\"a\" == \"a\"", &[]),
        ("${x == 1}", "vars.x == 1", &[C]),
        ("${x == null}", "vars.x == null", &[]),
        ("${null != x}", "null != vars.x", &[]),
        ("${1 == null}", "1 == null", &[]),
        ("${'a' != null}", "\"a\" != null", &[]),
        ("${a.b == null}", "vars.a.b == null", &[M]),
        (
            "${execution.getVariable('x') == null}",
            "vars.x == null",
            &[M],
        ),
        ("${(a + 1) == null}", "(vars.a + 1) == null", &[C, C]),
        ("${x < null}", "vars.x < null", &[C]),
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
        ("${x.y.z == 'q'}", "vars.x.y.z == \"q\"", &[M, C]),
        (
            "${x == 'a' || y == 1}",
            "(vars.x == \"a\") || (vars.y == 1)",
            &[C, C],
        ),
        (
            "${x == 'a' || x == 'b'}",
            "(vars.x == \"a\") || (vars.x == \"b\")",
            &[C, C],
        ),
        (
            "${x == 1 && x == 1}",
            "(vars.x == 1) && (vars.x == 1)",
            &[C],
        ),
        ("${a && b}", "vars.a && vars.b", &[C, C]),
        ("${a and b}", "vars.a && vars.b", &[C, C]),
        ("${a || b}", "vars.a || vars.b", &[C, C]),
        ("${a or b}", "vars.a || vars.b", &[C, C]),
        ("${a && a}", "vars.a && vars.a", &[C]),
        ("${!a}", "!vars.a", &[C]),
        ("${not a}", "!vars.a", &[C]),
        ("${!true}", "!true", &[]),
        ("${!(a && b)}", "!(vars.a && vars.b)", &[C, C]),
        ("${!(1 < 2)}", "!(1 < 2)", &[]),
        ("${a || b && c}", "vars.a || (vars.b && vars.c)", &[C, C, C]),
        (
            "${(a || b) && c}",
            "(vars.a || vars.b) && vars.c",
            &[C, C, C],
        ),
        ("${a && b || c}", "(vars.a && vars.b) || vars.c", &[C, C, C]),
        ("${1 > 2 || 3 >= 3}", "(1 > 2) || (3 >= 3)", &[]),
        ("${true && false}", "true && false", &[]),
        ("${!1}", "!1", &[C]),
        ("${!!a}", "!(!vars.a)", &[C]),
        ("${(!a).b}", "(!vars.a).b", &[M, C]),
        ("${(!a)[0]}", "(!vars.a)[0]", &[M, C]),
        ("${(-x).y}", "(-vars.x).y", &[M, C]),
        ("${empty ''}", "(size(\"\") == 0)", &[]),
        ("${a || b || c}", "vars.a || vars.b || vars.c", &[C, C, C]),
        ("${a - b - c}", "vars.a - vars.b - vars.c", &[C, C]),
        ("${a - (b - c)}", "vars.a - (vars.b - vars.c)", &[C, C]),
        ("${1 + 2 + 3 * 4 * 5}", "1 + 2 + (3 * 4 * 5)", &[]),
        ("${a == b == c}", "(vars.a == vars.b) == vars.c", &[C, C]),
        ("${s < '\u{e000}'}", "vars.s < \"\u{e000}\"", &[C, C]),
        ("${s < 'abc'}", "vars.s < \"abc\"", &[C]),
        ("${1 + 2 * 3}", "1 + (2 * 3)", &[]),
        ("${(1 + 2) * 3}", "(1 + 2) * 3", &[]),
        ("${1 - 2 - 3}", "1 - 2 - 3", &[]),
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
        ("${- -x}", "(-(-vars.x))", &[C, C]),
        ("${-(1 + 2)}", "(-(1 + 2))", &[]),
        ("${a ? 1 : 2}", "vars.a ? 1 : 2", &[C]),
        (
            "${a ? b : c ? d : e}",
            "vars.a ? vars.b : (vars.c ? vars.d : vars.e)",
            &[C, C],
        ),
        (
            "${(a ? b : c) ? d : e}",
            "(vars.a ? vars.b : vars.c) ? vars.d : vars.e",
            &[C, C],
        ),
        (
            "${x > 1 ? 'big' : 'small'}",
            "(vars.x > 1) ? \"big\" : \"small\"",
            &[C],
        ),
        ("${true ? 1 : 2}", "true ? 1 : 2", &[]),
        ("${1 ? 2 : 3}", "1 ? 2 : 3", &[C]),
        ("${empty x}", "(size(vars.x) == 0)", &[E]),
        ("${!empty x}", "!(size(vars.x) == 0)", &[E]),
        ("${not empty x}", "!(size(vars.x) == 0)", &[E]),
        ("${empty x && y}", "(size(vars.x) == 0) && vars.y", &[E, C]),
        ("${empty a.b}", "(size(vars.a.b) == 0)", &[M, E]),
        ("${\"a\\\"b\"}", "\"a\\\"b\"", &[]),
        ("${'it\\'s'}", "\"it's\"", &[]),
        ("${'a\\\\b'}", "\"a\\\\b\"", &[]),
        ("${'a\nb'}", "\"a\\nb\"", &[]),
        ("${'😀 é'}", "\"😀 é\"", &[]),
    ];
    assert!(cases.len() >= 60);
    for (source, expected, expected_codes) in cases {
        assert_golden(source, SourceLanguage::Juel, expected, expected_codes);
    }
}

#[test]
fn feel_golden_table() {
    let cases: &[(&str, &str, &[&str])] = &[
        ("= x", "vars.x", &[M]),
        ("=x", "vars.x", &[M]),
        ("= x = 1", "vars.x == 1", &[M, C]),
        ("= x != 1", "vars.x != 1", &[M, C]),
        ("= x < 5", "vars.x < 5", &[M, C]),
        ("= x >= 5", "vars.x >= 5", &[M, C]),
        ("= 1 < 5", "1 < 5", &[]),
        ("= a and b", "vars.a && vars.b", &[M, M, C, C]),
        (
            "= a or b and c",
            "vars.a || (vars.b && vars.c)",
            &[M, M, M, C, C, C],
        ),
        ("= not(a)", "!vars.a", &[M, C]),
        ("= a.b.c", "vars.a.b.c", &[M]),
        ("= \"abc\" = \"abc\"", "\"abc\" == \"abc\"", &[]),
        ("= if a then 1 else 2", "vars.a ? 1 : 2", &[M, C]),
        (
            "= if a then b else if c then d else e",
            "vars.a ? vars.b : (vars.c ? vars.d : vars.e)",
            &[M, M, M, M, M, C, C],
        ),
        ("= x in [1..5]", "vars.x >= 1 && vars.x <= 5", &[M, C, C]),
        ("= x in (1..5]", "vars.x > 1 && vars.x <= 5", &[M, C, C]),
        ("= x in [1..5)", "vars.x >= 1 && vars.x < 5", &[M, C, C]),
        ("= x in (1..5)", "vars.x > 1 && vars.x < 5", &[M, C, C]),
        ("= 3 in [1..5]", "3 >= 1 && 3 <= 5", &[]),
        (
            "= x in [a..b]",
            "vars.x >= vars.a && vars.x <= vars.b",
            &[M, M, M, C, C],
        ),
        ("= x in [1,2,3]", "vars.x in [1, 2, 3]", &[M, C, C, C]),
        ("= 2 in [1,2,3]", "2 in [1, 2, 3]", &[]),
        ("= x = null", "vars.x == null", &[M]),
        ("= x > -1", "vars.x > -1", &[M, C]),
        ("= null", "null", &[]),
        ("= true", "true", &[]),
        ("= \"a\\\"b\"", "\"a\\\"b\"", &[]),
        (
            "= (a or b) and c",
            "(vars.a || vars.b) && vars.c",
            &[M, M, M, C, C, C],
        ),
    ];
    for (source, expected, expected_codes) in cases {
        assert_golden(source, SourceLanguage::Feel, expected, expected_codes);
        let auto = translate(source, SourceLanguage::Auto).expect("auto detects a leading =");
        assert_eq!(
            auto,
            translate(source, SourceLanguage::Feel).unwrap(),
            "auto detection for `{source}`"
        );
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
    let mut found = codes(&t);
    found.sort_unstable();
    assert_eq!(found, vec![D, E, C, C]);
    let message = |code: &str| {
        t.warnings
            .iter()
            .find(|w| w.code == code)
            .unwrap()
            .message
            .clone()
    };
    assert!(message(D).contains("floating-point"));
    assert!(message(E).contains("null or unset value as empty"));
    assert!(!message(E).contains("JUEL returns null"));
}

#[test]
fn warnings_name_the_operand_and_are_reported_per_operand() {
    let t = juel("${x == 'a' || y == 1}").unwrap();
    assert_eq!(codes(&t), vec![C, C]);
    assert!(
        t.warnings[0].message.contains("vars.x == \"a\""),
        "{:?}",
        t.warnings
    );
    assert!(
        t.warnings[1].message.contains("vars.y == 1"),
        "{:?}",
        t.warnings
    );
    let t = juel("${p && q}").unwrap();
    assert!(
        t.warnings[0].message.contains("`vars.p`") && t.warnings[1].message.contains("`vars.q`")
    );
    let t = juel("${a.b.c == null}").unwrap();
    assert_eq!(codes(&t), vec![M]);
    assert!(t.warnings[0].message.contains("vars.a.b.c"));
    assert!(t.warnings[0].message.contains("evaluation error in CEL"));
    let t = translate("= x = null", SourceLanguage::Auto).unwrap();
    assert!(t.warnings[0].message.contains("null in FEEL"));
}

// ---- fixes: context API, constant overflow, Java whitespace, FEEL numbers ----

#[test]
fn bare_variables_is_an_ordinary_identifier() {
    assert_golden(
        "${variables.x}",
        SourceLanguage::Juel,
        "vars.variables.x",
        &[M],
    );
    assert_golden("${variables}", SourceLanguage::Juel, "vars.variables", &[]);
    assert_golden(
        "${execution.variables.x}",
        SourceLanguage::Juel,
        "vars.x",
        &[M],
    );
    assert_eq!(
        refusal("${execution.variables}", SourceLanguage::Juel).code,
        EXPR_UNSUPPORTED_CONSTRUCT
    );
    assert_eq!(
        refusal("${execution.variables[x]}", SourceLanguage::Juel).position,
        21
    );
}

#[test]
fn constant_integer_overflow_is_refused() {
    for source in [
        "${9223372036854775807 + 1}",
        "${-9223372036854775807 - 2}",
        "${9223372036854775807 - -1}",
        "${4611686018427387904 * 2}",
        "${(9223372036854775807 + 0) + 1}",
        "${9223372036854775807 * 9223372036854775807}",
        "${3037000500 * 3037000500}",
        "${-(-9223372036854775807 - 1)}",
    ] {
        let r = refusal(source, SourceLanguage::Juel);
        assert_eq!(r.code, EXPR_INTEGER_OVERFLOW, "{source}: {r}");
    }
    for source in [
        "${9223372036854775806 + 1}",
        "${-9223372036854775807 - 1}",
        "${3037000499 * 3037000499}",
        "${x + 9223372036854775807 + 1}",
        "${9223372036854775807 + 1.0}",
        "${-(-9223372036854775807)}",
        "${- -9223372036854775807}",
        "${execution.getVariable('task')}",
    ] {
        assert!(juel(source).is_ok(), "{source}");
    }
    assert_eq!(
        refusal("${1 + 9223372036854775807}", SourceLanguage::Juel).position,
        4
    );
}

#[test]
fn java_trim_and_whitespace_rules_apply() {
    assert_eq!(
        refusal("\u{a0}${x}", SourceLanguage::Auto).code,
        EXPR_COMPOSITE_TEMPLATE
    );
    assert_eq!(
        refusal("\u{a0}${x}", SourceLanguage::Juel).code,
        EXPR_COMPOSITE_TEMPLATE
    );
    assert_eq!(
        refusal("${x}\u{a0}", SourceLanguage::Juel).code,
        EXPR_COMPOSITE_TEMPLATE
    );
    assert_eq!(
        refusal("${a\u{a0}+ b}", SourceLanguage::Juel).code,
        EXPR_SYNTAX
    );
    assert_eq!(
        refusal("\u{a0}", SourceLanguage::Auto).code,
        EXPR_UNKNOWN_LANGUAGE
    );
    assert_eq!(juel("${a\u{2003}+\u{b}b}").unwrap().cel, "vars.a + vars.b");
    assert_eq!(juel("\u{0}\t\n ${x} \r").unwrap().cel, "vars.x");
}

#[test]
fn long_left_associative_chains_stay_within_the_cel_nesting_cap() {
    let names: Vec<String> = (0..34).map(|i| format!("v{i}")).collect();
    let source = format!("${{{}}}", names.join("||"));
    let translation = juel(&source).unwrap();
    assert!(!translation.cel.contains('('), "{}", translation.cel);
    for hot in [None, Some(0), Some(17), Some(33)] {
        let vars: BTreeMap<String, V> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), V::Bool(hot == Some(i))))
            .collect();
        assert_eq!(
            evaluate_cel(&translation.cel, &vars).unwrap(),
            json!(hot.is_some())
        );
    }
    let sum = format!("${{{}}}", ["1"; 60].join("+"));
    assert_eq!(
        evaluate_cel(&juel(&sum).unwrap().cel, &BTreeMap::new()).unwrap(),
        json!(60)
    );
    let difference = format!("${{100{}}}", "-1".repeat(60));
    assert_eq!(
        evaluate_cel(&juel(&difference).unwrap().cel, &BTreeMap::new()).unwrap(),
        json!(40)
    );
    // Only the left operand loses its parentheses; a right operand keeps them.
    assert_eq!(juel("${1 - (2 - 3)}").unwrap().cel, "1 - (2 - 3)");
    assert_eq!(
        juel("${a || (b || c)}").unwrap().cel,
        "vars.a || (vars.b || vars.c)"
    );
    assert_eq!(
        juel("${a + b - c}").unwrap().cel,
        "(vars.a + vars.b) - vars.c"
    );
}

#[test]
fn warning_texts_follow_the_language_and_the_operand() {
    let has = |t: &Translation, needle: &str| t.warnings.iter().any(|w| w.message.contains(needle));
    let feel_if = translate("= if a then 1 else 2", SourceLanguage::Feel).unwrap();
    assert!(
        has(&feel_if, "takes the else branch"),
        "{:?}",
        feel_if.warnings
    );
    let feel_and = translate("= a and b", SourceLanguage::Feel).unwrap();
    assert!(has(&feel_and, "treats a non-boolean operand as null"));
    assert!(!has(&feel_and, "else branch"));
    assert!(has(&juel("${s < '\u{e000}'}").unwrap(), "UTF-16"));
    assert!(!has(&juel("${s < 'abc'}").unwrap(), "UTF-16"));
    assert!(juel("${empty ''}").unwrap().warnings.is_empty());
    assert!(juel("${x == null}").unwrap().warnings.is_empty());
    assert_eq!(
        codes(&translate("= x = null", SourceLanguage::Feel).unwrap()),
        vec![M]
    );
}

#[test]
fn feel_numbers_have_no_exponent_but_juel_numbers_do() {
    assert_eq!(refusal("= 1e3", SourceLanguage::Feel).code, EXPR_SYNTAX);
    assert_eq!(
        refusal("= x > 2E-2", SourceLanguage::Feel).code,
        EXPR_SYNTAX
    );
    assert_eq!(translate("= 1.5", SourceLanguage::Feel).unwrap().cel, "1.5");
    assert_eq!(juel("${2E-2}").unwrap().cel, "0.02");
    assert_eq!(juel("${1e3}").unwrap().cel, "1000.0");
}

// ---- (b) differential tests against independent JUEL models ----

#[derive(Debug, Clone, PartialEq)]
enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
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
            V::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            V::Str(s) => json!(s),
            V::List(items) => Value::Array(items.iter().map(V::json).collect()),
            V::Map(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), v.json())).collect()),
        }
    }
}

/// Numbers compare by value (JUEL `7.0` and CEL `7` are the same number here);
/// a non-finite JUEL double has no CEL counterpart.
fn same(expected: &V, actual: &Value) -> bool {
    match (expected, actual) {
        (V::Null, Value::Null) => true,
        (V::Bool(a), Value::Bool(b)) => a == b,
        (V::Int(a), Value::Number(b)) => b.as_f64() == Some(*a as f64),
        (V::Float(a), Value::Number(b)) => a.is_finite() && b.as_f64() == Some(*a),
        (V::Str(a), Value::String(b)) => a == b,
        (V::List(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| same(x, y))
        }
        (V::Map(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        _ => false,
    }
}

type Outcome = Result<V, ()>;

/// Why a JUEL result may legitimately differ from the CEL result. Every
/// reason names the warning code the translation owes the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Why {
    /// Unset context variable, missing key, out-of-range index or null base/key.
    MissingValue,
    /// Null operand coerced to false or 0.
    NullCoercion,
    /// Operands of different runtime kinds, numeric strings, float modulo.
    MixedKinds,
    /// Null or text used as a boolean.
    BoolCoercion,
    /// `/` where the operands are not both floats.
    IntegerDivision,
    /// `empty` on null or on a number or boolean.
    EmptyOnScalar,
    /// Strings whose UTF-16 and code point orders disagree.
    StringOrder,
}

impl Why {
    fn code(self) -> &'static str {
        match self {
            Why::MissingValue => M,
            Why::NullCoercion | Why::MixedKinds | Why::BoolCoercion | Why::StringOrder => C,
            Why::IntegerDivision => D,
            Why::EmptyOnScalar => E,
        }
    }

    fn message_hint(self) -> &'static str {
        match self {
            Why::MissingValue => "evaluation error in CEL",
            Why::NullCoercion | Why::MixedKinds => "operand types",
            Why::BoolCoercion => "boolean",
            Why::IntegerDivision => "floating-point",
            Why::EmptyOnScalar => "null or unset value as empty",
            Why::StringOrder => "UTF-16",
        }
    }
}

type Reasons = BTreeSet<Why>;

#[derive(Clone, Copy)]
enum Num {
    I(i64),
    F(f64),
}

impl Num {
    fn f(self) -> f64 {
        match self {
            Num::I(i) => i as f64,
            Num::F(f) => f,
        }
    }
}

fn same_kind(a: &V, b: &V) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

/// EL 3.0 section 1.8 coercions, restricted to the value kinds the grid uses.
fn to_bool(v: &V, r: &mut Reasons) -> Result<bool, ()> {
    match v {
        V::Bool(b) => Ok(*b),
        V::Null => {
            r.insert(Why::BoolCoercion);
            Ok(false)
        }
        V::Str(s) => {
            r.insert(Why::BoolCoercion);
            Ok(s.eq_ignore_ascii_case("true"))
        }
        _ => Err(()),
    }
}

fn to_num(v: &V, r: &mut Reasons) -> Result<Num, ()> {
    match v {
        V::Int(i) => Ok(Num::I(*i)),
        V::Float(f) => Ok(Num::F(*f)),
        V::Null => {
            r.insert(Why::NullCoercion);
            Ok(Num::I(0))
        }
        V::Str(s) => {
            r.insert(Why::MixedKinds);
            if s.is_empty() {
                return Ok(Num::I(0));
            }
            s.parse::<i64>()
                .map(Num::I)
                .or_else(|_| s.parse::<f64>().map(Num::F))
                .map_err(|_| ())
        }
        _ => Err(()),
    }
}

fn el_eq(a: &V, b: &V, r: &mut Reasons) -> Result<bool, ()> {
    if a == b {
        return Ok(true);
    }
    if *a == V::Null || *b == V::Null || same_kind(a, b) {
        return Ok(false);
    }
    r.insert(Why::MixedKinds);
    let (float, int) = (
        matches!(a, V::Float(_)) || matches!(b, V::Float(_)),
        matches!(a, V::Int(_)) || matches!(b, V::Int(_)),
    );
    if float {
        return Ok(to_num(a, r)?.f() == to_num(b, r)?.f());
    }
    if int {
        return match (to_num(a, r)?, to_num(b, r)?) {
            (Num::I(x), Num::I(y)) => Ok(x == y),
            _ => Err(()),
        };
    }
    if matches!(a, V::Bool(_)) || matches!(b, V::Bool(_)) {
        // The mismatch itself is the reason; the text-to-boolean step is part of it.
        let mut ignored = Reasons::new();
        return Ok(to_bool(a, &mut ignored)? == to_bool(b, &mut ignored)?);
    }
    Err(())
}

fn el_cmp(a: &V, b: &V, r: &mut Reasons) -> Result<Option<std::cmp::Ordering>, ()> {
    use std::cmp::Ordering::Equal;
    if *a == V::Null && *b == V::Null {
        r.insert(Why::NullCoercion);
        return Ok(Some(Equal));
    }
    if *a == V::Null || *b == V::Null {
        r.insert(Why::NullCoercion);
        return Ok(None);
    }
    if !same_kind(a, b) {
        r.insert(Why::MixedKinds);
        if matches!(a, V::Float(_)) || matches!(b, V::Float(_)) {
            return Ok(to_num(a, r)?.f().partial_cmp(&to_num(b, r)?.f()));
        }
        return match (to_num(a, r)?, to_num(b, r)?) {
            (Num::I(x), Num::I(y)) => Ok(Some(x.cmp(&y))),
            _ => Err(()),
        };
    }
    match (a, b) {
        (V::Int(x), V::Int(y)) => Ok(Some(x.cmp(y))),
        (V::Float(x), V::Float(y)) => Ok(x.partial_cmp(y)),
        (V::Str(x), V::Str(y)) => {
            let utf16 = x.encode_utf16().cmp(y.encode_utf16());
            if utf16 != x.cmp(y) {
                r.insert(Why::StringOrder);
            }
            Ok(Some(utf16))
        }
        _ => Err(()),
    }
}

fn el_arith(op: &BinOp, a: &V, b: &V, r: &mut Reasons) -> Outcome {
    if *a == V::Null && *b == V::Null {
        r.insert(Why::NullCoercion);
        return Ok(V::Int(0));
    }
    if *op == BinOp::Div {
        if !(matches!(a, V::Float(_)) && matches!(b, V::Float(_))) {
            r.insert(Why::IntegerDivision);
        }
        return Ok(V::Float(to_num(a, r)?.f() / to_num(b, r)?.f()));
    }
    let (x, y) = (to_num(a, r)?, to_num(b, r)?);
    match (x, y) {
        (Num::I(x), Num::I(y)) => Ok(V::Int(match op {
            BinOp::Add => x.wrapping_add(y),
            BinOp::Sub => x.wrapping_sub(y),
            BinOp::Mul => x.wrapping_mul(y),
            _ if y == 0 => return Err(()),
            _ => x.wrapping_rem(y),
        })),
        (x, y) => {
            if !(matches!(a, V::Float(_)) && matches!(b, V::Float(_))) || *op == BinOp::Mod {
                r.insert(Why::MixedKinds);
            }
            Ok(V::Float(match op {
                BinOp::Add => x.f() + y.f(),
                BinOp::Sub => x.f() - y.f(),
                BinOp::Mul => x.f() * y.f(),
                _ => x.f() % y.f(),
            }))
        }
    }
}

/// JUEL model: a missing map key or out-of-range index is null, an unset
/// `execution` variable is null, an unresolved top-level identifier is an
/// error. `r` collects the reasons the evaluated path could diverge from CEL.
fn reference(expr: &Expr, vars: &BTreeMap<String, V>, r: &mut Reasons) -> Outcome {
    Ok(match expr {
        Expr::Int(n) => V::Int(*n),
        Expr::Float(f) => V::Float(*f),
        Expr::Str(s) => V::Str(s.clone()),
        Expr::Bool(b) => V::Bool(*b),
        Expr::Null => V::Null,
        Expr::Var(name) => vars.get(name).cloned().ok_or(())?,
        Expr::ContextVar(name) => match vars.get(name) {
            Some(value) => value.clone(),
            None => {
                r.insert(Why::MissingValue);
                V::Null
            }
        },
        Expr::Member(base, name) => match reference(base, vars, r)? {
            V::Null => {
                r.insert(Why::MissingValue);
                V::Null
            }
            V::Map(map) => map.get(name).cloned().unwrap_or_else(|| {
                r.insert(Why::MissingValue);
                V::Null
            }),
            _ => return Err(()),
        },
        Expr::Index(base, key) => {
            let base = reference(base, vars, r)?;
            let key = reference(key, vars, r)?;
            match (base, key) {
                (V::Null, _) | (_, V::Null) => {
                    r.insert(Why::MissingValue);
                    V::Null
                }
                (V::List(items), key) => {
                    if !matches!(key, V::Int(_)) {
                        r.insert(Why::MixedKinds);
                    }
                    let index = match to_num(&key, r)? {
                        Num::I(i) => i,
                        Num::F(f) => f.trunc() as i64,
                    };
                    match usize::try_from(index)
                        .ok()
                        .and_then(|i| items.get(i).cloned())
                    {
                        Some(item) => item,
                        None => {
                            r.insert(Why::MissingValue);
                            V::Null
                        }
                    }
                }
                (V::Map(map), V::Str(key)) => map.get(&key).cloned().unwrap_or_else(|| {
                    r.insert(Why::MissingValue);
                    V::Null
                }),
                _ => return Err(()),
            }
        }
        Expr::Not(inner) => V::Bool(!to_bool(&reference(inner, vars, r)?, r)?),
        Expr::Neg(inner) => match to_num(&reference(inner, vars, r)?, r)? {
            Num::I(i) => V::Int(i.wrapping_neg()),
            Num::F(f) => V::Float(-f),
        },
        Expr::Empty(inner) => V::Bool(match reference(inner, vars, r)? {
            V::Str(s) => s.is_empty(),
            V::List(l) => l.is_empty(),
            V::Map(m) => m.is_empty(),
            V::Null => {
                r.insert(Why::EmptyOnScalar);
                true
            }
            _ => {
                r.insert(Why::EmptyOnScalar);
                false
            }
        }),
        Expr::Ternary(c, t, e) => {
            let condition = reference(c, vars, r)?;
            if to_bool(&condition, r)? {
                reference(t, vars, r)?
            } else {
                reference(e, vars, r)?
            }
        }
        Expr::Binary(BinOp::And, l, rhs) => {
            let left = reference(l, vars, r)?;
            if !to_bool(&left, r)? {
                V::Bool(false)
            } else {
                let right = reference(rhs, vars, r)?;
                V::Bool(to_bool(&right, r)?)
            }
        }
        Expr::Binary(BinOp::Or, l, rhs) => {
            let left = reference(l, vars, r)?;
            if to_bool(&left, r)? {
                V::Bool(true)
            } else {
                let right = reference(rhs, vars, r)?;
                V::Bool(to_bool(&right, r)?)
            }
        }
        Expr::Binary(op, l, rhs) => {
            let (a, b) = (reference(l, vars, r)?, reference(rhs, vars, r)?);
            let ordering = |wanted: fn(std::cmp::Ordering) -> bool, r: &mut Reasons| {
                Ok::<_, ()>(V::Bool(el_cmp(&a, &b, r)?.is_some_and(wanted)))
            };
            match op {
                BinOp::Eq => V::Bool(el_eq(&a, &b, r)?),
                BinOp::Ne => V::Bool(!el_eq(&a, &b, r)?),
                BinOp::Lt => ordering(|o| o.is_lt(), r)?,
                BinOp::Gt => ordering(|o| o.is_gt(), r)?,
                BinOp::Le => ordering(|o| o.is_le(), r)?,
                BinOp::Ge => ordering(|o| o.is_ge(), r)?,
                _ => el_arith(op, &a, &b, r)?,
            }
        }
        Expr::Range { .. } | Expr::InList(..) => return Err(()),
    })
}

fn grid() -> Vec<BTreeMap<String, V>> {
    let mut grid = Vec::new();
    for (i, a) in [-3, 0, 2, 7].into_iter().enumerate() {
        for (j, b) in [0, 3].into_iter().enumerate() {
            for (k, f) in [1.5, 2.0].into_iter().enumerate() {
                for n in ["7", "x"] {
                    for p in [false, true] {
                        for s in ["", "x"] {
                            let step = i + j + k + usize::from(p) + s.len();
                            let list = if step % 2 == 0 {
                                vec![]
                            } else {
                                vec![V::Int(1), V::Int(2)]
                            };
                            let mut map = BTreeMap::from([("w".to_owned(), V::Int(5))]);
                            if step % 3 != 0 {
                                map.insert("k".to_owned(), V::Int(step as i64 % 2));
                            }
                            grid.push(BTreeMap::from([
                                ("a".to_owned(), V::Int(a)),
                                ("b".to_owned(), V::Int(b)),
                                ("f".to_owned(), V::Float(f)),
                                ("n".to_owned(), V::Str(n.to_owned())),
                                ("p".to_owned(), V::Bool(p)),
                                ("q".to_owned(), V::Bool(step % 2 == 1)),
                                ("s".to_owned(), V::Str(s.to_owned())),
                                ("t".to_owned(), V::Str(["x", "b"][step % 2].to_owned())),
                                ("l".to_owned(), V::List(list)),
                                ("m".to_owned(), V::Map(map)),
                                ("z".to_owned(), V::Null),
                                ("w".to_owned(), V::Str("😀".to_owned())),
                            ]));
                        }
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

/// Sources span the coercion-free subset and every known JUEL/CEL divergence
/// (null, absent variable, missing key, floats, numeric strings, booleans as
/// text, UTF-16 ordering); `u` is never bound, `z` is bound to null.
const PROPERTY_SOURCES: &[&str] = &[
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
    "${m.w}",
    "${m['w'] + 1}",
    "${m.w == 5 && !empty m}",
    "${a == 2 || s == 'x'}",
    "${p && a > 0 ? 'pos' : 'other'}",
    "${a eq b}",
    "${execution.getVariable('a') + execution.variables.b}",
    "${a == null}",
    "${s != null && p}",
    "${p || l[5] == 1}",
    "${!p && l[5] == 1}",
    "${a + b + a - b - a}",
    "${p || q || p && q}",
    "${a * b * a % 5 % 3}",
    // null, unset and missing values
    "${z}",
    "${u}",
    "${z == null}",
    "${z != null}",
    "${u == null}",
    "${execution.getVariable('u') == null}",
    "${execution.getVariable('u')}",
    "${execution.getVariable('z') == null}",
    "${execution.variables.u != null}",
    "${m.k == null}",
    "${m.k != null}",
    "${m.k}",
    "${m.w == null}",
    "${m.nope == null}",
    "${l[5] == null}",
    "${l[0] == null}",
    "${l[-1] == null}",
    "${empty z}",
    "${empty u}",
    "${empty m.k}",
    "${empty l[9]}",
    "${!z}",
    "${z || p}",
    "${z ? 1 : 2}",
    "${n ? 1 : 2}",
    "${s ? 1 : 2}",
    "${!n}",
    "${p && z}",
    "${a + z}",
    "${z + z}",
    "${-z}",
    "${z < a}",
    "${z <= z}",
    "${z >= null}",
    "${null / null}",
    "${z / z}",
    "${l[null]}",
    "${m[null]}",
    "${l[z]}",
    "${empty a}",
    "${empty p}",
    // numeric strings, floats, int/float mixes
    "${a == n}",
    "${n == 7}",
    "${n == '7'}",
    "${n < a}",
    "${n + a}",
    "${a + n}",
    "${n * 2}",
    "${f == 2}",
    "${f == a}",
    "${f + a}",
    "${f * 2.0}",
    "${f < a}",
    "${f > 1}",
    "${f % 2}",
    "${f == f}",
    "${f + f}",
    "${f == 1.5}",
    "${a / b}",
    "${a / 2}",
    "${a div 2}",
    "${f / 2.0}",
    "${f / f}",
    "${7 / 2}",
    "${7.0 / 2.0}",
    "${a / 2.0}",
    "${l[f - 1.5]}",
    "${a + ''}",
    "${'' + ''}",
    // booleans as text
    "${p == 'true'}",
    "${p == n}",
    "${p && n}",
    "${n || p}",
    // UTF-16 against code point ordering (one side is always a literal)
    "${w < '\u{e000}'}",
    "${'\u{e000}' >= w}",
    "${w < 'a'}",
    "${w > 'a'}",
    // literal-only and bare-variable forms stay silent
    "${a}",
    "${f}",
    "${s}",
    "${1 + 2}",
    "${'a' == 'a'}",
    "${1 < 2 && 2 < 3}",
    "${true ? 1 : 2}",
    "${empty ''}",
];

#[test]
fn juel_and_cel_never_diverge_silently() {
    let grid = grid();
    let mut agreed_clean = 0usize;
    let mut clean_with_warnings = 0usize;
    let mut diverged = 0usize;
    let mut per_reason: BTreeMap<Why, usize> = BTreeMap::new();
    let mut silent_sources = 0usize;
    for source in PROPERTY_SOURCES {
        let translation = juel(source).unwrap_or_else(|r| panic!("`{source}` refused: {r}"));
        let parsed = parse(source, SourceLanguage::Juel).unwrap();
        if translation.warnings.is_empty() {
            silent_sources += 1;
        }
        for vars in &grid {
            let mut reasons = Reasons::new();
            let expected = reference(&parsed.expr, vars, &mut reasons);
            let actual = evaluate_cel(&translation.cel, vars);
            let agree = match (&expected, &actual) {
                (Ok(e), Ok(a)) => same(e, a),
                (Err(()), Err(_)) => true,
                _ => false,
            };
            if reasons.is_empty() {
                assert!(
                    agree,
                    "unexplained divergence for `{source}` -> `{}` with {vars:?}: JUEL {expected:?}, CEL {actual:?}",
                    translation.cel
                );
                agreed_clean += 1;
                clean_with_warnings += usize::from(!translation.warnings.is_empty());
                continue;
            }
            if !agree {
                diverged += 1;
            }
            for why in &reasons {
                *per_reason.entry(*why).or_default() += 1;
                assert!(
                    translation
                        .warnings
                        .iter()
                        .any(|w| w.code == why.code() && w.message.contains(why.message_hint())),
                    "`{source}` -> `{}` owes {why:?} ({}) with {vars:?}: {:?}",
                    translation.cel,
                    why.code(),
                    translation.warnings
                );
            }
        }
    }
    assert!(agreed_clean > 3000, "only {agreed_clean} reason-free rows");
    assert!(
        clean_with_warnings > 1000,
        "only {clean_with_warnings} reason-free rows that carry warnings"
    );
    assert!(diverged > 800, "only {diverged} diverging rows");
    for why in [
        Why::MissingValue,
        Why::NullCoercion,
        Why::MixedKinds,
        Why::BoolCoercion,
        Why::IntegerDivision,
        Why::EmptyOnScalar,
        Why::StringOrder,
    ] {
        assert!(
            per_reason.get(&why).copied().unwrap_or(0) > 0,
            "{why:?} was never exercised: {per_reason:?}"
        );
    }
    assert!(
        silent_sources >= 8,
        "only {silent_sources} warning-free sources"
    );
}

/// Every variable holds the kind the expression needs: JUEL and CEL must agree
/// exactly no matter how many warnings the translation carries.
#[test]
fn well_typed_expressions_agree_regardless_of_warnings() {
    let sources = [
        "${a + b}",
        "${a - b - 1}",
        "${a * b + 2}",
        "${(a + b) * 2}",
        "${-a}",
        "${a % b}",
        "${a < b}",
        "${a >= b}",
        "${a == b}",
        "${a != 2}",
        "${p && q}",
        "${p || q && !p}",
        "${!p}",
        "${p ? a : b}",
        "${a > b ? s : t}",
        "${s == t}",
        "${s < t}",
        "${s != 'x'}",
        "${empty s}",
        "${empty l}",
        "${empty m}",
        "${l[0] + l[1]}",
        "${l[1] == 2}",
        "${m.w + m.k}",
        "${m['w'] > 1}",
        "${f + f}",
        "${f * 2.0}",
        "${f < 2.0}",
        "${f == f}",
        "${f / f}",
        "${p == q}",
        "${execution.getVariable('a') + b}",
        "${a + b + a - b - a}",
        "${p || q || p && q}",
        "${a * b * a % 5 % 3}",
        "${w < 'a'}",
        "${s != null}",
        "${!(a < b) || p}",
    ];
    let mut rows = 0usize;
    for source in sources {
        let translation = juel(source).unwrap();
        let parsed = parse(source, SourceLanguage::Juel).unwrap();
        for a in [-3, 0, 2, 7] {
            for b in [2, 3] {
                for p in [false, true] {
                    for (s, t) in [("x", "abc"), ("abc", "abc")] {
                        for f in [1.5, 2.0] {
                            let vars = BTreeMap::from([
                                ("a".to_owned(), V::Int(a)),
                                ("b".to_owned(), V::Int(b)),
                                ("p".to_owned(), V::Bool(p)),
                                ("q".to_owned(), V::Bool(!p || a > 0)),
                                ("s".to_owned(), V::Str(s.to_owned())),
                                ("t".to_owned(), V::Str(t.to_owned())),
                                ("f".to_owned(), V::Float(f)),
                                ("w".to_owned(), V::Str("😀".to_owned())),
                                ("l".to_owned(), V::List(vec![V::Int(1), V::Int(2)])),
                                (
                                    "m".to_owned(),
                                    V::Map(BTreeMap::from([
                                        ("w".to_owned(), V::Int(5)),
                                        ("k".to_owned(), V::Int(1)),
                                    ])),
                                ),
                            ]);
                            let mut reasons = Reasons::new();
                            let expected = reference(&parsed.expr, &vars, &mut reasons).unwrap();
                            let actual =
                                evaluate_cel(&translation.cel, &vars).unwrap_or_else(|e| {
                                    panic!("`{source}` -> `{}` with {vars:?}: {e}", translation.cel)
                                });
                            assert!(
                                reasons.is_empty(),
                                "`{source}` is not well typed here: {reasons:?}"
                            );
                            assert!(
                                same(&expected, &actual),
                                "`{source}` -> `{}` with {vars:?}: {expected:?} vs {actual}",
                                translation.cel
                            );
                            rows += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(rows > 2000);
}

#[test]
fn known_divergences_carry_their_specific_warning() {
    let base = || {
        BTreeMap::from([
            ("a".to_owned(), V::Int(7)),
            ("n".to_owned(), V::Str("7".to_owned())),
            ("z".to_owned(), V::Null),
            ("w".to_owned(), V::Str("😀".to_owned())),
            (
                "m".to_owned(),
                V::Map(BTreeMap::from([("w".to_owned(), V::Int(5))])),
            ),
            ("l".to_owned(), V::List(vec![])),
            ("p".to_owned(), V::Bool(false)),
        ])
    };
    // (source, JUEL result, code the translation must carry)
    let cases: &[(&str, V, &str)] = &[
        ("${execution.getVariable('u') == null}", V::Bool(true), M),
        ("${execution.variables.u == null}", V::Bool(true), M),
        ("${m.nope == null}", V::Bool(true), M),
        ("${l[5] == null}", V::Bool(true), M),
        ("${a == n}", V::Bool(true), C),
        ("${n + a}", V::Int(14), C),
        ("${!z}", V::Bool(true), C),
        ("${z ? 1 : 2}", V::Int(2), C),
        ("${p == 'false'}", V::Bool(true), C),
        ("${w < '\u{e000}'}", V::Bool(true), C),
        ("${empty z}", V::Bool(true), E),
        ("${a / 2}", V::Float(3.5), D),
        ("${a + ''}", V::Int(7), C),
        ("${null / null}", V::Int(0), C),
    ];
    for (source, juel_result, code) in cases {
        let vars = base();
        let translation = juel(source).unwrap();
        let parsed = parse(source, SourceLanguage::Juel).unwrap();
        let mut reasons = Reasons::new();
        assert_eq!(
            reference(&parsed.expr, &vars, &mut reasons),
            Ok(juel_result.clone()),
            "JUEL model for `{source}`"
        );
        assert!(
            !reasons.is_empty(),
            "`{source}` should be explained by a reason"
        );
        let cel = evaluate_cel(&translation.cel, &vars);
        assert!(
            !matches!(&cel, Ok(v) if same(juel_result, v)),
            "`{source}` unexpectedly agrees with JUEL ({cel:?}); drop it from the divergence table"
        );
        assert!(
            codes(&translation).contains(code),
            "`{source}` lacks {code}: {:?}",
            translation.warnings
        );
    }
    let vars = base();
    for source in ["${z == null}", "${u == null}"] {
        let guard = juel(source).unwrap();
        assert!(
            guard.warnings.is_empty(),
            "a bare-identifier null guard errors in both languages: {source}"
        );
    }
    assert_eq!(
        evaluate_cel(&juel("${z == null}").unwrap().cel, &vars).unwrap(),
        json!(true)
    );
}

// ---- T1: independent evaluator over constant expressions ----

#[derive(Debug, Clone, PartialEq)]
enum K {
    I(i64),
    F(f64),
    B(bool),
    S(String),
}

fn k_tokens(inner: &str) -> Vec<String> {
    let chars: Vec<char> = inner.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == ' ' {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else if c == '\'' {
            let start = i + 1;
            i = start;
            while chars[i] != '\'' {
                i += 1;
            }
            tokens.push(format!("'{}", chars[start..i].iter().collect::<String>()));
            i += 1;
        } else if c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else {
            let pair: String = chars[i..chars.len().min(i + 2)].iter().collect();
            if ["&&", "||", "==", "!=", "<=", ">="].contains(&pair.as_str()) {
                tokens.push(pair);
                i += 2;
            } else {
                tokens.push(c.to_string());
                i += 1;
            }
        }
    }
    tokens
}

const K_LEVELS: [&[&str]; 6] = [
    &["||", "or"],
    &["&&", "and"],
    &["==", "!=", "eq", "ne"],
    &["<", ">", "<=", ">=", "lt", "gt", "le", "ge"],
    &["+", "-"],
    &["*", "/", "div", "%", "mod"],
];

struct KEval {
    tokens: Vec<String>,
    at: usize,
}

impl KEval {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.at).map(String::as_str)
    }

    fn eat(&mut self, token: &str) -> bool {
        let hit = self.peek() == Some(token);
        if hit {
            self.at += 1;
        }
        hit
    }

    fn ternary(&mut self) -> K {
        let condition = self.binary(0);
        if self.eat("?") {
            let then = self.ternary();
            assert!(self.eat(":"));
            let otherwise = self.ternary();
            return if condition == K::B(true) {
                then
            } else {
                otherwise
            };
        }
        condition
    }

    fn binary(&mut self, level: usize) -> K {
        if level == K_LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1);
        while let Some(op) = self
            .peek()
            .filter(|t| K_LEVELS[level].contains(t))
            .map(str::to_owned)
        {
            self.at += 1;
            let right = self.binary(level + 1);
            left = k_apply(&op, left, right);
        }
        left
    }

    fn unary(&mut self) -> K {
        if self.eat("!") || self.eat("not") {
            let K::B(b) = self.unary() else {
                panic!("not on a non-boolean")
            };
            return K::B(!b);
        }
        if self.eat("-") {
            return match self.unary() {
                K::I(i) => K::I(-i),
                K::F(f) => K::F(-f),
                other => panic!("minus on {other:?}"),
            };
        }
        if self.eat("empty") {
            let K::S(s) = self.unary() else {
                panic!("empty on a non-string constant")
            };
            return K::B(s.is_empty());
        }
        self.primary()
    }

    fn primary(&mut self) -> K {
        let token = self.tokens[self.at].clone();
        self.at += 1;
        match token.as_str() {
            "(" => {
                let inner = self.ternary();
                assert!(self.eat(")"));
                inner
            }
            "true" => K::B(true),
            "false" => K::B(false),
            t if t.starts_with('\'') => K::S(t[1..].to_owned()),
            t if t.contains('.') => K::F(t.parse().unwrap()),
            t => K::I(t.parse().unwrap()),
        }
    }
}

fn k_num(k: &K) -> Option<f64> {
    match k {
        K::I(i) => Some(*i as f64),
        K::F(f) => Some(*f),
        _ => None,
    }
}

fn k_apply(op: &str, a: K, b: K) -> K {
    use std::cmp::Ordering::*;
    let order = |a: &K, b: &K| match (a, b) {
        (K::S(x), K::S(y)) => x.cmp(y),
        _ => k_num(a).unwrap().partial_cmp(&k_num(b).unwrap()).unwrap(),
    };
    match op {
        "||" | "or" => K::B(a == K::B(true) || b == K::B(true)),
        "&&" | "and" => K::B(a == K::B(true) && b == K::B(true)),
        "==" | "eq" => K::B(match (k_num(&a), k_num(&b)) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        }),
        "!=" | "ne" => K::B(!(k_apply("==", a, b) == K::B(true))),
        "<" | "lt" => K::B(order(&a, &b) == Less),
        ">" | "gt" => K::B(order(&a, &b) == Greater),
        "<=" | "le" => K::B(order(&a, &b) != Greater),
        ">=" | "ge" => K::B(order(&a, &b) != Less),
        "/" | "div" => K::F(k_num(&a).unwrap() / k_num(&b).unwrap()),
        "+" | "-" | "*" | "%" | "mod" => match (&a, &b) {
            (K::I(x), K::I(y)) => K::I(match op {
                "+" => x + y,
                "-" => x - y,
                "*" => x * y,
                _ => x % y,
            }),
            _ => {
                let (x, y) = (k_num(&a).unwrap(), k_num(&b).unwrap());
                K::F(match op {
                    "+" => x + y,
                    "-" => x - y,
                    "*" => x * y,
                    _ => x % y,
                })
            }
        },
        other => panic!("operator {other}"),
    }
}

fn k_json(k: &K) -> Value {
    match k {
        K::I(i) => json!(i),
        K::F(f) => json!(f),
        K::B(b) => json!(b),
        K::S(s) => json!(s),
    }
}

fn json_same(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => expected == actual,
    }
}

/// Expected values are worked out by hand from the EL 3.0 precedence table
/// (ternary right-associative, unary above `* / % div mod`, relational above
/// equality); the last column marks the one known divergence, JUEL `/`.
#[test]
fn constant_expressions_match_hand_worked_el_results() {
    let table: &[(&str, Value, bool)] = &[
        ("${1 + 2 * 3}", json!(7), false),
        ("${(1 + 2) * 3}", json!(9), false),
        ("${10 - 4 - 3}", json!(3), false),
        ("${2 * 3 % 4}", json!(2), false),
        ("${20 mod 6 * 2}", json!(4), false),
        ("${100 - 2 * 3 * 4}", json!(76), false),
        ("${true || false && false}", json!(true), false),
        ("${(true || false) && false}", json!(false), false),
        ("${false && true || true}", json!(true), false),
        ("${!true || true}", json!(true), false),
        ("${!(true || true)}", json!(false), false),
        ("${not false and false}", json!(false), false),
        ("${1 < 2 == true}", json!(true), false),
        ("${1 == 1 != false}", json!(true), false),
        ("${3 > 2 && 2 > 1}", json!(true), false),
        ("${1 + 1 == 2}", json!(true), false),
        ("${2 * 3 >= 6}", json!(true), false),
        ("${true ? 1 : 2}", json!(1), false),
        ("${false ? 1 : 2}", json!(2), false),
        ("${true ? false ? 1 : 2 : 3}", json!(2), false),
        ("${false ? 1 : false ? 2 : 3}", json!(3), false),
        ("${true ? 1 : true ? 2 : 3}", json!(1), false),
        ("${false ? 1 : true ? 2 : 3}", json!(2), false),
        ("${1 > 2 ? 'a' : 'b'}", json!("b"), false),
        ("${true && false ? 1 : 2}", json!(2), false),
        ("${false || true ? 'y' : 'n'}", json!("y"), false),
        ("${-3 + 5}", json!(2), false),
        ("${- -3}", json!(3), false),
        ("${-(2 + 3)}", json!(-5), false),
        ("${2 - -3}", json!(5), false),
        ("${empty ''}", json!(true), false),
        ("${empty 'a'}", json!(false), false),
        ("${!empty ''}", json!(false), false),
        ("${not empty 'a'}", json!(true), false),
        ("${empty '' && true}", json!(true), false),
        ("${empty 'a' || empty ''}", json!(true), false),
        ("${7 mod 3}", json!(1), false),
        ("${-7 % 3}", json!(-1), false),
        ("${7 % -3}", json!(1), false),
        ("${'ab' == 'ab'}", json!(true), false),
        ("${'ab' != 'ab'}", json!(false), false),
        ("${'a' < 'b'}", json!(true), false),
        ("${'b' lt 'a'}", json!(false), false),
        ("${1.5 + 1.5}", json!(3.0), false),
        ("${2.0 * 2.5}", json!(5.0), false),
        ("${1 eq 1 and 2 ne 3}", json!(true), false),
        (
            "${5 ge 5 && 5 le 5 && 6 gt 5 && 4 lt 5}",
            json!(true),
            false,
        ),
        ("${true == false || !false}", json!(true), false),
        ("${1 + 2 == 3 ? 'ok' : 'bad'}", json!("ok"), false),
        ("${2 + 3 * 4 - 5}", json!(9), false),
        ("${(2 + 3) * (4 - 5)}", json!(-5), false),
        ("${1 - (2 - 3)}", json!(2), false),
        ("${100 - 30 - 20 - 5}", json!(45), false),
        ("${2 * 3 * 4 * 5}", json!(120), false),
        ("${100 % 7 % 4}", json!(2), false),
        ("${true && false && true || true}", json!(true), false),
        (
            "${false || false || true && true && false}",
            json!(false),
            false,
        ),
        ("${1 + 2 + 3 * 4 * 5 - 6 - 7}", json!(50), false),
        ("${10 - 2 - (3 - 1)}", json!(6), false),
        ("${2 * (3 * 4) * 5}", json!(120), false),
        ("${100 - (30 - 20) - 5}", json!(85), false),
        ("${true && (false || true) && true}", json!(true), false),
        ("${7.0 / 2.0}", json!(3.5), false),
        ("${7 / 2}", json!(3.5), true),
        ("${7 div 2}", json!(3.5), true),
        ("${1 + 6 / 4}", json!(2.5), true),
    ];
    assert!(table.len() >= 40);
    let none = BTreeMap::new();
    for (source, expected, diverges) in table {
        let inner = &source[2..source.len() - 1];
        let mut model = KEval {
            tokens: k_tokens(inner),
            at: 0,
        };
        let modelled = k_json(&model.ternary());
        assert_eq!(model.at, model.tokens.len(), "model consumed `{source}`");
        assert!(
            json_same(expected, &modelled),
            "hand value vs independent model for `{source}`: {expected} vs {modelled}"
        );
        let translation = juel(source).unwrap();
        let cel = evaluate_cel(&translation.cel, &none)
            .unwrap_or_else(|e| panic!("`{source}` -> `{}`: {e}", translation.cel));
        if *diverges {
            assert!(
                !json_same(expected, &cel),
                "`{source}` was expected to diverge, CEL gave {cel}"
            );
            assert!(
                codes(&translation).contains(&D),
                "`{source}` lacks the division warning"
            );
        } else {
            assert!(
                json_same(expected, &cel),
                "`{source}` -> `{}`: expected {expected}, CEL {cel}",
                translation.cel
            );
        }
    }
}

// ---- T4: FEEL ranges and lists against an independent evaluator ----

/// Parses the source text itself (opener, `..`, closer) instead of the AST.
fn feel_model(source: &str, vars: &BTreeMap<&str, i64>, x: i64) -> bool {
    let rhs = source.split(" in ").nth(1).unwrap().trim();
    let resolve = |t: &str| t.trim().parse::<i64>().unwrap_or_else(|_| vars[t.trim()]);
    let opener = rhs.chars().next().unwrap();
    let closer = rhs.chars().last().unwrap();
    let inner = &rhs[1..rhs.len() - 1];
    if let Some((lo, hi)) = inner.split_once("..") {
        let (lo, hi) = (resolve(lo), resolve(hi));
        (if opener == '[' { x >= lo } else { x > lo })
            && (if closer == ']' { x <= hi } else { x < hi })
    } else {
        inner.split(',').any(|item| resolve(item) == x)
    }
}

#[test]
fn feel_ranges_and_lists_match_an_independent_evaluator() {
    let mut sources = Vec::new();
    for opener in ['[', '('] {
        for closer in [']', ')'] {
            for (lo, hi) in [("1", "5"), ("3", "3"), ("5", "1"), ("a", "b"), ("-2", "b")] {
                sources.push(format!("= x in {opener}{lo}..{hi}{closer}"));
            }
        }
    }
    for list in ["[1,2,3]", "[5]", "[0, 7]", "[a, 2, b]"] {
        sources.push(format!("= x in {list}"));
    }
    let mut checked = 0;
    for source in &sources {
        let translation =
            translate(source, SourceLanguage::Feel).unwrap_or_else(|r| panic!("`{source}`: {r}"));
        for x in -4..=9 {
            let model_vars = BTreeMap::from([("a", 1), ("b", 5)]);
            let vars = BTreeMap::from([
                ("x".to_owned(), V::Int(x)),
                ("a".to_owned(), V::Int(1)),
                ("b".to_owned(), V::Int(5)),
            ]);
            assert_eq!(
                evaluate_cel(&translation.cel, &vars).unwrap(),
                json!(feel_model(source, &model_vars, x)),
                "`{source}` -> `{}` at x = {x}",
                translation.cel
            );
            checked += 1;
        }
    }
    assert!(checked >= 300);
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
        evaluate_cel(&juel("${execution.variables['a b']}").unwrap().cel, &vars).unwrap(),
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
        ("${execution.variables}", "context", 2),
        ("${execution.variables[x]}", "computed name", 21),
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
        ("${task}", "resolver", 2),
        ("${externalTask.x}", "resolver", 2),
        ("${caseExecution}", "resolver", 2),
        ("${authenticatedUserId == 'x'}", "resolver", 2),
        ("${a.class}", "introspection", 4),
        ("${a['class']}", "introspection", 3),
        ("${(-5).x}", "literal", 7),
        ("${(-1.5)[0]}", "literal", 8),
        ("${'\u{1f600}' < '\u{e000}'}", "UTF-16", 6),
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
    let chain = format!("${{{}1{}}}", "1-(".repeat(40), ")".repeat(40));
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
