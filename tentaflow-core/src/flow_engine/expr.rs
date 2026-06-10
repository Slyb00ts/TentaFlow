// ===== File: flow_engine/expr.rs — CEL expression evaluation over the flow scope (HARNESS_PLAN §3.12) =====
//
// Single expression engine for every CEL consumer in the flow engine:
// `condition.expression`, `loop.until`, `map.items` and the generic
// `input_mapping`/`output_mapping` in the executor. The scope binds `vars`,
// `payload`, `artifacts` and `meta` plus caller-supplied extras (`item`,
// `index`, `iteration`) as top-level CEL variables.
//
// Crate choice: `cel` 0.13 — the renamed continuation of `cel-interpreter`
// (cel-rust project; `cel-interpreter` stopped at 0.10). Sandboxed, no I/O,
// non-Turing-complete. Hostile input is contained by up-front validation and
// a budget, not by panic-catching: a length cap, a nesting cap over brackets
// AND `?` ternaries (the crate's own parser recursion guard does NOT cover
// list/map literals and panics on the ternary chains it does cover), a
// wall-clock evaluation deadline, and a dedicated big-stack worker thread for
// parser/interpreter recursion. The worker's panic-to-error translation is
// dev/test-only belt-and-braces — the production binary builds with
// `panic = "abort"`, so a reachable panic aborts the process before any
// boundary could see it. The real defense is the hostile-input test battery
// below, pinning that JSON-representable scopes cannot reach known panic
// sites in cel 0.13; any reachable panic found there is a bug to neutralize
// with pre-parse/pre-eval validation, never something to catch.

use std::collections::{BTreeMap, HashMap};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use cel::{Context, Program, Value as CelValue};

use super::blob_store::BlobRef;
use super::envelope::FlowValue;

/// Hard cap on expression length. Flow configs are human-written; anything
/// longer is either a mistake or hostile input and is rejected before
/// parsing. The cap doubles as the parser-recursion bound for bracketless
/// operator chains (`a.a.a…`, `1+1+1…`): every chain link nests the parse
/// tree one level and consumes at least two characters, debug-build
/// parse/visit frames run tens of KB per level, and the measured overflow
/// cliff sits between 383 and 511 levels on a 16 MB stack — so 512 chars
/// (<= 255 levels) on the 32 MB worker stack keeps ~2x headroom. Raising
/// this limit requires re-running the hostile battery below.
pub const MAX_EXPR_CHARS: usize = 512;

/// Hard cap on parser nesting depth, counted over `(`/`[`/`{` brackets and
/// `?` ternaries. The cel parser recursion is extremely stack-hungry in debug
/// builds (~100 KB+ per nesting level) and its own recursion guard does not
/// cover list/map literals, so deep input aborts the process with a stack
/// overflow instead of erroring. `?` counts because the ternary else-arm is
/// right-recursive (`cond ? a : expr`): a bracketless chain of ternaries
/// re-enters the parser's `expr` rule once per `?`, and tripping the crate's
/// own depth limit on that shape panics on a `u16` underflow in cel 0.13
/// (parser.rs:405, debug builds). Checked before parsing; counting ignores
/// string literals on purpose — a false rejection of brackets or `?` inside
/// strings is acceptable, a missed deep input is a crash.
pub const MAX_EXPR_NESTING: usize = 32;

/// Default wall-clock budget for one parse or evaluation pass. CEL is not
/// Turing-complete, but comprehensions compose polynomially
/// (`items.map(x, items.map(y, …))` over a few-thousand-element list pins a
/// core for minutes), so every pass gets a deadline; flow expressions are
/// small data plumbing and a second is already generous.
pub const DEFAULT_EVAL_TIMEOUT_MS: u64 = 1_000;

/// Compiled-program cache size. On overflow the whole cache is cleared —
/// flows reuse a small, stable set of expressions, so a full clear is a
/// one-off cost and keeps the eviction logic trivial.
const PROGRAM_CACHE_CAP: usize = 256;

/// Stack size for the dedicated parse/evaluate thread. The cel parser burns
/// over 100 KB of stack per bracket-nesting level in debug builds — 64 nested
/// parens overflow even an 8 MB stack, and default 2 MB threads (tokio
/// workers, test threads) die far earlier — so all parser/interpreter
/// recursion runs on a thread with a known-large stack. Sized together with
/// `MAX_EXPR_CHARS` (see there for the operator-chain depth math) with ~2x
/// headroom; the reservation is address space, committed lazily.
const EVAL_STACK_BYTES: usize = 32 * 1024 * 1024;

/// Expression failure carrying the offending expression text and a
/// human-readable cause. The caller prepends the node name when building the
/// node error message ("node name, expression, cause" per §3.12).
#[derive(Debug, Clone, thiserror::Error)]
#[error("expression `{expression}`: {cause}")]
pub struct ExprError {
    pub expression: String,
    pub cause: String,
}

impl ExprError {
    fn new(expression: &str, cause: impl Into<String>) -> Self {
        // Over-limit hostile input must not be copied verbatim into error
        // messages; valid expressions (<= MAX_EXPR_CHARS) stay untruncated.
        let expression = if expression.chars().count() > MAX_EXPR_CHARS {
            let mut truncated: String = expression.chars().take(MAX_EXPR_CHARS).collect();
            truncated.push('…');
            truncated
        } else {
            expression.to_string()
        };
        Self {
            expression,
            cause: cause.into(),
        }
    }
}

/// Read-only view of the data a CEL expression can see. Borrowed from the
/// envelope by the caller; `extras` carry loop/map locals (`iteration`,
/// `item`, `index`) bound as top-level variables. Extras are bound last, so
/// on a name collision the extra wins — callers control extra names.
pub struct ExprScope<'a> {
    pub vars: &'a BTreeMap<String, FlowValue>,
    pub payload: &'a FlowValue,
    pub artifacts: &'a HashMap<String, FlowValue>,
    pub meta: &'a BTreeMap<String, serde_json::Value>,
    pub extras: &'a [(&'a str, serde_json::Value)],
}

/// Evaluates a CEL expression against the scope and returns the result as
/// JSON. `timeout` bounds the wall-clock budget per parse/evaluation pass
/// (`None` = `DEFAULT_EVAL_TIMEOUT_MS`). Hostile input is rejected by the
/// length/nesting caps before parsing and by the deadline during evaluation.
/// Library panics become errors in dev/test builds only — in release
/// (`panic = "abort"`) they would abort the process, which is why the
/// hostile-input tests pin that JSON-representable scopes cannot reach the
/// crate's known panic sites.
pub fn evaluate(
    expr: &str,
    scope: &ExprScope,
    timeout: Option<Duration>,
) -> Result<serde_json::Value, ExprError> {
    let value = evaluate_cel(expr, scope, timeout)?;
    value
        .json()
        .map_err(|e| ExprError::new(expr, format!("result is not representable as JSON: {e}")))
}

/// Strict boolean evaluation for `condition.expression` / `loop.until`:
/// any non-bool result is an error — no silent truthiness coercion.
pub fn evaluate_bool(
    expr: &str,
    scope: &ExprScope,
    timeout: Option<Duration>,
) -> Result<bool, ExprError> {
    match evaluate_cel(expr, scope, timeout)? {
        CelValue::Bool(b) => Ok(b),
        other => Err(ExprError::new(
            expr,
            format!("expected bool, got {}", other.type_of()),
        )),
    }
}

/// JSON projection of a `FlowValue` for the CEL scope. Blob variants
/// (Audio/Image/Video/Other) expose only a small descriptor
/// `{kind, mime, size_bytes}` — never bytes; `kind` uses the same snake_case
/// tags as the FlowValue serde representation.
pub fn flow_value_to_json(value: &FlowValue) -> serde_json::Value {
    match value {
        FlowValue::Empty => serde_json::Value::Null,
        FlowValue::Text(t) => serde_json::Value::String(t.clone()),
        FlowValue::Json(j) => j.clone(),
        FlowValue::Embedding(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|f| {
                    // NaN/inf are not representable in JSON — map to null
                    // instead of failing the whole scope binding.
                    serde_json::Number::from_f64(f64::from(*f))
                        .map(serde_json::Value::Number)
                        .unwrap_or(serde_json::Value::Null)
                })
                .collect(),
        ),
        FlowValue::Audio { blob_ref, mime, .. } => blob_descriptor("audio", mime, blob_ref),
        FlowValue::Image { blob_ref, mime, .. } => blob_descriptor("image", mime, blob_ref),
        FlowValue::Video { blob_ref, mime, .. } => blob_descriptor("video", mime, blob_ref),
        FlowValue::Other { blob_ref, mime, .. } => blob_descriptor("other", mime, blob_ref),
    }
}

fn blob_descriptor(kind: &str, mime: &str, blob_ref: &BlobRef) -> serde_json::Value {
    serde_json::json!({
        "kind": kind,
        "mime": mime,
        "size_bytes": blob_ref.size_bytes,
    })
}

fn evaluate_cel(
    expr: &str,
    scope: &ExprScope,
    timeout: Option<Duration>,
) -> Result<CelValue, ExprError> {
    if expr.trim().is_empty() {
        return Err(ExprError::new(expr, "expression is empty"));
    }
    if expr.chars().count() > MAX_EXPR_CHARS {
        return Err(ExprError::new(
            expr,
            format!("expression exceeds the {MAX_EXPR_CHARS}-character limit"),
        ));
    }
    if nesting_depth(expr) > MAX_EXPR_NESTING {
        return Err(ExprError::new(
            expr,
            format!(
                "expression exceeds the nesting depth limit of {MAX_EXPR_NESTING} \
                 (`?` ternaries count toward this limit, and so do brackets and `?` \
                 inside string literals)"
            ),
        ));
    }
    let timeout = timeout.unwrap_or(Duration::from_millis(DEFAULT_EVAL_TIMEOUT_MS));
    let program = compiled(expr, timeout)?;
    let bindings = referenced_bindings(&program, scope);
    // Execution runs on the big-stack worker under the wall-clock budget:
    // interpreter recursion follows AST depth, and the crate has internal
    // `unwrap()`s (e.g. Value -> Val conversion in Context) that are
    // unreachable for pure-data scopes — pinned by the hostile-input tests,
    // because in release (`panic = "abort"`) a reachable panic would abort
    // the whole process, not surface as a node error.
    let outcome = run_on_eval_stack(
        {
            let program = Arc::clone(&program);
            move || {
                let mut ctx = Context::default();
                for (name, value) in bindings {
                    ctx.add_variable_from_value(name, value);
                }
                program
                    .execute(&ctx)
                    .map_err(|e| format!("evaluation failed: {e}"))
            }
        },
        timeout,
    );
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(cause)) => Err(ExprError::new(expr, cause)),
        Err(cause) => Err(ExprError::new(expr, cause)),
    }
}

/// Runs `f` on a freshly spawned thread with `EVAL_STACK_BYTES` of stack and
/// a wall-clock deadline. The worker owns everything it touches, so the
/// caller never blocks on it past the budget. The per-call spawn cost
/// (~tens of µs) is negligible next to flow node work, and the program cache
/// keeps recompilation off the hot path.
///
/// `catch_unwind` is dev/test-only belt-and-braces (see the module header):
/// release builds abort on panic before it could run, so hostile-input tests
/// pin panic-freedom instead of relying on this translation.
fn run_on_eval_stack<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
    timeout: Duration,
) -> Result<T, String> {
    let (tx, rx) = mpsc::sync_channel(1);
    let handle = std::thread::Builder::new()
        .name("cel-eval".into())
        .stack_size(EVAL_STACK_BYTES)
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
                .map_err(|panic| format!("internal evaluator panic: {}", panic_message(&*panic)));
            let _ = tx.send(outcome);
        })
        .map_err(|e| format!("cannot spawn evaluation thread: {e}"))?;
    match rx.recv_timeout(timeout) {
        Ok(outcome) => {
            // The worker already sent its result, so this join is bounded.
            let _ = handle.join();
            outcome
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Deliberate leak: detach the runaway worker instead of joining.
            // The thread (and the bindings moved into it) dies on its own
            // when the polynomial evaluation finally finishes; the caller
            // must return now, not block behind it.
            drop(handle);
            Err(format!(
                "evaluation deadline exceeded ({} ms)",
                timeout.as_millis()
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("evaluation thread terminated without a result".into())
        }
    }
}

/// Maximum running depth of `(`/`[`/`{` brackets, with closers clamped at
/// zero. `?` also opens a level and nothing closes it: the ternary else-arm
/// nests to the right without any bracket, so only the total `?` count bounds
/// the parser recursion of a chain (see `MAX_EXPR_NESTING`). Deliberately
/// string-literal-agnostic.
fn nesting_depth(expr: &str) -> usize {
    let mut depth = 0usize;
    let mut max_depth = 0usize;
    for c in expr.chars() {
        match c {
            '(' | '[' | '{' | '?' => {
                depth += 1;
                max_depth = max_depth.max(depth);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max_depth
}

/// Converts only the scope variables the compiled program actually references
/// into owned CEL values. Binding everything would deep-clone the entire
/// scope on every evaluation; `Program::references()` lets a `loop.until`
/// like `vars.harness_done == true` skip a multi-megabyte payload entirely.
/// Over-collection is harmless (comprehension loop variables also show up as
/// references); ownership is what the evaluation worker thread moves in.
/// Extras stay bound last so they shadow base names on collision.
fn referenced_bindings(program: &Program, scope: &ExprScope) -> Vec<(String, CelValue)> {
    let refs = program.references();
    let mut bindings: Vec<(String, CelValue)> = Vec::new();
    if refs.has_variable("vars") {
        let vars: HashMap<String, CelValue> = scope
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), json_to_cel(&flow_value_to_json(v))))
            .collect();
        bindings.push(("vars".into(), CelValue::Map(vars.into())));
    }
    if refs.has_variable("payload") {
        bindings.push((
            "payload".into(),
            json_to_cel(&flow_value_to_json(scope.payload)),
        ));
    }
    if refs.has_variable("artifacts") {
        let artifacts: HashMap<String, CelValue> = scope
            .artifacts
            .iter()
            .map(|(k, v)| (k.clone(), json_to_cel(&flow_value_to_json(v))))
            .collect();
        bindings.push(("artifacts".into(), CelValue::Map(artifacts.into())));
    }
    if refs.has_variable("meta") {
        let meta: HashMap<String, CelValue> = scope
            .meta
            .iter()
            .map(|(k, v)| (k.clone(), json_to_cel(v)))
            .collect();
        bindings.push(("meta".into(), CelValue::Map(meta.into())));
    }
    for (name, value) in scope.extras {
        if refs.has_variable(name) {
            bindings.push(((*name).to_string(), json_to_cel(value)));
        }
    }
    bindings
}

/// JSON -> CEL conversion. Hand-rolled instead of `cel::to_value` because the
/// serde path maps every non-negative integer to `UInt`, and cel-rust has no
/// Int/UInt cross-type overloads — `vars.attempt == 1` would silently compare
/// `UInt(1) == Int(1)` and yield false. Integers that fit i64 become `Int`
/// (the type of CEL integer literals); only values above i64::MAX stay UInt.
fn json_to_cel(value: &serde_json::Value) -> CelValue {
    match value {
        serde_json::Value::Null => CelValue::Null,
        serde_json::Value::Bool(b) => CelValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                CelValue::Int(i)
            } else if let Some(u) = n.as_u64() {
                CelValue::UInt(u)
            } else {
                // serde_json numbers are i64, u64 or f64 — this branch is the
                // f64 case, where as_f64 is always Some.
                CelValue::Float(n.as_f64().unwrap_or_default())
            }
        }
        serde_json::Value::String(s) => CelValue::String(Arc::new(s.clone())),
        serde_json::Value::Array(items) => {
            CelValue::List(Arc::new(items.iter().map(json_to_cel).collect()))
        }
        serde_json::Value::Object(map) => {
            let entries: HashMap<String, CelValue> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_cel(v)))
                .collect();
            CelValue::Map(entries.into())
        }
    }
}

/// Returns the compiled program for `expr`, compiling and caching on miss.
/// Compilation goes through a full ANTLR-style recursive parser — non-trivial
/// for expressions re-evaluated per loop/map iteration, hence the cache.
/// Failures are not cached: hostile unique expressions would only thrash the
/// (cheaply failing) parser, while poisoning the cache with garbage keys.
fn compiled(expr: &str, timeout: Duration) -> Result<Arc<Program>, ExprError> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Program>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(program) = lock_cache(cache).get(expr) {
        return Ok(Arc::clone(program));
    }
    let source = expr.to_string();
    let compile_outcome = run_on_eval_stack(move || Program::compile(&source), timeout);
    let program = match compile_outcome {
        Ok(Ok(program)) => Arc::new(program),
        Ok(Err(e)) => return Err(ExprError::new(expr, format!("parse error: {e}"))),
        Err(cause) => return Err(ExprError::new(expr, cause)),
    };
    let mut guard = lock_cache(cache);
    if guard.len() >= PROGRAM_CACHE_CAP {
        guard.clear();
    }
    guard.insert(expr.to_string(), Arc::clone(&program));
    Ok(program)
}

fn lock_cache(
    cache: &Mutex<HashMap<String, Arc<Program>>>,
) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Program>>> {
    // The lock only guards plain HashMap ops, so a poisoned state (panic on
    // another thread mid-insert) leaves the map structurally valid — recover
    // instead of propagating the poison panic.
    cache.lock().unwrap_or_else(PoisonError::into_inner)
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    /// Owns scope data so tests can build an `ExprScope` of borrows.
    struct Fixture {
        vars: BTreeMap<String, FlowValue>,
        payload: FlowValue,
        artifacts: HashMap<String, FlowValue>,
        meta: BTreeMap<String, serde_json::Value>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                vars: BTreeMap::new(),
                payload: FlowValue::Empty,
                artifacts: HashMap::new(),
                meta: BTreeMap::new(),
            }
        }

        fn scope<'a>(&'a self, extras: &'a [(&'a str, serde_json::Value)]) -> ExprScope<'a> {
            ExprScope {
                vars: &self.vars,
                payload: &self.payload,
                artifacts: &self.artifacts,
                meta: &self.meta,
                extras,
            }
        }
    }

    fn eval(expr: &str) -> Result<serde_json::Value, ExprError> {
        let fx = Fixture::new();
        evaluate(expr, &fx.scope(&[]), None)
    }

    fn blob(size_bytes: u64, mime: &str) -> BlobRef {
        BlobRef {
            id: "blob-1".into(),
            size_bytes,
            mime: mime.into(),
            sha256: "0".repeat(64),
        }
    }

    // --- literals / arithmetic / comparisons ---

    #[test]
    fn literals_and_arithmetic() {
        assert_eq!(eval("1 + 2 * 3").unwrap(), json!(7));
        assert_eq!(eval("10 % 3").unwrap(), json!(1));
        assert_eq!(eval("1.5 * 2.0").unwrap(), json!(3.0));
        assert_eq!(eval("\"a\" + \"b\"").unwrap(), json!("ab"));
        assert_eq!(eval("[1, 2] + [3]").unwrap(), json!([1, 2, 3]));
        assert_eq!(eval("null").unwrap(), json!(null));
    }

    #[test]
    fn comparisons_and_ternary() {
        assert_eq!(eval("2 > 1").unwrap(), json!(true));
        assert_eq!(eval("2 == 3").unwrap(), json!(false));
        assert_eq!(eval("\"a\" != \"b\"").unwrap(), json!(true));
        assert_eq!(eval("true ? 1 : 2").unwrap(), json!(1));
        assert_eq!(eval("1 in [1, 2]").unwrap(), json!(true));
    }

    #[test]
    fn comprehension_macros() {
        assert_eq!(eval("[1, 2, 3].map(x, x * 2)").unwrap(), json!([2, 4, 6]));
        assert_eq!(eval("[1, 2, 3].filter(x, x > 1)").unwrap(), json!([2, 3]));
        assert_eq!(eval("[1, 2, 3].all(x, x > 0)").unwrap(), json!(true));
        assert_eq!(eval("size([1, 2, 3])").unwrap(), json!(3));
    }

    #[test]
    fn division_by_zero_is_error() {
        let err = eval("1 / 0").unwrap_err();
        assert!(err.cause.contains("evaluation failed"), "{}", err.cause);
    }

    // --- vars (incl. nested json) ---

    #[test]
    fn vars_access_text_and_nested_json() {
        let mut fx = Fixture::new();
        fx.vars.insert("name".into(), FlowValue::Text("zed".into()));
        fx.vars.insert(
            "x".into(),
            FlowValue::Json(json!({"a": {"b": [10, 20, 30]}})),
        );
        let scope = fx.scope(&[]);
        assert_eq!(evaluate("vars.name", &scope, None).unwrap(), json!("zed"));
        assert_eq!(evaluate("vars.x.a.b[1]", &scope, None).unwrap(), json!(20));
        assert_eq!(
            evaluate("vars.name == \"zed\" && vars.x.a.b[0] == 10", &scope, None).unwrap(),
            json!(true)
        );
    }

    #[test]
    fn vars_harness_done_default_until_expression() {
        // The default `loop.until` from §3.5 must work as-is.
        let mut fx = Fixture::new();
        fx.vars
            .insert("harness_done".into(), FlowValue::Json(json!(true)));
        assert!(evaluate_bool("vars.harness_done == true", &fx.scope(&[]), None).unwrap());
    }

    #[test]
    fn vars_embedding_exposed_as_float_array() {
        let mut fx = Fixture::new();
        fx.vars
            .insert("emb".into(), FlowValue::Embedding(vec![0.5, 1.5]));
        let scope = fx.scope(&[]);
        assert_eq!(evaluate("vars.emb[1]", &scope, None).unwrap(), json!(1.5));
        assert_eq!(evaluate("size(vars.emb)", &scope, None).unwrap(), json!(2));
    }

    // --- payload ---

    #[test]
    fn payload_as_text() {
        let mut fx = Fixture::new();
        fx.payload = FlowValue::Text("hello".into());
        let scope = fx.scope(&[]);
        assert_eq!(evaluate("payload", &scope, None).unwrap(), json!("hello"));
        assert_eq!(evaluate("size(payload)", &scope, None).unwrap(), json!(5));
        assert!(evaluate_bool("payload.startsWith(\"he\")", &scope, None).unwrap());
    }

    #[test]
    fn payload_as_json() {
        let mut fx = Fixture::new();
        fx.payload = FlowValue::Json(json!({"items": [{"id": 7}], "ok": true}));
        let scope = fx.scope(&[]);
        assert_eq!(evaluate("payload.ok", &scope, None).unwrap(), json!(true));
        assert_eq!(
            evaluate("payload.items[0].id", &scope, None).unwrap(),
            json!(7)
        );
        // `map.items` usage from §3.5: an expression selecting an array.
        assert_eq!(
            evaluate("payload.items", &scope, None).unwrap(),
            json!([{"id": 7}])
        );
    }

    #[test]
    fn payload_empty_is_null() {
        let fx = Fixture::new();
        assert_eq!(
            evaluate("payload", &fx.scope(&[]), None).unwrap(),
            json!(null)
        );
        assert!(evaluate_bool("payload == null", &fx.scope(&[]), None).unwrap());
    }

    #[test]
    fn payload_blob_exposes_descriptor_only() {
        let mut fx = Fixture::new();
        fx.payload = FlowValue::Audio {
            blob_ref: blob(42, "audio/wav"),
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
        };
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("payload", &scope, None).unwrap(),
            json!({"kind": "audio", "mime": "audio/wav", "size_bytes": 42})
        );
        // Descriptor numbers bind as Int, so plain integer literals compare.
        assert!(evaluate_bool("payload.size_bytes == 42", &scope, None).unwrap());
        // The descriptor must not leak blob internals (id / sha256 / bytes).
        assert!(evaluate("payload.id", &scope, None).is_err());
        assert!(evaluate("payload.sha256", &scope, None).is_err());
    }

    #[test]
    fn artifact_blob_variants_use_snake_case_kind() {
        let mut fx = Fixture::new();
        fx.artifacts.insert(
            "frame".into(),
            FlowValue::Image {
                blob_ref: blob(100, "image/png"),
                mime: "image/png".into(),
                dims: Some((640, 480)),
            },
        );
        fx.artifacts.insert(
            "clip".into(),
            FlowValue::Video {
                blob_ref: blob(200, "video/mp4"),
                mime: "video/mp4".into(),
                duration_ms: Some(1000),
            },
        );
        fx.artifacts.insert(
            "doc".into(),
            FlowValue::Other {
                blob_ref: blob(300, "application/pdf"),
                mime: "application/pdf".into(),
                filename: Some("a.pdf".into()),
            },
        );
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("artifacts.frame.kind", &scope, None).unwrap(),
            json!("image")
        );
        assert_eq!(
            evaluate("artifacts.clip.kind", &scope, None).unwrap(),
            json!("video")
        );
        assert_eq!(
            evaluate("artifacts.doc.kind", &scope, None).unwrap(),
            json!("other")
        );
        assert_eq!(
            evaluate("artifacts.doc.mime", &scope, None).unwrap(),
            json!("application/pdf")
        );
    }

    // --- artifacts / meta ---

    #[test]
    fn artifacts_lookup_by_field_and_index() {
        let mut fx = Fixture::new();
        fx.artifacts
            .insert("transcript".into(), FlowValue::Text("hi".into()));
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("artifacts.transcript", &scope, None).unwrap(),
            json!("hi")
        );
        assert_eq!(
            evaluate("artifacts[\"transcript\"]", &scope, None).unwrap(),
            json!("hi")
        );
    }

    #[test]
    fn meta_is_visible_read_only_data() {
        let mut fx = Fixture::new();
        fx.meta.insert("run_id".into(), json!("abc"));
        fx.meta.insert("attempt".into(), json!(2));
        let scope = fx.scope(&[]);
        assert!(evaluate_bool("meta.run_id == \"abc\"", &scope, None).unwrap());
        assert_eq!(
            evaluate("meta.attempt + 1", &scope, None).unwrap(),
            json!(3)
        );
    }

    // --- extras (loop/map locals) ---

    #[test]
    fn extras_bound_as_top_level_variables() {
        let fx = Fixture::new();
        let extras = [
            ("item", json!({"id": 7})),
            ("index", json!(3)),
            ("iteration", json!(1)),
        ];
        let scope = fx.scope(&extras);
        assert_eq!(
            evaluate("item.id + index", &scope, None).unwrap(),
            json!(10)
        );
        assert!(evaluate_bool("iteration == 1", &scope, None).unwrap());
        assert!(evaluate_bool("index < 5 && item.id > 0", &scope, None).unwrap());
    }

    #[test]
    fn extras_shadow_base_bindings() {
        let mut fx = Fixture::new();
        fx.payload = FlowValue::Text("original".into());
        let extras = [("payload", json!("shadowed"))];
        assert_eq!(
            evaluate("payload", &fx.scope(&extras), None).unwrap(),
            json!("shadowed")
        );
    }

    // --- errors, not panics ---

    #[test]
    fn missing_map_key_is_error() {
        let mut fx = Fixture::new();
        fx.vars.insert("x".into(), FlowValue::Json(json!({"a": 1})));
        let scope = fx.scope(&[]);
        let err = evaluate("vars.nope", &scope, None).unwrap_err();
        assert_eq!(err.expression, "vars.nope");
        assert!(!err.cause.is_empty());
        let err = evaluate("vars.x.missing.deeper", &scope, None).unwrap_err();
        assert_eq!(err.expression, "vars.x.missing.deeper");
    }

    #[test]
    fn undeclared_variable_is_error() {
        let err = eval("bogus + 1").unwrap_err();
        assert!(err.cause.contains("bogus"), "{}", err.cause);
    }

    #[test]
    fn has_macro_probes_missing_keys() {
        let mut fx = Fixture::new();
        fx.vars.insert("x".into(), FlowValue::Json(json!({"a": 1})));
        let scope = fx.scope(&[]);
        assert!(evaluate_bool("has(vars.x)", &scope, None).unwrap());
        assert!(!evaluate_bool("has(vars.nope)", &scope, None).unwrap());
    }

    #[test]
    fn type_mismatch_is_error() {
        let err = eval("1 + \"a\"").unwrap_err();
        assert!(!err.cause.is_empty());
        assert!(eval("[1] + 2").is_err());
        assert!(eval("{\"a\": 1} + {\"b\": 2}").is_err());
    }

    #[test]
    fn parse_error_carries_expression() {
        let err = eval("1 +").unwrap_err();
        assert_eq!(err.expression, "1 +");
        assert!(err.cause.contains("parse error"), "{}", err.cause);
        // Display ties expression and cause together for node messages.
        let display = err.to_string();
        assert!(display.contains("1 +") && display.contains("parse error"));
    }

    // --- evaluate_bool strictness ---

    #[test]
    fn evaluate_bool_accepts_only_bool() {
        let fx = Fixture::new();
        let scope = fx.scope(&[]);
        assert!(evaluate_bool("2 > 1", &scope, None).unwrap());
        assert!(!evaluate_bool("false", &scope, None).unwrap());
        for non_bool in ["1 + 1", "\"true\"", "null", "[true]", "1.0"] {
            let err = evaluate_bool(non_bool, &scope, None).unwrap_err();
            assert!(
                err.cause.contains("expected bool"),
                "{}: {}",
                non_bool,
                err.cause
            );
        }
    }

    // --- limits ---

    #[test]
    fn empty_expression_is_error() {
        assert!(eval("").is_err());
        assert!(eval("   \t\n").is_err());
    }

    #[test]
    fn length_cap_enforced_at_boundary() {
        // Exactly at the cap: padding + "1 + 1" = MAX_EXPR_CHARS chars, valid.
        let at_cap = format!("{}1 + 1", " ".repeat(MAX_EXPR_CHARS - 5));
        assert_eq!(at_cap.chars().count(), MAX_EXPR_CHARS);
        assert_eq!(eval(&at_cap).unwrap(), json!(2));

        let over_cap = format!("{}1 + 1", " ".repeat(MAX_EXPR_CHARS - 4));
        let err = eval(&over_cap).unwrap_err();
        assert!(err.cause.contains("character limit"), "{}", err.cause);
    }

    #[test]
    fn oversized_expression_is_truncated_in_error() {
        let huge = "x".repeat(1_000_000);
        let err = eval(&huge).unwrap_err();
        assert!(err.expression.chars().count() <= MAX_EXPR_CHARS + 1);
        assert!(err.expression.ends_with('…'));
    }

    // --- evaluation budget ---

    #[test]
    fn evaluation_deadline_bounds_runaway_comprehensions() {
        let mut fx = Fixture::new();
        let items: Vec<u64> = (0..2_000).collect();
        fx.payload = FlowValue::Json(json!({ "items": items }));
        let scope = fx.scope(&[]);
        // Three nested `.all` comprehensions = 8e9 interpreter steps with no
        // result-list allocation, so the detached runaway worker burns CPU
        // but not memory for the remainder of the test run.
        let expr = "payload.items.all(x, payload.items.all(y, \
                    payload.items.all(z, x + y + z >= 0)))";
        let budget = Duration::from_millis(100);
        let start = Instant::now();
        let err = evaluate(expr, &scope, Some(budget)).unwrap_err();
        let elapsed = start.elapsed();
        assert!(
            err.cause.contains("evaluation deadline exceeded"),
            "{}",
            err.cause
        );
        assert!(
            elapsed < budget * 2,
            "took {elapsed:?} for budget {budget:?}"
        );
    }

    // --- hostile inputs: errors, never panics or hangs ---

    #[test]
    fn deeply_nested_brackets_hit_nesting_cap() {
        // List/map literal recursion in the cel parser is unguarded (it would
        // stack-overflow instead of erroring on deep input), so the pre-parse
        // nesting cap must reject all bracket kinds before the parser runs.
        // Depth 60 exceeds the nesting cap while every shape stays under the
        // length cap, so this pins which guard fires.
        for (open, close) in [("(", ")"), ("[", "]"), ("{\"a\": ", "}")] {
            let depth = 60;
            let expr = format!("{}1{}", open.repeat(depth), close.repeat(depth));
            assert!(expr.chars().count() <= MAX_EXPR_CHARS);
            let err = eval(&expr).unwrap_err();
            assert!(err.cause.contains("nesting depth"), "{}", err.cause);
        }
    }

    #[test]
    fn nesting_cap_boundary() {
        // Sane nesting must still parse; one past the cap must error.
        let ok = format!(
            "{}1{}",
            "(".repeat(MAX_EXPR_NESTING),
            ")".repeat(MAX_EXPR_NESTING)
        );
        assert_eq!(eval(&ok).unwrap(), json!(1));
        let over = format!(
            "{}1{}",
            "(".repeat(MAX_EXPR_NESTING + 1),
            ")".repeat(MAX_EXPR_NESTING + 1)
        );
        let err = eval(&over).unwrap_err();
        assert!(err.cause.contains("nesting depth"), "{}", err.cause);
        // Brackets inside string literals count too — the documented
        // false-reject trade-off, called out in the error message.
        let in_string = format!("size(\"{}\") > 0", "[".repeat(MAX_EXPR_NESTING + 1));
        let err = eval(&in_string).unwrap_err();
        assert!(err.cause.contains("string literals"), "{}", err.cause);
    }

    #[test]
    fn nested_list_literals_within_cap_evaluate() {
        let depth = 10;
        let expr = format!(
            "{}1{}{}",
            "[".repeat(depth),
            "]".repeat(depth),
            "[0]".repeat(depth - 1)
        );
        // Ten nested singleton lists indexed back down to the scalar.
        assert_eq!(eval(&expr).unwrap(), json!([1]));
    }

    #[test]
    fn bracketless_deep_recursion_is_contained() {
        // These shapes carry no brackets, so they probe the parser directly
        // at full MAX_EXPR_CHARS on the 16 MB worker stack. The pinned
        // outcomes matter less than the absence of an abort — any abort here
        // means the pre-parse guard must grow to cover the offending shape.
        //
        // Right-recursive ternary chain: each `?` re-enters the parser's
        // `expr` rule, and the crate's own depth limit panics on a u16
        // underflow while rejecting that shape (cel 0.13 parser.rs:405) —
        // the `?`-aware nesting cap must fire first.
        let unit = "true ? 1 : ";
        let ternary = format!("{}0", unit.repeat((MAX_EXPR_CHARS - 1) / unit.len()));
        assert!(ternary.chars().count() <= MAX_EXPR_CHARS);
        let err = eval(&ternary).unwrap_err();
        assert!(err.cause.contains("nesting depth"), "{}", err.cause);
        // A chain at the cap must stay usable — the cap rejects pathology,
        // not real ternaries.
        let sane = format!("{}0", unit.repeat(MAX_EXPR_NESTING));
        assert_eq!(eval(&sane).unwrap(), json!(1));

        // Unary-operator repetition is consumed by grammar loops (`('!')+`),
        // not recursion. cel 0.13 collapses ANY repetition count to a single
        // application (its even-count fold discards the result — a broken
        // port of cel-go), so N `!` negate once and N `-` negate once;
        // pinned, like the size() deviation, so an upstream fix is noticed.
        let bangs = format!("{}true", "!".repeat(MAX_EXPR_CHARS - 4));
        assert_eq!(eval(&bangs).unwrap(), json!(false));
        let negs = format!("{}1", "-".repeat(MAX_EXPR_CHARS - 1));
        assert_eq!(eval(&negs).unwrap(), json!(-1));

        // Operator chains nest the parse tree one level per link, which is
        // what bounds MAX_EXPR_CHARS: the measured overflow cliff is between
        // 383 and 511 levels on a 16 MB stack (see the constant). Both chain
        // shapes at full length must stay parseable on the worker stack.
        let members = format!("a{}", ".a".repeat((MAX_EXPR_CHARS - 1) / 2));
        assert!(members.chars().count() <= MAX_EXPR_CHARS);
        // `a` is undeclared, so evaluation errors — after a safe parse.
        assert!(eval(&members).is_err());
        let adds = format!("1{}", "+1".repeat((MAX_EXPR_CHARS - 1) / 2));
        assert!(adds.chars().count() <= MAX_EXPR_CHARS);
        assert_eq!(eval(&adds).unwrap(), json!(1 + (MAX_EXPR_CHARS - 1) / 2));
    }

    #[test]
    fn deeply_nested_scope_json_round_trips() {
        // serde_json's own wire parser caps nesting at 128 — match that depth
        // to prove scope binding survives the worst wire-legal input.
        let mut deep = json!(1);
        for _ in 0..128 {
            deep = json!({ "a": deep });
        }
        let mut fx = Fixture::new();
        fx.vars.insert("deep".into(), FlowValue::Json(deep));
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("vars.deep.a.a.a.a", &scope, None).unwrap()["a"]["a"]["a"]["a"].is_object(),
            true
        );
        assert!(evaluate_bool("has(vars.deep.a)", &scope, None).unwrap());
    }

    #[test]
    fn weird_unicode_in_string_literals_and_identifiers() {
        assert_eq!(eval("\"żółć\" + \"💧\"").unwrap(), json!("żółć💧"));
        // cel-rust `size()` on strings counts BYTES, not code points (the CEL
        // spec says code points) — pinned here so an upstream fix is noticed.
        assert_eq!(eval("size(\"💧💧\")").unwrap(), json!(8));
        // Emoji are not valid CEL identifiers — parse error, not a panic.
        assert!(eval("💥 + 1").is_err());
        // Embedded NUL and RTL override in a literal must not break anything.
        assert!(eval("\"a\u{202e}b\" != \"ab\"").unwrap().as_bool().unwrap());
    }

    #[test]
    fn non_ascii_object_keys_index_by_string() {
        let mut fx = Fixture::new();
        fx.vars
            .insert("x".into(), FlowValue::Json(json!({"żółć": 7})));
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("vars.x[\"żółć\"]", &scope, None).unwrap(),
            json!(7)
        );
    }

    #[test]
    fn uint_scope_values_above_i64_max() {
        let mut fx = Fixture::new();
        fx.vars
            .insert("big".into(), FlowValue::Json(json!(u64::MAX)));
        let scope = fx.scope(&[]);
        // Round-trips through CEL UInt back to the exact JSON number.
        assert_eq!(evaluate("vars.big", &scope, None).unwrap(), json!(u64::MAX));
        // cel-rust has no Int/UInt cross-type equality: comparing a UInt
        // scope value against a plain int literal is quietly false — same
        // class of deviation as the size()-bytes pin above. A `u`-suffixed
        // uint literal compares correctly.
        assert!(!evaluate_bool("vars.big == 1", &scope, None).unwrap());
        assert!(evaluate_bool("vars.big == 18446744073709551615u", &scope, None).unwrap());
    }

    #[test]
    fn huge_scope_string_is_handled() {
        let mut fx = Fixture::new();
        fx.payload = FlowValue::Text("x".repeat(1_000_000));
        let scope = fx.scope(&[]);
        assert_eq!(
            evaluate("size(payload)", &scope, None).unwrap(),
            json!(1_000_000)
        );
        assert!(evaluate_bool("payload.contains(\"x\")", &scope, None).unwrap());
    }

    #[test]
    fn hostile_garbage_inputs_are_errors() {
        for garbage in [
            "}{",
            "((((",
            "....",
            "a.b.c(",
            "\\u0000",
            "0x",
            "1e999999999999",
            "'unterminated",
            "f(((((((((((((((((((((((((((((((((((((((((",
        ] {
            assert!(eval(garbage).is_err(), "expected error for: {garbage}");
        }
    }

    #[test]
    fn integer_overflow_is_error_not_panic() {
        assert!(eval("9223372036854775807 + 1").is_err());
        assert!(eval("9223372036854775807 * 2").is_err());
    }

    // --- cache behavior ---

    #[test]
    fn cache_overflow_clears_and_keeps_working() {
        let fx = Fixture::new();
        let scope = fx.scope(&[]);
        // Repeated evaluation of one expression exercises the cache-hit path.
        for _ in 0..3 {
            assert_eq!(evaluate("40 + 2", &scope, None).unwrap(), json!(42));
        }
        // More distinct expressions than PROGRAM_CACHE_CAP forces at least one
        // full clear; results must stay correct throughout.
        for i in 0..(PROGRAM_CACHE_CAP + 50) {
            let expr = format!("{i} + 1");
            assert_eq!(evaluate(&expr, &scope, None).unwrap(), json!(i + 1));
        }
        assert_eq!(evaluate("40 + 2", &scope, None).unwrap(), json!(42));
    }

    #[test]
    fn failed_compilation_is_not_cached_and_stays_failing() {
        assert!(eval("1 +").is_err());
        assert!(eval("1 +").is_err());
    }
}
