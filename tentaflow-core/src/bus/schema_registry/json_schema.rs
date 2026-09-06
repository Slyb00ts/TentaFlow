// =============================================================================
// File: bus/schema_registry/json_schema.rs — JSON Schema subset (F3)
// =============================================================================
// SUM/tentabus/PLAN-F3.md, owner decision 2: a HAND-WRITTEN SUBSET validator,
// zero new dependencies. Any keyword outside the supported subset is
// REJECTED AT COMPILE (registration) TIME, never silently ignored — a
// partial validator that skips keywords would be worse than none for a
// compliance-facing feature. This module has no shared IR with `payload_format`
// or `stored_only` — it owns its own compiled representation end to end.
//
// Supported subset (validated structurally, applied at `validate` time):
//   `type` (string or non-empty array), `properties`, `required` (unique
//   strings), `additionalProperties` (bool or schema), `minProperties`,
//   `maxProperties`, `items` (single schema only — tuple `items` arrays are
//   rejected), `minItems`, `maxItems`, `uniqueItems`, `enum` (non-empty),
//   `const`, `minimum`/`maximum`/`exclusiveMinimum`/`exclusiveMaximum`
//   (numeric only — the draft-04 boolean form is rejected), `multipleOf`
//   (> 0), `minLength`/`maxLength` (Unicode scalar values, not bytes),
//   `pattern` (compiled with `regex::RegexBuilder` + a ~1 MiB size limit at
//   registration time, a DoS guard per PLAN-F3 R2), `allOf`/`anyOf`/`oneOf`
//   (non-empty arrays), `not`, `dependentRequired`, `$defs`/`definitions`,
//   and ONLY LOCAL `$ref` (`#/$defs/<name>` or `#/definitions/<name>`,
//   resolved and cycle-checked at compile time — remote/relative/`$id`-based
//   refs are rejected, closing the SSRF surface named in PLAN-F3 R2).
//
// Annotation-only keywords are accepted, loosely type-checked, and otherwise
// ignored at validation time: `$schema`, `$id`, `title`, `description`,
// `$comment`, `examples`, `default`, `deprecated`, `readOnly`, `writeOnly`,
// and `format` (accepted and ignored — this build does not implement any
// `format` semantics, documented per the owner's subset list).
//
// Explicitly REJECTED (fall through to the generic "unsupported keyword"
// error): `if`/`then`/`else`, `patternProperties`, `propertyNames`,
// `contains`, `prefixItems`, `unevaluatedProperties`/`unevaluatedItems`,
// `$dynamicRef`/`$dynamicAnchor`/`$anchor`, `dependentSchemas`,
// `dependencies` (the draft-07 form), `contentEncoding`/`contentMediaType`,
// `minContains`/`maxContains`, tuple-form `items`.
//
// `$ref` is EXCLUSIVE: a schema object carrying `$ref` applies ONLY the
// resolved target's semantics — no sibling keyword is merged into
// validation, matching draft-07's ref-is-exclusive behavior (a deliberate
// simplification versus 2019-09+). Because a validating sibling next to
// `$ref` would then be silently inert, the owner's "nothing is silently
// ignored" rule is enforced here too: a `$ref` object may carry ONLY
// annotation-only siblings (`title`, `description`, `$comment`, `examples`,
// `default`, `deprecated`, `readOnly`, `writeOnly`) — any validating
// keyword (`type`, `properties`, `minLength`, ...) next to `$ref` is
// rejected at compile time with `SchemaError::Invalid`, naming the keyword
// and pointing at the referenced definition as the fix.
//
// `$ref` cycles (`#/$defs/a` -> `#/$defs/a`, directly or transitively) are
// rejected at compile time so `validate` never needs unbounded recursion; a
// combined structural/reference nesting depth of 64 is enforced for the
// same reason. Every `$defs`/`definitions` entry is validated at compile
// time even if never referenced by a `$ref` (a lazily-resolved-only def
// would let unsupported keywords hide in dead schema text).
//
// A root-level `$ref` may additionally carry `$defs`/`definitions` as
// siblings (ONLY at the schema root) — everywhere else `REF_ALLOWED_SIBLINGS`
// stays strict. `$defs`/`definitions` are pure lookup tables with no
// validating semantics of their own, so this does not weaken `$ref`
// exclusivity; without it a root-level `$ref` schema could never be
// registered at all, since its `$defs` would have nowhere else to live.
//
// Compilation is bounded against combinatorial `$ref`/combinator explosion:
// `$ref` targets are memoized per compiled schema, but `validate` still
// re-walks a resolved target on every occurrence (matching JSON Schema's own
// semantics — the same sub-schema can apply to different instance data at
// different points in `allOf`/`anyOf`/`oneOf`). A chain of `$defs` each
// referencing the previous one twice via `allOf` doubles the number of
// `validate_node` calls per level, so `compile` estimates that expansion
// (structural node count through `$ref`/`allOf`/`anyOf`/`oneOf`/`not`/
// `properties`/`items`, WITHOUT memoizing repeated subtrees — mirroring
// `validate`'s own unmemoized walk) and rejects the schema at registration
// time once the estimate exceeds `MAX_COMPILE_EXPANSION_ESTIMATE`. Because
// `properties`/`items` cardinality at validation time is driven by the
// PAYLOAD, not the schema, and is therefore invisible to that static
// estimate, `validate` additionally enforces a hard runtime step budget
// (`MAX_VALIDATION_STEPS`) via a counter threaded through every
// `validate_node` call: exhausting it fails the record closed with a
// `SchemaError::Violation`, never an unbounded hang. A schema/payload
// combination that needs more than that many steps to validate is
// considered unusable on the publish hot path by design.
//
// `validate` parses the payload once (`serde_json::from_slice`, unavoidable)
// and otherwise allocates only on the FAILURE path: JSON-pointer-ish error
// paths are built into a single reused `String` scratch buffer (push before
// recursing, truncate after) rather than per-call `format!` strings, and
// error messages are only formatted once a violation is confirmed. Violation
// messages are PATH + CONSTRAINT ONLY, never the offending payload value or
// string contents (e.g. matched/unmatched string text, a regex `pattern`'s
// text, a numeric magnitude) — these messages reach audit rows, warn-level
// logs, and DLQ headers, none of which are appropriate places for payload
// content to land.
//
// `uniqueItems` runs in O(n): each array item is canonicalized (numbers
// normalized to stay CONSISTENT with the `const`/`enum` numeric-equality
// rule below within +/-2^53 (the range that rule's own `f64` fallback tier
// is trustworthy over) — beyond that boundary the canonicalization also
// routes through `f64`, same as that fallback tier does, trading exactness
// for staying consistent with it rather than independently claiming exact
// integer precision `number_eq` itself does not always preserve; object keys
// EXPLICITLY sorted — this crate's `serde_json` has the `preserve_order`
// feature enabled transitively, so `Map` is an insertion-ordered `IndexMap`,
// not a `BTreeMap`, and relying on its iteration order would miss a
// duplicate whose object keys arrived in a different source order) into a
// `HashSet<String>` membership check, rather than the O(n^2) pairwise
// deep-equality comparison a naive implementation would use — the latter
// turns a large array of repeated values into a publish-time hang.
//
// Numeric equality for `const`/`enum`/general deep-equality compares a pair
// of `serde_json::Number`s by `as_i64`, then `as_u64`, then `as_f64`, in
// that order: two integer-typed numbers (any JSON literal syntax) compare
// exactly with no precision loss even above 2^53; only when neither side has
// an exact integer representation (a JSON float literal, or a magnitude
// beyond `u64::MAX`) do both fall back to `f64`, which is only trustworthy
// for integral values within +/-2^53 — an intrinsic property of that
// fallback tier, not a separately enforced bound. `uniqueItems`
// canonicalization (`canonical_number`) cannot replicate that PAIRWISE
// tiering on its own (it sees one value at a time, not the pair being
// compared), so above +/-2^53 it always takes the `f64` tier — meaning two
// distinct huge integer literals that happen to round to the same `f64` are
// treated as duplicates by `uniqueItems`, matching what `number_eq` would
// conclude if either of them were instead a float literal.
// `multipleOf` uses the same integer-exactness split: an integer value
// divided by an integer divisor is checked with exact modulo arithmetic;
// otherwise the ratio is checked against a relative epsilon, but a ratio
// whose magnitude exceeds 2^53 (where a `f64` has no fractional bits left to
// compare at all) or that is non-finite is treated as an unverifiable
// violation rather than silently accepted.
//
// `check_compatibility` compares the ROOT `properties`/`required`/
// `additionalProperties`/`type` level structurally, with per-shared-property
// type-set widening (`integer` treated as a subset of `number`); any other
// difference in a shared property's subschema (deep JSON inequality after
// stripping `type`) is `Incompatible`. Per owner decision 2 ("nothing
// outside the supported subset may be silently ignored"), every OTHER root
// keyword — `minProperties`/`maxProperties`, `allOf`/`anyOf`/`oneOf`/`not`,
// `dependentRequired`, `enum`, `const`, and any future addition to
// `SUPPORTED_KEYWORDS` — must be byte-for-byte deep-equal between old and
// new, EXCEPT the annotation-only keywords (`$schema`, `$id`, `title`,
// `description`, `$comment`, `examples`, `default`, `deprecated`,
// `readOnly`, `writeOnly`), which never affect validation. `$defs`/
// `definitions` are handled separately and more narrowly, by
// `check_reachable_defs_are_unchanged`: only entries transitively reachable
// (via `$ref`) from the OLD schema's `properties` must stay deep-equal —
// closing the same `$ref`-mediated bypass (`properties.x = {"$ref":
// "#/$defs/Id"}` looking unchanged while `$defs.Id` itself changed
// underneath it) without also rejecting a purely additive change that
// happens to introduce a brand-new, previously-unreferenced definition (see
// that function's doc comment). Schema-form `additionalProperties` (an
// object, not a boolean) is likewise required to be deep-equal on both
// sides; the existing boolean-only widening rule (`false` -> `true`) is
// unaffected. This is conservative and
// fail-closed by design, not a full recursive compatibility check.
// =============================================================================

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use regex::{Regex, RegexBuilder};
use serde_json::{Map, Number, Value};

use super::{Compatibility, CompiledSchema, SchemaError, SchemaKindOps, MAX_SCHEMA_TEXT_BYTES};

/// Combined structural-nesting / `$ref`-chain-depth cap. Bounds both
/// pathologically deep schema trees and long (non-cyclic) `$ref` chains so
/// compilation never blows the stack — an admin-supplied schema is a DoS
/// surface (PLAN-F3 R2).
const MAX_SCHEMA_DEPTH: u32 = 64;

const SIZE_LIMIT_BYTES: usize = 1 << 20; // ~1 MiB compiled-pattern bound.

/// Compile-time cap on the estimated number of `validate_node` calls a
/// single `validate` invocation against this schema could perform, computed
/// WITHOUT memoizing repeated `$ref` targets (see `estimate_expansion_cost`
/// and the module header) — catches a combinatorial `allOf`/`anyOf`/`oneOf`
/// chain of `$ref`s at registration time, before it ever reaches the publish
/// hot path.
const MAX_COMPILE_EXPANSION_ESTIMATE: u64 = 100_000;

/// Hard runtime cap on the number of `validate_node` calls a single
/// `validate` invocation may perform, threaded through as a `&mut u32`
/// counter. Guards the case `MAX_COMPILE_EXPANSION_ESTIMATE` cannot see:
/// `properties`/`items` fan-out driven by the PAYLOAD (an admin cannot know
/// the size of a future record when registering a schema). See the module
/// header.
const MAX_VALIDATION_STEPS: u32 = 200_000;

/// Largest integer value a `f64` can represent exactly (2^53). Used as the
/// trust boundary for the `as_f64` fallback tier of numeric-equality and
/// `multipleOf` comparisons — see the module header.
const MAX_EXACT_INTEGER_F64: f64 = 9_007_199_254_740_992.0;

/// Same value as `MAX_EXACT_INTEGER_F64`, kept as a `u64` so
/// `canonical_number` can compare an integer literal's magnitude against it
/// with EXACT integer arithmetic. Casting the integer to `f64` first (as an
/// earlier version of this function did) would round a magnitude just past
/// this boundary DOWN to exactly `MAX_EXACT_INTEGER_F64` (e.g.
/// `9_007_199_254_740_993_u64 as f64 == 9_007_199_254_740_992.0`), making
/// the `<=` boundary check itself lossy and silently wrong right at the
/// boundary it exists to protect.
const MAX_EXACT_INTEGER_U64: u64 = 9_007_199_254_740_992;

const SUPPORTED_KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "minProperties",
    "maxProperties",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "enum",
    "const",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "allOf",
    "anyOf",
    "oneOf",
    "not",
    "dependentRequired",
    "$ref",
    "$defs",
    "definitions",
    "$schema",
    "$id",
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
    "format",
];

/// Keywords a `$ref` object may carry alongside `$ref` itself. `$ref` is
/// exclusive (see module header) — any keyword NOT in this list would be
/// silently inert next to `$ref`, which the owner's "nothing is silently
/// ignored" rule forbids, so `compile_node` rejects it outright instead.
/// `compile_node` additionally allows `$defs`/`definitions` here, but ONLY
/// at the schema root (see the module header) — kept out of this shared
/// list since it is not a general `$ref`-sibling rule.
const REF_ALLOWED_SIBLINGS: &[&str] = &[
    "$ref",
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
];

const SUPPORTED_DRAFTS: &[&str] = &[
    "http://json-schema.org/draft-07/schema#",
    "http://json-schema.org/draft-07/schema",
    "https://json-schema.org/draft-07/schema#",
    "https://json-schema.org/draft-07/schema",
    "https://json-schema.org/draft/2019-09/schema#",
    "https://json-schema.org/draft/2019-09/schema",
    "https://json-schema.org/draft/2020-12/schema#",
    "https://json-schema.org/draft/2020-12/schema",
];

// -----------------------------------------------------------------------
// Compiled representation
// -----------------------------------------------------------------------

/// Compiled JSON Schema (subset). Opaque outside this module. This is the
/// form held long-lived in the publish-path validator cache, so it carries
/// ONLY the validation-oriented IR — no copy of the raw parsed document.
/// `check_compatibility`/`derive_subschema` need the raw JSON shape too, but
/// they always operate on caller-supplied schema text directly (never on a
/// cached `Compiled` instance, see `registry::register`/`set_compatibility`)
/// and get it via `compile_schema_text`, not through this struct.
#[derive(Debug)]
pub struct Compiled {
    schema: SchemaNode,
}

#[derive(Debug, Clone)]
enum SchemaNode {
    Bool(bool),
    Object(Arc<ObjectSchema>),
}

#[derive(Debug)]
struct ObjectSchema {
    types: Option<Vec<JsonType>>,
    properties: BTreeMap<String, SchemaNode>,
    required: BTreeSet<String>,
    additional_properties: AdditionalProperties,
    min_properties: Option<u64>,
    max_properties: Option<u64>,
    items: Option<Box<SchemaNode>>,
    min_items: Option<u64>,
    max_items: Option<u64>,
    unique_items: bool,
    enum_values: Option<Vec<Value>>,
    const_value: Option<Value>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    exclusive_minimum: Option<f64>,
    exclusive_maximum: Option<f64>,
    /// Kept as the original `serde_json::Number` (not `f64`) so `validate`
    /// can distinguish an exact integer divisor from a float one — see
    /// `multipleOf` handling in `validate_object_schema` and the module
    /// header.
    multiple_of: Option<Number>,
    min_length: Option<u64>,
    max_length: Option<u64>,
    pattern: Option<Regex>,
    all_of: Vec<SchemaNode>,
    any_of: Vec<SchemaNode>,
    one_of: Vec<SchemaNode>,
    not: Option<Box<SchemaNode>>,
    dependent_required: BTreeMap<String, Vec<String>>,
}

impl Default for ObjectSchema {
    fn default() -> Self {
        ObjectSchema {
            types: None,
            properties: BTreeMap::new(),
            required: BTreeSet::new(),
            additional_properties: AdditionalProperties::Allowed,
            min_properties: None,
            max_properties: None,
            items: None,
            min_items: None,
            max_items: None,
            unique_items: false,
            enum_values: None,
            const_value: None,
            minimum: None,
            maximum: None,
            exclusive_minimum: None,
            exclusive_maximum: None,
            multiple_of: None,
            min_length: None,
            max_length: None,
            pattern: None,
            all_of: Vec::new(),
            any_of: Vec::new(),
            one_of: Vec::new(),
            not: None,
            dependent_required: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
enum AdditionalProperties {
    /// Absent or `true`.
    Allowed,
    /// `false`.
    Denied,
    /// A schema form — additional properties are validated against it.
    Schema(Box<SchemaNode>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonType {
    Object,
    Array,
    String,
    Number,
    Integer,
    Boolean,
    Null,
}

impl JsonType {
    fn parse(s: &str) -> Option<JsonType> {
        match s {
            "object" => Some(JsonType::Object),
            "array" => Some(JsonType::Array),
            "string" => Some(JsonType::String),
            "number" => Some(JsonType::Number),
            "integer" => Some(JsonType::Integer),
            "boolean" => Some(JsonType::Boolean),
            "null" => Some(JsonType::Null),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            JsonType::Object => "object",
            JsonType::Array => "array",
            JsonType::String => "string",
            JsonType::Number => "number",
            JsonType::Integer => "integer",
            JsonType::Boolean => "boolean",
            JsonType::Null => "null",
        }
    }

    fn matches(self, value: &Value) -> bool {
        match self {
            JsonType::Object => value.is_object(),
            JsonType::Array => value.is_array(),
            JsonType::String => value.is_string(),
            JsonType::Number => value.is_number(),
            JsonType::Integer => is_integer_value(value),
            JsonType::Boolean => value.is_boolean(),
            JsonType::Null => value.is_null(),
        }
    }
}

fn is_integer_value(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                true
            } else {
                n.as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0)
            }
        }
        _ => false,
    }
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// -----------------------------------------------------------------------
// Compile
// -----------------------------------------------------------------------

/// Compile-time context: raw (name -> definition) references for
/// `$defs`/`definitions` at the schema root, memoized compiled results
/// keyed the same way, and the set of keys currently being compiled (for
/// `$ref` cycle detection).
struct Ctx<'a> {
    defs: BTreeMap<String, &'a Value>,
    /// Memoized compiled `$ref` targets, keyed `"$defs/<name>"` /
    /// `"definitions/<name>"`, alongside each entry's HEIGHT: the maximum
    /// depth reached anywhere inside it (structurally, or transitively
    /// through its own `$ref`s) MINUS the depth its own root was compiled
    /// at — i.e. how much additional depth budget the subtree consumes
    /// beyond wherever it is rooted. A cache hit skips re-walking the
    /// target, so `compile_node`'s own `depth > MAX_SCHEMA_DEPTH` check
    /// never runs for anything inside it a second time; `resolve_ref`
    /// re-derives an equivalent bound from the stored height instead —
    /// without it, a `$ref` first resolved shallowly and reused much
    /// deeper elsewhere could push its own descendants past
    /// `MAX_SCHEMA_DEPTH` with no check ever catching it (finding 2).
    cache: HashMap<String, (SchemaNode, u32)>,
    compiling: HashSet<String>,
}

fn ptr(path: &str) -> &str {
    if path.is_empty() {
        "<root>"
    } else {
        path
    }
}

fn inv(path: &str, msg: &str) -> SchemaError {
    SchemaError::Invalid(format!("{}: {msg}", ptr(path)))
}

fn child_path(path: &str, segment: &str) -> String {
    format!("{path}/{segment}")
}

fn validate_schema_draft(map: &Map<String, Value>) -> Result<(), SchemaError> {
    let Some(v) = map.get("$schema") else {
        return Ok(());
    };
    let s = v
        .as_str()
        .ok_or_else(|| inv("", "'$schema' must be a string"))?;
    if !SUPPORTED_DRAFTS.contains(&s) {
        return Err(inv(
            "",
            &format!("unsupported '$schema' draft '{s}' (supported: draft-07, 2019-09, 2020-12)"),
        ));
    }
    Ok(())
}

fn collect_root_defs<'a>(
    map: &'a Map<String, Value>,
    keyword: &'static str,
    ctx: &mut Ctx<'a>,
) -> Result<(), SchemaError> {
    let Some(v) = map.get(keyword) else {
        return Ok(());
    };
    let obj = v
        .as_object()
        .ok_or_else(|| inv("", &format!("'{keyword}' must be an object")))?;
    for (name, sub) in obj {
        ctx.defs.insert(format!("{keyword}/{name}"), sub);
    }
    Ok(())
}

fn parse_local_ref(r: &str) -> Option<(&'static str, String)> {
    if let Some(name) = r.strip_prefix("#/$defs/") {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        Some(("$defs", name.to_string()))
    } else if let Some(name) = r.strip_prefix("#/definitions/") {
        if name.is_empty() || name.contains('/') {
            return None;
        }
        Some(("definitions", name.to_string()))
    } else {
        None
    }
}

/// Compiles the raw JSON body `def_value` of a `$defs`/`definitions` entry
/// `key` fresh (cache miss), tracking the maximum absolute depth reached
/// anywhere within it via a dedicated LOCAL `max_depth` accumulator (seeded
/// at `compiled_depth`, the depth its own root is compiled at), then caches
/// the result alongside its HEIGHT (`max_depth` reached minus
/// `compiled_depth`) — see `Ctx::cache`'s doc comment. Shared by
/// `resolve_ref` (cache miss path) and `compile_schema_text`'s coverage pass
/// (unreferenced `$defs`/`definitions` entries), so both populate the cache
/// with the same height accounting.
fn compile_and_cache_def<'a>(
    key: String,
    def_value: &'a Value,
    ctx: &mut Ctx<'a>,
    compiled_depth: u32,
) -> Result<(SchemaNode, u32), SchemaError> {
    ctx.compiling.insert(key.clone());
    let mut max_depth = compiled_depth;
    let result = compile_node(
        def_value,
        &format!("/{key}"),
        ctx,
        compiled_depth,
        &mut max_depth,
    );
    ctx.compiling.remove(&key);
    let node = result?;
    let height = max_depth.saturating_sub(compiled_depth);
    ctx.cache.insert(key, (node.clone(), height));
    Ok((node, height))
}

fn resolve_ref<'a>(
    r: &str,
    path: &str,
    ctx: &mut Ctx<'a>,
    depth: u32,
    max_depth: &mut u32,
) -> Result<SchemaNode, SchemaError> {
    let Some((container, name)) = parse_local_ref(r) else {
        return Err(inv(
            path,
            &format!(
                "unsupported '$ref' target '{r}' (only '#/$defs/<name>' and \
                 '#/definitions/<name>' local references are supported)"
            ),
        ));
    };
    let key = format!("{container}/{name}");
    if let Some((cached, cached_height)) = ctx.cache.get(&key) {
        // The cache hit bypasses `compile_node`'s own depth check for
        // everything inside the cached subtree, so re-derive an equivalent
        // bound here from the subtree's stored HEIGHT: if reused here, its
        // root would sit at `depth + 1`, so the worst absolute depth any
        // node inside it could now reach is `depth + 1 + cached_height`.
        let worst_case_depth = (depth + 1).saturating_add(*cached_height);
        if worst_case_depth > MAX_SCHEMA_DEPTH {
            return Err(inv(
                path,
                &format!(
                    "schema nesting/`$ref` chain exceeds the maximum depth of {MAX_SCHEMA_DEPTH}"
                ),
            ));
        }
        *max_depth = (*max_depth).max(worst_case_depth);
        return Ok(cached.clone());
    }
    if ctx.compiling.contains(&key) {
        return Err(inv(path, &format!("'$ref' cycle detected at '#/{key}'")));
    }
    let Some(def_value) = ctx.defs.get(&key).copied() else {
        return Err(inv(
            path,
            &format!("'$ref' target '#/{key}' does not resolve to an existing definition"),
        ));
    };
    let compiled_depth = depth + 1;
    let (node, height) = compile_and_cache_def(key, def_value, ctx, compiled_depth)?;
    *max_depth = (*max_depth).max(compiled_depth + height);
    Ok(node)
}

fn compile_node<'a>(
    value: &Value,
    path: &str,
    ctx: &mut Ctx<'a>,
    depth: u32,
    max_depth: &mut u32,
) -> Result<SchemaNode, SchemaError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(inv(
            path,
            &format!("schema nesting/`$ref` chain exceeds the maximum depth of {MAX_SCHEMA_DEPTH}"),
        ));
    }
    *max_depth = (*max_depth).max(depth);
    match value {
        Value::Bool(b) => Ok(SchemaNode::Bool(*b)),
        Value::Object(map) => {
            for key in map.keys() {
                if !SUPPORTED_KEYWORDS.contains(&key.as_str()) {
                    return Err(inv(path, &format!("unsupported keyword '{key}'")));
                }
            }
            if let Some(reference) = map.get("$ref") {
                // `$ref` is exclusive (see module header): a validating
                // sibling would be silently inert, so only annotation-only
                // siblings are allowed here — anything else is rejected
                // outright rather than ignored. `$defs`/`definitions` are
                // additionally allowed AT THE ROOT ONLY: they carry no
                // validating semantics of their own (they are a lookup
                // table, never applied directly to the instance), and
                // without this a root-level `$ref` schema could never be
                // registered at all, since its `$defs` would have nowhere
                // else to live.
                for key in map.keys() {
                    let allowed_here = REF_ALLOWED_SIBLINGS.contains(&key.as_str())
                        || (path.is_empty() && matches!(key.as_str(), "$defs" | "definitions"));
                    if !allowed_here {
                        return Err(SchemaError::Invalid(format!(
                            "keyword '{key}' next to '$ref' at {} is not supported (put it in \
                             the referenced definition)",
                            ptr(path)
                        )));
                    }
                }
                validate_annotation_keywords(map, path)?;
                let r = reference
                    .as_str()
                    .ok_or_else(|| inv(path, "'$ref' must be a string"))?;
                return resolve_ref(r, path, ctx, depth, max_depth);
            }
            Ok(SchemaNode::Object(Arc::new(compile_object(
                map, path, ctx, depth, max_depth,
            )?)))
        }
        other => Err(inv(
            path,
            &format!(
                "schema must be an object or a boolean, got {}",
                value_type_name(other)
            ),
        )),
    }
}

fn parse_type_keyword(v: &Value, path: &str) -> Result<Vec<JsonType>, SchemaError> {
    match v {
        Value::String(s) => JsonType::parse(s)
            .map(|t| vec![t])
            .ok_or_else(|| inv(path, &format!("unknown type '{s}'"))),
        Value::Array(arr) => {
            if arr.is_empty() {
                return Err(inv(path, "'type' array must not be empty"));
            }
            arr.iter()
                .map(|item| {
                    let s = item
                        .as_str()
                        .ok_or_else(|| inv(path, "'type' array items must be strings"))?;
                    JsonType::parse(s).ok_or_else(|| inv(path, &format!("unknown type '{s}'")))
                })
                .collect()
        }
        _ => Err(inv(
            path,
            "'type' must be a string or a non-empty array of strings",
        )),
    }
}

fn parse_string_array(
    v: &Value,
    path: &str,
    name: &str,
    enforce_unique: bool,
) -> Result<Vec<String>, SchemaError> {
    let arr = v
        .as_array()
        .ok_or_else(|| inv(path, &format!("'{name}' must be an array of strings")))?;
    let mut out = Vec::with_capacity(arr.len());
    let mut seen = BTreeSet::new();
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| inv(path, &format!("'{name}' items must be strings")))?
            .to_string();
        if enforce_unique && !seen.insert(s.clone()) {
            return Err(inv(
                path,
                &format!("'{name}' must not contain duplicate entries (duplicate '{s}')"),
            ));
        }
        out.push(s);
    }
    Ok(out)
}

fn parse_non_negative_int(v: &Value, path: &str, name: &str) -> Result<u64, SchemaError> {
    v.as_u64()
        .ok_or_else(|| inv(path, &format!("'{name}' must be a non-negative integer")))
}

fn parse_number(v: &Value, path: &str, name: &str) -> Result<f64, SchemaError> {
    v.as_f64()
        .ok_or_else(|| inv(path, &format!("'{name}' must be a number")))
}

fn compile_object<'a>(
    map: &Map<String, Value>,
    path: &str,
    ctx: &mut Ctx<'a>,
    depth: u32,
    max_depth: &mut u32,
) -> Result<ObjectSchema, SchemaError> {
    let mut schema = ObjectSchema::default();

    if let Some(v) = map.get("type") {
        schema.types = Some(parse_type_keyword(v, path)?);
    }

    if let Some(v) = map.get("properties") {
        let obj = v
            .as_object()
            .ok_or_else(|| inv(path, "'properties' must be an object"))?;
        for (name, sub) in obj {
            let child = compile_node(
                sub,
                &child_path(path, &format!("properties/{name}")),
                ctx,
                depth + 1,
                max_depth,
            )?;
            schema.properties.insert(name.clone(), child);
        }
    }

    if let Some(v) = map.get("required") {
        schema.required = parse_string_array(v, path, "required", true)?
            .into_iter()
            .collect();
    }

    if let Some(v) = map.get("additionalProperties") {
        schema.additional_properties = match v {
            Value::Bool(true) => AdditionalProperties::Allowed,
            Value::Bool(false) => AdditionalProperties::Denied,
            Value::Object(_) => AdditionalProperties::Schema(Box::new(compile_node(
                v,
                &child_path(path, "additionalProperties"),
                ctx,
                depth + 1,
                max_depth,
            )?)),
            _ => {
                return Err(inv(
                    path,
                    "'additionalProperties' must be a boolean or a schema",
                ))
            }
        };
    }

    if let Some(v) = map.get("minProperties") {
        schema.min_properties = Some(parse_non_negative_int(v, path, "minProperties")?);
    }
    if let Some(v) = map.get("maxProperties") {
        schema.max_properties = Some(parse_non_negative_int(v, path, "maxProperties")?);
    }

    if let Some(v) = map.get("items") {
        if v.is_array() {
            return Err(inv(
                path,
                "'items' must be a single schema, not an array (tuple validation is not supported)",
            ));
        }
        schema.items = Some(Box::new(compile_node(
            v,
            &child_path(path, "items"),
            ctx,
            depth + 1,
            max_depth,
        )?));
    }

    if let Some(v) = map.get("minItems") {
        schema.min_items = Some(parse_non_negative_int(v, path, "minItems")?);
    }
    if let Some(v) = map.get("maxItems") {
        schema.max_items = Some(parse_non_negative_int(v, path, "maxItems")?);
    }
    if let Some(v) = map.get("uniqueItems") {
        schema.unique_items = v
            .as_bool()
            .ok_or_else(|| inv(path, "'uniqueItems' must be a boolean"))?;
    }

    if let Some(v) = map.get("enum") {
        let arr = v
            .as_array()
            .ok_or_else(|| inv(path, "'enum' must be a non-empty array"))?;
        if arr.is_empty() {
            return Err(inv(path, "'enum' must not be empty"));
        }
        schema.enum_values = Some(arr.clone());
    }

    if let Some(v) = map.get("const") {
        schema.const_value = Some(v.clone());
    }

    if let Some(v) = map.get("minimum") {
        schema.minimum = Some(parse_number(v, path, "minimum")?);
    }
    if let Some(v) = map.get("maximum") {
        schema.maximum = Some(parse_number(v, path, "maximum")?);
    }
    if let Some(v) = map.get("exclusiveMinimum") {
        schema.exclusive_minimum = Some(parse_number(v, path, "exclusiveMinimum")?);
    }
    if let Some(v) = map.get("exclusiveMaximum") {
        schema.exclusive_maximum = Some(parse_number(v, path, "exclusiveMaximum")?);
    }
    if let Some(v) = map.get("multipleOf") {
        // Kept as the original `Number` (not just its `f64` value) so
        // `validate` can tell an exact-integer divisor from a float one —
        // see `multiple_of`'s field doc and the module header.
        let Value::Number(num) = v else {
            return Err(inv(path, "'multipleOf' must be a number"));
        };
        let n = num
            .as_f64()
            .ok_or_else(|| inv(path, "'multipleOf' must be a number"))?;
        if n.is_nan() || n <= 0.0 {
            return Err(inv(path, "'multipleOf' must be greater than 0"));
        }
        schema.multiple_of = Some(num.clone());
    }

    if let Some(v) = map.get("minLength") {
        schema.min_length = Some(parse_non_negative_int(v, path, "minLength")?);
    }
    if let Some(v) = map.get("maxLength") {
        schema.max_length = Some(parse_non_negative_int(v, path, "maxLength")?);
    }

    if let Some(v) = map.get("pattern") {
        let s = v
            .as_str()
            .ok_or_else(|| inv(path, "'pattern' must be a string"))?;
        let re = RegexBuilder::new(s)
            .size_limit(SIZE_LIMIT_BYTES)
            .build()
            .map_err(|e| inv(path, &format!("invalid 'pattern': {e}")))?;
        schema.pattern = Some(re);
    }

    for kw in ["allOf", "anyOf", "oneOf"] {
        if let Some(v) = map.get(kw) {
            let arr = v
                .as_array()
                .ok_or_else(|| inv(path, &format!("'{kw}' must be a non-empty array")))?;
            if arr.is_empty() {
                return Err(inv(path, &format!("'{kw}' must not be empty")));
            }
            let mut nodes = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                nodes.push(compile_node(
                    item,
                    &child_path(path, &format!("{kw}/{i}")),
                    ctx,
                    depth + 1,
                    max_depth,
                )?);
            }
            match kw {
                "allOf" => schema.all_of = nodes,
                "anyOf" => schema.any_of = nodes,
                "oneOf" => schema.one_of = nodes,
                _ => unreachable!(),
            }
        }
    }

    if let Some(v) = map.get("not") {
        schema.not = Some(Box::new(compile_node(
            v,
            &child_path(path, "not"),
            ctx,
            depth + 1,
            max_depth,
        )?));
    }

    if let Some(v) = map.get("dependentRequired") {
        let obj = v
            .as_object()
            .ok_or_else(|| inv(path, "'dependentRequired' must be an object"))?;
        for (k, val) in obj {
            let arr = parse_string_array(val, path, "dependentRequired", false)?;
            schema.dependent_required.insert(k.clone(), arr);
        }
    }

    // `$defs`/`definitions`: root-level entries are pre-scanned into
    // `ctx.defs` (see `compile`) and validated either lazily via `$ref` or
    // by the post-compile coverage pass — never eagerly recursed into here,
    // to avoid compiling them out of dependency order. A NESTED occurrence
    // (unusual but structurally legal) is validated eagerly right here,
    // since `$ref` can never address it — JSON Pointer targets always
    // resolve against the document root, not the current schema position.
    for kw in ["$defs", "definitions"] {
        if let Some(v) = map.get(kw) {
            let obj = v
                .as_object()
                .ok_or_else(|| inv(path, &format!("'{kw}' must be an object")))?;
            if !path.is_empty() {
                for (name, sub) in obj {
                    compile_node(
                        sub,
                        &child_path(path, &format!("{kw}/{name}")),
                        ctx,
                        depth + 1,
                        max_depth,
                    )?;
                }
            }
        }
    }

    validate_annotation_keywords(map, path)?;
    // `default` and `$ref` themselves: no further structural validation.

    Ok(schema)
}

/// Loosely type-checks the annotation-only keywords (accepted, otherwise
/// ignored at validation time). Shared by `compile_object`'s normal walk
/// and by `compile_node`'s `$ref`-sibling check — the latter never sees
/// `$schema`/`$id`/`format` in practice since `REF_ALLOWED_SIBLINGS`
/// excludes them, but the checks are harmless no-ops when absent.
fn validate_annotation_keywords(map: &Map<String, Value>, path: &str) -> Result<(), SchemaError> {
    if let Some(v) = map.get("$schema") {
        v.as_str()
            .ok_or_else(|| inv(path, "'$schema' must be a string"))?;
    }
    if let Some(v) = map.get("$id") {
        v.as_str()
            .ok_or_else(|| inv(path, "'$id' must be a string"))?;
    }
    for kw in ["title", "description", "$comment"] {
        if let Some(v) = map.get(kw) {
            v.as_str()
                .ok_or_else(|| inv(path, &format!("'{kw}' must be a string")))?;
        }
    }
    if let Some(v) = map.get("examples") {
        v.as_array()
            .ok_or_else(|| inv(path, "'examples' must be an array"))?;
    }
    for kw in ["deprecated", "readOnly", "writeOnly"] {
        if let Some(v) = map.get(kw) {
            v.as_bool()
                .ok_or_else(|| inv(path, &format!("'{kw}' must be a boolean")))?;
        }
    }
    if let Some(v) = map.get("format") {
        v.as_str()
            .ok_or_else(|| inv(path, "'format' must be a string"))?;
    }
    Ok(())
}

// -----------------------------------------------------------------------
// Validate
// -----------------------------------------------------------------------

fn violation(path: &str, msg: &str) -> SchemaError {
    SchemaError::Violation(format!("{}: {msg}", ptr(path)))
}

/// Numeric equality for `json_deep_eq`/`const`/`enum`/`uniqueItems`: compares
/// a pair of `serde_json::Number`s by `as_i64`, then `as_u64`, then `as_f64`,
/// in that priority order (see the module header). Two integer-typed numbers
/// — regardless of JSON literal syntax, and regardless of magnitude — compare
/// exactly with no precision loss; `serde_json::Number::as_f64` on an integer
/// beyond 2^53 would silently round to its nearest representable double,
/// which is exactly the bug this ordering avoids (two DIFFERENT large
/// integers can round to the SAME double). Only when neither side has an
/// exact integer representation do both fall back to `f64`, which is then
/// only trustworthy within +/-2^53 — an intrinsic property of that fallback
/// tier, not something this function separately re-checks.
fn number_eq(x: &Number, y: &Number) -> bool {
    if let (Some(a), Some(b)) = (x.as_i64(), y.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (x.as_u64(), y.as_u64()) {
        return a == b;
    }
    x.as_f64() == y.as_f64()
}

/// The exact integer value of `n`, if it has one — `Some` for any JSON
/// integer literal (`i64` or `u64` range, unified into `i128` so the two can
/// be compared/reduced directly), `None` for a JSON float literal. Used by
/// `multipleOf` to pick exact modulo arithmetic over `n`'s value and
/// divisor whenever both are exact integers.
fn number_as_exact_i128(n: &Number) -> Option<i128> {
    if let Some(i) = n.as_i64() {
        return Some(i as i128);
    }
    n.as_u64().map(|u| u as i128)
}

/// Deep JSON equality treating numbers by numeric value (so `1` and `1.0`
/// compare equal, matching JSON Schema's definition of instance equality)
/// rather than `serde_json::Value`'s derived `PartialEq`, which compares the
/// `Number`'s internal representation and would otherwise treat an
/// integer-typed and float-typed encoding of the same value as unequal.
fn json_deep_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => number_eq(x, y),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(xi, yi)| json_deep_eq(xi, yi))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|yv| json_deep_eq(v, yv)))
        }
        _ => a == b,
    }
}

/// Normalizes a number for `uniqueItems` canonicalization (see
/// `canonicalize`): folds any integral value within +/-2^53 — regardless of
/// whether it arrived as an exact `i64`/`u64` or as an integral `f64` (e.g.
/// JSON `5` vs `5.0`) — onto the SAME `serde_json::Number`, so two items
/// that `json_deep_eq`/`number_eq` treat as equal always canonicalize to the
/// same text.
///
/// Beyond +/-2^53, this deliberately gives up exactness and routes through
/// the SAME `as_f64` fallback tier `number_eq` itself falls back to once
/// neither side has a matching exact-integer representation: `number_eq`
/// compares an out-of-range integer literal against a float literal via
/// `as_f64`, which silently rounds the integer to its nearest representable
/// double — so keeping the integer's own exact text here (as an earlier
/// version of this function did) would let two values `number_eq` treats as
/// equal (because they round to the same double) canonicalize to DIFFERENT
/// keys, and `uniqueItems` would miss the duplicate (finding 6). Folding
/// every number beyond that boundary through the same lossy `as_f64` step
/// keeps `uniqueItems` consistent with `number_eq`'s own precision tier,
/// at the cost of two distinct huge integers that happen to round to the
/// same double now also canonicalizing identically — an intrinsic
/// consequence of `number_eq`'s fallback, not a separate bug here.
fn canonical_number(n: &Number) -> Number {
    if let Some(i) = n.as_i64() {
        if i.unsigned_abs() <= MAX_EXACT_INTEGER_U64 {
            return Number::from(i);
        }
    } else if let Some(u) = n.as_u64() {
        if u <= MAX_EXACT_INTEGER_U64 {
            return Number::from(u);
        }
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.is_finite() && f.fract() == 0.0 && f.abs() <= MAX_EXACT_INTEGER_F64 {
        return Number::from(f as i64);
    }
    Number::from_f64(f).unwrap_or_else(|| n.clone())
}

/// Writes a canonical, deep-equality-consistent text form of `value` into
/// `out`: numbers normalized via `canonical_number`, object keys explicitly
/// sorted. This crate's `serde_json` is built with the `preserve_order`
/// feature ON (pulled in transitively — see `cargo tree -e features -i
/// serde_json`), so `serde_json::Map` is an insertion-ordered `IndexMap`,
/// NOT a `BTreeMap`: two objects with the same key/value pairs in a
/// different source order (`{"a":1,"b":2}` vs `{"b":2,"a":1}`, which JSON
/// Schema — and `json_deep_eq` — treat as equal) would otherwise
/// canonicalize to different text and `uniqueItems` would miss the
/// duplicate. Building the canonical text directly (rather than rebuilding
/// a `Value` and serializing that) avoids ever routing object keys back
/// through `Map`'s own iteration order.
fn canonicalize_into(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            write!(out, "{}", canonical_number(n)).expect("write! to a String never fails");
        }
        Value::String(s) => {
            out.push_str(&serde_json::to_string(s).expect("serializing a &str never fails"));
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, item) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonicalize_into(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("serializing a &str never fails"));
                out.push(':');
                canonicalize_into(v, out);
            }
            out.push('}');
        }
    }
}

fn budget_exhausted(path: &str) -> SchemaError {
    violation(
        path,
        &format!("validation step budget exhausted (limit {MAX_VALIDATION_STEPS} steps)"),
    )
}

fn validate_node(
    node: &SchemaNode,
    value: &Value,
    path: &mut String,
    steps: &mut u32,
) -> Result<(), SchemaError> {
    *steps += 1;
    if *steps > MAX_VALIDATION_STEPS {
        return Err(budget_exhausted(path));
    }
    match node {
        SchemaNode::Bool(true) => Ok(()),
        SchemaNode::Bool(false) => {
            Err(violation(path, "schema is 'false': no value is valid here"))
        }
        SchemaNode::Object(schema) => validate_object_schema(schema, value, path, steps),
    }
}

fn validate_object_schema(
    schema: &ObjectSchema,
    value: &Value,
    path: &mut String,
    steps: &mut u32,
) -> Result<(), SchemaError> {
    if let Some(types) = &schema.types {
        if !types.iter().any(|t| t.matches(value)) {
            let expected: Vec<&str> = types.iter().map(|t| t.as_str()).collect();
            return Err(violation(
                path,
                &format!("expected {}", expected.join(" or ")),
            ));
        }
    }

    if let Some(values) = &schema.enum_values {
        if !values.iter().any(|v| json_deep_eq(v, value)) {
            return Err(violation(path, "value is not in 'enum'"));
        }
    }

    if let Some(cv) = &schema.const_value {
        if !json_deep_eq(cv, value) {
            return Err(violation(path, "value does not equal 'const'"));
        }
    }

    match value {
        Value::Object(obj) => {
            for name in &schema.required {
                if !obj.contains_key(name) {
                    return Err(violation(
                        path,
                        &format!("missing required property '{name}'"),
                    ));
                }
            }
            if let Some(min) = schema.min_properties {
                if (obj.len() as u64) < min {
                    return Err(violation(
                        path,
                        &format!("fewer properties than minProperties {min}"),
                    ));
                }
            }
            if let Some(max) = schema.max_properties {
                if (obj.len() as u64) > max {
                    return Err(violation(
                        path,
                        &format!("more properties than maxProperties {max}"),
                    ));
                }
            }

            for (name, sub_value) in obj {
                let mark = path.len();
                path.push('/');
                path.push_str(name);
                let result = if let Some(sub_schema) = schema.properties.get(name) {
                    validate_node(sub_schema, sub_value, path, steps)
                } else {
                    match &schema.additional_properties {
                        AdditionalProperties::Allowed => Ok(()),
                        AdditionalProperties::Denied => {
                            Err(violation(path, "additional property is not allowed"))
                        }
                        AdditionalProperties::Schema(sub) => {
                            validate_node(sub, sub_value, path, steps)
                        }
                    }
                };
                path.truncate(mark);
                result?;
            }

            for (dep_prop, deps) in &schema.dependent_required {
                if obj.contains_key(dep_prop) {
                    for req in deps {
                        if !obj.contains_key(req) {
                            return Err(violation(
                                path,
                                &format!(
                                    "property '{dep_prop}' requires property '{req}' to also be present"
                                ),
                            ));
                        }
                    }
                }
            }
        }
        Value::Array(arr) => {
            if let Some(min) = schema.min_items {
                if (arr.len() as u64) < min {
                    return Err(violation(path, &format!("fewer items than minItems {min}")));
                }
            }
            if let Some(max) = schema.max_items {
                if (arr.len() as u64) > max {
                    return Err(violation(path, &format!("more items than maxItems {max}")));
                }
            }
            if schema.unique_items {
                // O(n): canonicalize each item into a comparable text key
                // instead of an O(n^2) pairwise deep-equality scan — the
                // latter turns a large array of repeated values into a
                // publish-time hang (see the module header).
                let mut seen: HashSet<String> = HashSet::with_capacity(arr.len());
                for item in arr {
                    let mut key = String::new();
                    canonicalize_into(item, &mut key);
                    if !seen.insert(key) {
                        return Err(violation(path, "items are not unique"));
                    }
                }
            }
            if let Some(item_schema) = &schema.items {
                for (i, item) in arr.iter().enumerate() {
                    let mark = path.len();
                    write!(path, "/{i}").expect("write! to a String never fails");
                    let result = validate_node(item_schema, item, path, steps);
                    path.truncate(mark);
                    result?;
                }
            }
        }
        Value::String(s) => {
            let len = s.chars().count() as u64;
            if let Some(min) = schema.min_length {
                if len < min {
                    return Err(violation(path, &format!("shorter than minLength {min}")));
                }
            }
            if let Some(max) = schema.max_length {
                if len > max {
                    return Err(violation(path, &format!("longer than maxLength {max}")));
                }
            }
            if let Some(re) = &schema.pattern {
                if !re.is_match(s) {
                    return Err(violation(path, "does not match pattern"));
                }
            }
        }
        Value::Number(num) => {
            let n = num.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = schema.minimum {
                if n < min {
                    return Err(violation(path, &format!("below minimum {min}")));
                }
            }
            if let Some(max) = schema.maximum {
                if n > max {
                    return Err(violation(path, &format!("above maximum {max}")));
                }
            }
            if let Some(ex_min) = schema.exclusive_minimum {
                if n <= ex_min {
                    return Err(violation(
                        path,
                        &format!("at or below exclusiveMinimum {ex_min}"),
                    ));
                }
            }
            if let Some(ex_max) = schema.exclusive_maximum {
                if n >= ex_max {
                    return Err(violation(
                        path,
                        &format!("at or above exclusiveMaximum {ex_max}"),
                    ));
                }
            }
            if let Some(m) = &schema.multiple_of {
                let violates = match (number_as_exact_i128(num), number_as_exact_i128(m)) {
                    // Both sides are exact integers (any magnitude): exact
                    // modulo arithmetic, no floating-point error possible.
                    (Some(value_i), Some(div_i)) => value_i % div_i != 0,
                    _ => {
                        let ratio = n / m.as_f64().unwrap_or(f64::NAN);
                        // Beyond +/-2^53 a `f64` has no fractional bits left
                        // to compare — the check would be a silent no-op
                        // rather than a real constraint, so treat it (and
                        // any non-finite ratio, e.g. from overflow) as an
                        // unverifiable violation instead of accepting it.
                        !ratio.is_finite()
                            || ratio.abs() > MAX_EXACT_INTEGER_F64
                            || (ratio - ratio.round()).abs() > 1e-9 * ratio.abs().max(1.0)
                    }
                };
                if violates {
                    return Err(violation(path, "not a multiple of 'multipleOf'"));
                }
            }
        }
        _ => {}
    }

    for sub in &schema.all_of {
        validate_node(sub, value, path, steps)?;
    }

    if !schema.any_of.is_empty() {
        let mut matched = false;
        for sub in &schema.any_of {
            let ok = validate_node(sub, value, path, steps).is_ok();
            if *steps > MAX_VALIDATION_STEPS {
                return Err(budget_exhausted(path));
            }
            if ok {
                matched = true;
                break;
            }
        }
        if !matched {
            return Err(violation(
                path,
                "value does not match any branch of 'anyOf'",
            ));
        }
    }

    if !schema.one_of.is_empty() {
        let mut match_count = 0usize;
        for sub in &schema.one_of {
            let ok = validate_node(sub, value, path, steps).is_ok();
            if *steps > MAX_VALIDATION_STEPS {
                return Err(budget_exhausted(path));
            }
            if ok {
                match_count += 1;
            }
        }
        match match_count {
            1 => {}
            0 => {
                return Err(violation(
                    path,
                    "value does not match any branch of 'oneOf'",
                ))
            }
            _ => {
                return Err(violation(
                    path,
                    "value matches more than one branch of 'oneOf'",
                ))
            }
        }
    }

    if let Some(not_schema) = &schema.not {
        let matched_not = validate_node(not_schema, value, path, steps).is_ok();
        // Fail closed, matching the `anyOf`/`oneOf` arms above: if the inner
        // walk exhausted the step budget, `Ok` doesn't mean "not' was
        // satisfied", it means the walk gave up. Treating that as "not"
        // succeeding would accept an otherwise-violating record.
        if *steps > MAX_VALIDATION_STEPS {
            return Err(budget_exhausted(path));
        }
        if matched_not {
            return Err(violation(path, "value must not match the 'not' schema"));
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------
// Compatibility
// -----------------------------------------------------------------------

fn properties_map(v: &Value) -> BTreeMap<&str, &Value> {
    v.get("properties")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.as_str(), v)).collect())
        .unwrap_or_default()
}

fn required_set(v: &Value) -> BTreeSet<&str> {
    v.get("required")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

fn additional_properties_is_false(v: &Value) -> bool {
    matches!(v.get("additionalProperties"), Some(Value::Bool(false)))
}

/// Root keys the structural compatibility check evaluates directly
/// (`properties`/`required`/`type`) or handles via its own dedicated rule
/// (`additionalProperties`, see `check_additional_properties_schema_form`).
const COMPAT_STRUCTURALLY_HANDLED_ROOT_KEYS: &[&str] =
    &["properties", "required", "type", "additionalProperties"];

/// Root keys that never affect validation and so are exempt from the
/// "every other root key must be unchanged" rule below — same list as
/// `REF_ALLOWED_SIBLINGS` plus `format`, i.e. every annotation-only keyword
/// this module accepts.
const COMPAT_ANNOTATION_ONLY_ROOT_KEYS: &[&str] = &[
    "$schema",
    "$id",
    "title",
    "description",
    "$comment",
    "examples",
    "default",
    "deprecated",
    "readOnly",
    "writeOnly",
    "format",
];

/// Owner decision 2 ("nothing outside the supported subset may be silently
/// ignored") applied to compatibility checking: every root keyword this
/// module supports OTHER than the four handled structurally above, the
/// annotation-only ones, and `$defs`/`definitions` (handled separately by
/// `check_reachable_defs_are_unchanged`, see its doc comment) must be
/// byte-for-byte deep-equal between `old` and `new`, or the change is
/// `Incompatible`. This is the fail-closed backstop for everything the
/// structural comparison cannot see on its own — `minProperties`/
/// `maxProperties`, `allOf`/`anyOf`/`oneOf`/`not`, `dependentRequired`,
/// `enum`, `const`.
fn check_other_root_keys_are_unchanged(old: &Value, new: &Value) -> Result<(), SchemaError> {
    let (Some(old_map), Some(new_map)) = (old.as_object(), new.as_object()) else {
        return Ok(()); // Non-object roots are rejected earlier by `root_type_includes_object`.
    };
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    keys.extend(old_map.keys().map(String::as_str));
    keys.extend(new_map.keys().map(String::as_str));
    for key in keys {
        if key == "$defs" || key == "definitions" {
            continue; // Handled by `check_reachable_defs_are_unchanged` instead.
        }
        if COMPAT_STRUCTURALLY_HANDLED_ROOT_KEYS.contains(&key)
            || COMPAT_ANNOTATION_ONLY_ROOT_KEYS.contains(&key)
        {
            continue;
        }
        let old_v = old_map.get(key);
        let new_v = new_map.get(key);
        let equal = match (old_v, new_v) {
            (Some(a), Some(b)) => json_deep_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !equal {
            return Err(SchemaError::Incompatible(format!(
                "root keyword '{key}' changed and is not covered by the structural \
                 properties/required/type/additionalProperties compatibility check"
            )));
        }
    }
    Ok(())
}

/// Collects `"$defs/<name>"` / `"definitions/<name>"` -> raw definition body
/// pairs present at `v`'s root. Shared by `derive_subschema`'s reachability
/// pruning and `check_reachable_defs_are_unchanged` below.
fn defs_by_key(v: &Value) -> BTreeMap<String, &Value> {
    let mut out = BTreeMap::new();
    for kw in ["$defs", "definitions"] {
        if let Some(Value::Object(obj)) = v.get(kw) {
            for (name, sub) in obj {
                out.insert(format!("{kw}/{name}"), sub);
            }
        }
    }
    out
}

/// `$defs`/`definitions` entries transitively reachable (via `$ref`) from
/// `v`'s `properties`.
fn defs_reachable_from_properties(v: &Value) -> BTreeSet<String> {
    let mut ref_roots = BTreeSet::new();
    if let Some(Value::Object(props)) = v.get("properties") {
        for sub in props.values() {
            collect_ref_targets(sub, &mut ref_roots);
        }
    }
    reachable_defs(&ref_roots, &defs_by_key(v))
}

/// Finding 5 (F3 second-pass review): only `$defs`/`definitions` entries
/// transitively reachable from the OLD schema's `properties` are required to
/// stay byte-for-byte unchanged between `old` and `new`. A definition that
/// is unreachable from any OLD property — including one added purely
/// alongside a brand-new, purely-additive `new` property — has no effect on
/// `validate`'s behavior against data written under the OLD schema, so
/// requiring it to match exactly would reject genuinely compatible changes
/// (e.g. Backward-adding an optional property whose subschema happens to be
/// factored out into a new `$defs` entry via `$ref` instead of inlined).
/// A definition reachable from an OLD property must still match exactly on
/// both sides: this is what keeps the `$ref`-mediated bypass closed, where
/// `properties.x = {"$ref": "#/$defs/Id"}` looks unchanged at the property
/// level but `$defs.Id` itself was silently swapped underneath it.
fn check_reachable_defs_are_unchanged(old: &Value, new: &Value) -> Result<(), SchemaError> {
    let reachable = defs_reachable_from_properties(old);
    if reachable.is_empty() {
        return Ok(());
    }
    let old_defs = defs_by_key(old);
    let new_defs = defs_by_key(new);
    for key in &reachable {
        let equal = match (old_defs.get(key.as_str()), new_defs.get(key.as_str())) {
            (Some(a), Some(b)) => json_deep_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !equal {
            return Err(SchemaError::Incompatible(format!(
                "definition '#/{key}', reachable from an old-schema property, changed and is not \
                 covered by the structural properties/required/type/additionalProperties \
                 compatibility check"
            )));
        }
    }
    Ok(())
}

/// Schema-form `additionalProperties` (an object, not a boolean) must be
/// deep-equal on both sides — the boolean-only widening rule in
/// `check_direction` does not apply to it, since a schema constrains the
/// SHAPE of additional properties, not just whether they are allowed.
fn check_additional_properties_schema_form_is_unchanged(
    old: &Value,
    new: &Value,
) -> Result<(), SchemaError> {
    let old_ap = old.get("additionalProperties");
    let new_ap = new.get("additionalProperties");
    let old_is_schema = matches!(old_ap, Some(Value::Object(_)));
    let new_is_schema = matches!(new_ap, Some(Value::Object(_)));
    if !old_is_schema && !new_is_schema {
        return Ok(());
    }
    let equal = match (old_ap, new_ap) {
        (Some(a), Some(b)) => json_deep_eq(a, b),
        _ => false,
    };
    if !equal {
        return Err(SchemaError::Incompatible(
            "root keyword 'additionalProperties' (schema form) changed".to_string(),
        ));
    }
    Ok(())
}

fn type_set(v: &Value) -> Option<BTreeSet<String>> {
    match v.get("type") {
        None => None,
        Some(Value::String(s)) => Some([s.clone()].into_iter().collect()),
        Some(Value::Array(arr)) => Some(
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

fn root_type_includes_object(v: &Value) -> bool {
    match v.get("type") {
        None => true,
        Some(Value::String(s)) => s == "object",
        Some(Value::Array(arr)) => arr.iter().any(|x| x.as_str() == Some("object")),
        _ => false,
    }
}

/// `types_reader` must be able to accept everything `types_data` allows —
/// equal types match, and `integer` in `types_data` is satisfied by
/// `number` in `types_reader` (the one widening rule this subset supports).
/// `None` means "no `type` restriction", i.e. every instance type: an
/// unrestricted reader accepts any data; an unrestricted data-writer can
/// only be safely read by an equally unrestricted reader.
fn types_widened_or_equal(
    data: &Option<BTreeSet<String>>,
    reader: &Option<BTreeSet<String>>,
) -> bool {
    match (data, reader) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(d), Some(r)) => d
            .iter()
            .all(|dt| r.contains(dt) || (dt == "integer" && r.contains("number"))),
    }
}

fn without_type_key(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut clone = map.clone();
            clone.remove("type");
            Value::Object(clone)
        }
        other => other.clone(),
    }
}

fn check_shared_properties_differ_only_by_type(
    old: &Value,
    new: &Value,
) -> Result<(), SchemaError> {
    let old_props = properties_map(old);
    let new_props = properties_map(new);
    for (name, old_schema) in &old_props {
        if let Some(new_schema) = new_props.get(name) {
            let old_stripped = without_type_key(old_schema);
            let new_stripped = without_type_key(new_schema);
            if !json_deep_eq(&old_stripped, &new_stripped) {
                return Err(SchemaError::Incompatible(format!(
                    "property '{name}' subschema differs beyond 'type' widening"
                )));
            }
        }
    }
    Ok(())
}

/// One direction of a compatibility check: can `reader` read data written
/// under `data`? Used for both `Backward` (`reader` = new, `data` = old)
/// and `Forward` (`reader` = old, `data` = new).
fn check_direction(reader: &Value, data: &Value) -> Result<(), SchemaError> {
    let reader_required = required_set(reader);
    let data_required = required_set(data);
    for r in &reader_required {
        if !data_required.contains(r) {
            return Err(SchemaError::Incompatible(format!(
                "property '{r}' is required by the reader schema but not guaranteed present by the writer schema"
            )));
        }
    }

    let reader_props = properties_map(reader);
    let data_props = properties_map(data);
    let reader_additional_false = additional_properties_is_false(reader);
    for name in data_props.keys() {
        if !reader_props.contains_key(name) && reader_additional_false {
            return Err(SchemaError::Incompatible(format!(
                "property '{name}' present in the writer schema has no counterpart in the reader \
                 schema, which rejects additional properties"
            )));
        }
    }

    for (name, data_schema) in &data_props {
        if let Some(reader_schema) = reader_props.get(name) {
            let data_types = type_set(data_schema);
            let reader_types = type_set(reader_schema);
            if !types_widened_or_equal(&data_types, &reader_types) {
                return Err(SchemaError::Incompatible(format!(
                    "property '{name}' type is narrowed: writer allows {data_types:?}, reader allows {reader_types:?}"
                )));
            }
        }
    }

    let data_additional_false = additional_properties_is_false(data);
    if !data_additional_false && reader_additional_false {
        return Err(SchemaError::Incompatible(
            "writer schema allows additional properties but reader schema rejects them \
             (additionalProperties: false)"
                .to_string(),
        ));
    }

    Ok(())
}

// -----------------------------------------------------------------------
// Trait impl
// -----------------------------------------------------------------------

/// Everything `compile` produces, before it is trimmed down to the
/// long-lived `Compiled` form. Shared by `compile` itself and by
/// `compiled_root`/`compiled_defs` (`check_compatibility`/`derive_subschema`
/// always recompile from caller-supplied text — see `Compiled`'s doc
/// comment — so there is exactly one place that walks a schema document).
struct CompileOutput {
    schema: SchemaNode,
    root: Value,
    /// Every `$defs`/`definitions` entry, compiled, keyed `"$defs/<name>"` /
    /// `"definitions/<name>"` — used by `compile`'s expansion-cost estimate
    /// (finding 1) to also catch a combinatorial chain that the root schema
    /// itself does not directly reference.
    defs: HashMap<String, SchemaNode>,
}

fn compile_schema_text(schema_text: &str) -> Result<CompileOutput, SchemaError> {
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(SchemaError::Invalid(format!(
            "schema text is {} bytes, exceeding the {MAX_SCHEMA_TEXT_BYTES}-byte limit",
            schema_text.len()
        )));
    }
    let root: Value = serde_json::from_str(schema_text)
        .map_err(|e| SchemaError::Invalid(format!("not valid JSON: {e}")))?;
    let map = root
        .as_object()
        .ok_or_else(|| SchemaError::Invalid("root schema must be a JSON object".to_string()))?;

    validate_schema_draft(map)?;

    let mut ctx = Ctx {
        defs: BTreeMap::new(),
        cache: HashMap::new(),
        compiling: HashSet::new(),
    };
    collect_root_defs(map, "$defs", &mut ctx)?;
    collect_root_defs(map, "definitions", &mut ctx)?;

    let mut root_max_depth = 0u32;
    let node = compile_node(&root, "", &mut ctx, 0, &mut root_max_depth)?;

    // Coverage pass: every `$defs`/`definitions` entry is validated even
    // if never referenced by a `$ref` — the owner's hard requirement is
    // that nothing in a registered schema is silently ignored. `ctx.defs`
    // is a `BTreeMap` (not a `HashMap`) specifically so this iteration order
    // — and therefore which `$ref` cycle/depth errors fire first, and the
    // resulting `CompileOutput.defs` — is deterministic across repeated
    // compiles of the same text, instead of varying with the process's
    // random hash seed (finding 2).
    let remaining: Vec<String> = ctx
        .defs
        .keys()
        .filter(|k| !ctx.cache.contains_key(*k))
        .cloned()
        .collect();
    for key in remaining {
        let def_value = *ctx.defs.get(&key).expect("key came from ctx.defs.keys()");
        compile_and_cache_def(key, def_value, &mut ctx, 1)?;
    }

    // Explicitly destructure `ctx` to drop `defs` (borrows into `root`) here,
    // rather than at the end of scope — the latter would conflict with
    // moving `root` into the returned `CompileOutput` below, since a
    // borrow-holding field pending an implicit `Drop` must not outlive the
    // data it borrows.
    let Ctx { cache, .. } = ctx;
    let defs: HashMap<String, SchemaNode> = cache
        .into_iter()
        .map(|(key, (node, _height))| (key, node))
        .collect();

    Ok(CompileOutput {
        schema: node,
        root,
        defs,
    })
}

/// Estimates the worst-case number of `validate_node` calls a single
/// `validate` invocation against `node` could perform, WITHOUT memoizing
/// repeated `$ref` targets — mirroring `validate_node`'s own unmemoized walk,
/// which is exactly what lets a chain of `$ref`s inside `allOf`/`anyOf`/
/// `oneOf` multiply out combinatorially. Bails out with `Err` the moment the
/// running `total` exceeds `MAX_COMPILE_EXPANSION_ESTIMATE`, so the estimator
/// itself terminates in bounded time no matter how deep the (potentially
/// exponential) tree it is walking is — it never needs to finish counting a
/// schema it is about to reject anyway.
fn estimate_expansion_cost(node: &SchemaNode, total: &mut u64) -> Result<(), ()> {
    *total += 1;
    if *total > MAX_COMPILE_EXPANSION_ESTIMATE {
        return Err(());
    }
    let SchemaNode::Object(schema) = node else {
        return Ok(());
    };
    for sub in schema.properties.values() {
        estimate_expansion_cost(sub, total)?;
    }
    if let Some(items) = &schema.items {
        estimate_expansion_cost(items, total)?;
    }
    if let AdditionalProperties::Schema(sub) = &schema.additional_properties {
        estimate_expansion_cost(sub, total)?;
    }
    for sub in schema
        .all_of
        .iter()
        .chain(schema.any_of.iter())
        .chain(schema.one_of.iter())
    {
        estimate_expansion_cost(sub, total)?;
    }
    if let Some(sub) = &schema.not {
        estimate_expansion_cost(sub, total)?;
    }
    Ok(())
}

/// Collects every local `$ref` target (`"$defs/<name>"` / `"definitions/<name>"`)
/// found anywhere in `value`'s subtree — used by `derive_subschema` to find
/// which `$defs`/`definitions` entries a projection's surviving properties
/// still depend on.
fn collect_ref_targets(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(r) = map.get("$ref").and_then(Value::as_str) {
                if let Some((container, name)) = parse_local_ref(r) {
                    out.insert(format!("{container}/{name}"));
                }
            }
            for v in map.values() {
                collect_ref_targets(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_ref_targets(v, out);
            }
        }
        _ => {}
    }
}

/// Transitive closure of `$defs`/`definitions` entries reachable by `$ref`
/// from `roots`, using `defs_by_key` (`"$defs/<name>"` -> its raw JSON body)
/// to follow one more hop each time (`$defs/A` -> `$defs/B` -> ...).
fn reachable_defs(
    roots: &BTreeSet<String>,
    defs_by_key: &BTreeMap<String, &Value>,
) -> BTreeSet<String> {
    let mut reachable = BTreeSet::new();
    let mut stack: Vec<String> = roots.iter().cloned().collect();
    while let Some(key) = stack.pop() {
        if !reachable.insert(key.clone()) {
            continue;
        }
        if let Some(def_value) = defs_by_key.get(&key) {
            let mut nested = BTreeSet::new();
            collect_ref_targets(def_value, &mut nested);
            for n in nested {
                if !reachable.contains(&n) {
                    stack.push(n);
                }
            }
        }
    }
    reachable
}

pub struct JsonSchemaOps;

impl JsonSchemaOps {
    fn compiled_root(&self, schema_text: &str) -> Result<Value, SchemaError> {
        Ok(compile_schema_text(schema_text)?.root)
    }
}

impl SchemaKindOps for JsonSchemaOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        let out = compile_schema_text(schema_text)?;

        // Reject at REGISTRATION time (never at publish time) a schema whose
        // structural `$ref`/combinator expansion is already too large for a
        // single `validate` call to complete safely — see
        // `estimate_expansion_cost` and the module header. Walking every
        // named `$defs`/`definitions` entry too (not just what the root
        // schema happens to reference) catches a pathological chain even
        // when nothing in `properties` points at its head yet.
        let mut expansion_total: u64 = 0;
        let mut exceeded = estimate_expansion_cost(&out.schema, &mut expansion_total).is_err();
        if !exceeded {
            for def_node in out.defs.values() {
                if estimate_expansion_cost(def_node, &mut expansion_total).is_err() {
                    exceeded = true;
                    break;
                }
            }
        }
        if exceeded {
            return Err(SchemaError::Invalid(format!(
                "schema's `$ref`/combinator expansion is too large to validate safely (estimated \
                 over {MAX_COMPILE_EXPANSION_ESTIMATE} `validate_node` calls per validation); \
                 shorten `allOf`/`anyOf`/`oneOf` chains through `$defs`/`definitions`"
            )));
        }

        Ok(CompiledSchema::JsonSchema(Compiled { schema: out.schema }))
    }

    fn validate(&self, compiled: &CompiledSchema, payload: &[u8]) -> Result<(), SchemaError> {
        let CompiledSchema::JsonSchema(c) = compiled else {
            return Err(SchemaError::Invalid(
                "validate called with a non-json_schema compiled schema".to_string(),
            ));
        };
        let value: Value = serde_json::from_slice(payload)
            .map_err(|_| SchemaError::Violation("<root>: payload is not valid JSON".to_string()))?;
        let mut path = String::new();
        let mut steps: u32 = 0;
        validate_node(&c.schema, &value, &mut path, &mut steps)
    }

    fn derive_subschema(
        &self,
        schema_text: &str,
        allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError> {
        let root = self.compiled_root(schema_text)?;
        let Value::Object(mut map) = root else {
            return Err(SchemaError::Invalid(
                "root schema must be an object to derive a projection".to_string(),
            ));
        };

        for combinator in ["allOf", "anyOf", "oneOf", "not"] {
            if map.contains_key(combinator) {
                return Err(SchemaError::Invalid(format!(
                    "cannot derive a projection through the root-level '{combinator}' combinator"
                )));
            }
        }

        if let Some(Value::Object(props)) = map.get_mut("properties") {
            props.retain(|name, _| allowed.contains(name));
        }

        if let Some(Value::Array(required)) = map.get_mut("required") {
            required.retain(|v| v.as_str().is_some_and(|s| allowed.contains(s)));
        }

        // Prune `$defs`/`definitions` to entries transitively reachable from
        // the SURVIVING root schema — not just `properties` — so a def only
        // referenced by a dropped property (e.g. a pattern-carrying
        // `$defs/SSN`) does not leak into the projection, and does not
        // become unsatisfiable dead weight the reader has no way to reach.
        // Scanning every root key OTHER than `$defs`/`definitions`
        // themselves (rather than `properties` alone) is required because
        // other root keywords are copied into the projection verbatim, e.g.
        // a root-level `items: {"$ref": "#/$defs/Item"}` — seeding roots
        // from `properties` only would prune `$defs/Item` out from under
        // that still-present `$ref`, leaving a dangling reference that
        // fails to recompile (finding 3).
        let reachable = {
            let mut ref_roots = BTreeSet::new();
            for (key, v) in map.iter() {
                if key == "$defs" || key == "definitions" {
                    continue;
                }
                collect_ref_targets(v, &mut ref_roots);
            }
            let mut defs_by_key: BTreeMap<String, &Value> = BTreeMap::new();
            for kw in ["$defs", "definitions"] {
                if let Some(Value::Object(obj)) = map.get(kw) {
                    for (name, sub) in obj {
                        defs_by_key.insert(format!("{kw}/{name}"), sub);
                    }
                }
            }
            reachable_defs(&ref_roots, &defs_by_key)
        };
        for kw in ["$defs", "definitions"] {
            if let Some(Value::Object(obj)) = map.get_mut(kw) {
                obj.retain(|name, _| reachable.contains(&format!("{kw}/{name}")));
            }
        }
        for kw in ["$defs", "definitions"] {
            if matches!(map.get(kw), Some(Value::Object(obj)) if obj.is_empty()) {
                map.remove(kw);
            }
        }

        // `additionalProperties: false` already caps the projected object at
        // the number of SURVIVING `properties` entries — not `allowed.len()`:
        // `allowed` may name fields the source schema never declared under
        // `properties` at all (e.g. a policy that allows a name the schema
        // doesn't happen to describe), which grant no room to satisfy
        // `additionalProperties: false` since they were never eligible to
        // survive the `properties.retain` above. Clamping against
        // `allowed.len()` in that case would leave `minProperties` above the
        // true achievable maximum, making the projection unsatisfiable
        // (finding 4). Any wider `minProperties`/`maxProperties` inherited
        // from the source schema is clamped to the actual surviving count.
        let surviving_properties = map
            .get("properties")
            .and_then(Value::as_object)
            .map_or(0u64, |props| props.len() as u64);
        if let Some(min_val) = map.get("minProperties").and_then(Value::as_u64) {
            let clamped = min_val.min(surviving_properties);
            map.insert("minProperties".to_string(), Value::Number(clamped.into()));
        }
        if let Some(max_val) = map.get("maxProperties").and_then(Value::as_u64) {
            let clamped = max_val.min(surviving_properties);
            map.insert("maxProperties".to_string(), Value::Number(clamped.into()));
        }

        if let Some(Value::Object(dep_req)) = map.get("dependentRequired") {
            for (key, deps) in dep_req {
                if !allowed.contains(key) {
                    continue; // Entry itself is dropped below; nothing to check.
                }
                if let Some(arr) = deps.as_array() {
                    for d in arr {
                        if let Some(name) = d.as_str() {
                            if !allowed.contains(name) {
                                return Err(SchemaError::Invalid(format!(
                                    "cannot derive a projection: 'dependentRequired' entry for \
                                     '{key}' references removed property '{name}'"
                                )));
                            }
                        }
                    }
                }
            }
        }
        if let Some(Value::Object(dep_req)) = map.get_mut("dependentRequired") {
            dep_req.retain(|key, _| allowed.contains(key));
        }

        // The projection must describe EXACTLY the allowed fields, so
        // `additionalProperties` is always forced to `false` — including
        // when the source schema had it absent, `true`, or a schema form.
        map.insert("additionalProperties".to_string(), Value::Bool(false));

        Ok(serde_json::to_string(&Value::Object(map))
            .expect("serializing a serde_json::Value never fails"))
    }

    fn check_compatibility(
        &self,
        old_schema_text: &str,
        new_schema_text: &str,
        mode: Compatibility,
    ) -> Result<(), SchemaError> {
        if mode == Compatibility::None {
            return Ok(());
        }
        let old = self.compiled_root(old_schema_text)?;
        let new = self.compiled_root(new_schema_text)?;

        if json_deep_eq(&old, &new) {
            return Ok(());
        }

        if !root_type_includes_object(&old) || !root_type_includes_object(&new) {
            return Err(SchemaError::Incompatible(
                "root schema 'type' must include \"object\" on both sides for structural \
                 compatibility comparison"
                    .to_string(),
            ));
        }

        check_shared_properties_differ_only_by_type(&old, &new)?;
        check_other_root_keys_are_unchanged(&old, &new)?;
        check_reachable_defs_are_unchanged(&old, &new)?;
        check_additional_properties_schema_form_is_unchanged(&old, &new)?;

        match mode {
            Compatibility::Backward => check_direction(&new, &old)?,
            Compatibility::Forward => check_direction(&old, &new)?,
            Compatibility::Full => {
                check_direction(&new, &old)?;
                check_direction(&old, &new)?;
            }
            Compatibility::None => unreachable!(),
        }
        Ok(())
    }
}

pub(super) static JSON_SCHEMA_OPS: JsonSchemaOps = JsonSchemaOps;

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const PATIENT_SCHEMA: &str = r##"{
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Patient",
        "type": "object",
        "$defs": {
            "PatientId": { "type": "string", "pattern": "^[A-Z0-9]{6,10}$" }
        },
        "properties": {
            "id": { "$ref": "#/$defs/PatientId" },
            "name": { "type": "string", "minLength": 1 },
            "dob": { "type": "string", "format": "date" },
            "age": { "type": "integer", "minimum": 0 },
            "tags": { "type": "array", "items": { "type": "string" }, "uniqueItems": true }
        },
        "required": ["id", "name"],
        "additionalProperties": false
    }"##;

    fn allowed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // --- compile: rejections -------------------------------------------

    #[test]
    fn compile_rejects_malformed_json() {
        let err = JSON_SCHEMA_OPS.compile("{ not json").unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(_)));
    }

    #[test]
    fn compile_rejects_non_object_root() {
        assert!(matches!(
            JSON_SCHEMA_OPS.compile("true").unwrap_err(),
            SchemaError::Invalid(_)
        ));
        assert!(matches!(
            JSON_SCHEMA_OPS.compile("false").unwrap_err(),
            SchemaError::Invalid(_)
        ));
        assert!(matches!(
            JSON_SCHEMA_OPS.compile("42").unwrap_err(),
            SchemaError::Invalid(_)
        ));
    }

    #[test]
    fn compile_rejects_unsupported_schema_draft() {
        let text = r#"{"$schema": "http://json-schema.org/draft-04/schema#", "type": "object"}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("draft")));
    }

    #[test]
    fn compile_rejects_oversized_schema_text() {
        let padding = "a".repeat(MAX_SCHEMA_TEXT_BYTES + 1);
        let text = format!(r#"{{"type": "object", "title": "{padding}"}}"#);
        let err = JSON_SCHEMA_OPS.compile(&text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("byte")));
    }

    #[test]
    fn compile_rejects_remote_http_ref() {
        let text =
            r#"{"type":"object","properties":{"x":{"$ref":"http://evil.example/schema.json"}}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(_)));
    }

    #[test]
    fn compile_rejects_relative_ref() {
        let text = r#"{"type":"object","properties":{"x":{"$ref":"other.json#/foo"}}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(_)));
    }

    #[test]
    fn compile_rejects_unresolvable_local_ref() {
        let text =
            r##"{"type":"object","$defs":{},"properties":{"x":{"$ref":"#/$defs/Missing"}}}"##;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("does not resolve")));
    }

    #[test]
    fn compile_rejects_ref_cycle() {
        let text = r##"{"type":"object","$defs":{"a":{"$ref":"#/$defs/a"}}}"##;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("cycle")));
    }

    #[test]
    fn compile_accepts_annotation_only_siblings_next_to_ref() {
        let text = r##"{"type":"object","$defs":{"Id":{"type":"string"}},
            "properties":{"id":{"$ref":"#/$defs/Id","title":"Identifier",
            "description":"An id","deprecated":false}}}"##;
        JSON_SCHEMA_OPS
            .compile(text)
            .expect("annotation-only siblings next to $ref should compile");
    }

    #[test]
    fn compile_rejects_a_validating_sibling_next_to_ref() {
        let text = r##"{"type":"object","$defs":{"Id":{"type":"string"}},
            "properties":{"id":{"$ref":"#/$defs/Id","minLength":3}}}"##;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m)
            if m.contains("'minLength'") && m.contains("'$ref'") && m.contains("/properties/id")));
    }

    #[test]
    fn compile_rejects_if_then_else() {
        let text = r#"{"type":"object","if":{"type":"object"}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("'if'")));
    }

    #[test]
    fn compile_rejects_pattern_properties() {
        let text = r#"{"type":"object","patternProperties":{"^S_":{"type":"string"}}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("'patternProperties'")));
    }

    #[test]
    fn compile_rejects_contains() {
        let text = r#"{"type":"array","contains":{"type":"string"}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("'contains'")));
    }

    #[test]
    fn compile_rejects_prefix_items() {
        let text = r#"{"type":"array","prefixItems":[{"type":"string"}]}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("'prefixItems'")));
    }

    #[test]
    fn compile_rejects_dependencies_keyword() {
        let text = r#"{"type":"object","dependencies":{"a":["b"]}}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("'dependencies'")));
    }

    #[test]
    fn compile_rejects_tuple_items() {
        let text = r#"{"type":"array","items":[{"type":"string"},{"type":"number"}]}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("array")));
    }

    #[test]
    fn compile_rejects_draft04_boolean_exclusive_minimum() {
        let text = r#"{"type":"number","exclusiveMinimum": true}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(_)));
    }

    #[test]
    fn compile_rejects_invalid_regex_pattern() {
        let text = r#"{"type":"string","pattern":"(unclosed"}"#;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("pattern")));
    }

    // --- compile: acceptance ---------------------------------------------

    #[test]
    fn compile_accepts_realistic_patient_schema_with_defs_ref_and_annotations() {
        JSON_SCHEMA_OPS
            .compile(PATIENT_SCHEMA)
            .expect("patient schema should compile");
    }

    // --- validate ---------------------------------------------------------

    fn compiled_patient() -> CompiledSchema {
        JSON_SCHEMA_OPS.compile(PATIENT_SCHEMA).unwrap()
    }

    #[test]
    fn validate_accepts_a_conforming_payload() {
        let c = compiled_patient();
        let payload =
            br#"{"id":"ABC1234","name":"Jan","dob":"2000-01-01","age":25,"tags":["a","b"]}"#;
        assert!(JSON_SCHEMA_OPS.validate(&c, payload).is_ok());
    }

    #[test]
    fn validate_rejects_missing_required_property() {
        let c = compiled_patient();
        let payload = br#"{"id":"ABC1234"}"#;
        let err = JSON_SCHEMA_OPS.validate(&c, payload).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("name")));
    }

    #[test]
    fn validate_rejects_wrong_type() {
        let c = compiled_patient();
        let payload = br#"{"id":"ABC1234","name":123}"#;
        let err = JSON_SCHEMA_OPS.validate(&c, payload).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("expected string")));
    }

    #[test]
    fn validate_rejects_integer_with_fraction() {
        let c = compiled_patient();
        let payload = br#"{"id":"ABC1234","name":"Jan","age":1.5}"#;
        let err = JSON_SCHEMA_OPS.validate(&c, payload).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("expected integer")));
    }

    #[test]
    fn validate_rejects_extra_key_when_additional_properties_is_false() {
        let c = compiled_patient();
        let payload = br#"{"id":"ABC1234","name":"Jan","ssn":"999-99-9999"}"#;
        let err = JSON_SCHEMA_OPS.validate(&c, payload).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("not allowed")));
    }

    #[test]
    fn validate_rejects_pattern_mismatch() {
        let c = compiled_patient();
        let payload = br#"{"id":"not-an-id","name":"Jan"}"#;
        let err = JSON_SCHEMA_OPS.validate(&c, payload).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("pattern")));
    }

    #[test]
    fn validate_rejects_enum_mismatch() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"string","enum":["a","b"]}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS.validate(&compiled, br#""c""#).unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("enum")));
    }

    #[test]
    fn validate_rejects_min_length_counting_unicode_scalar_values() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"string","minLength":3}"#)
            .unwrap();
        // 3 Unicode scalar values (9 bytes) satisfies minLength 3, proving
        // byte length is not what is counted.
        JSON_SCHEMA_OPS
            .validate(&compiled, "\"\u{2713}\u{2713}\u{2713}\"".as_bytes())
            .expect("3 scalar values should satisfy minLength 3 even though it is 9 bytes");
        // 2 Unicode scalar values, well under 3 even though it's 6 bytes.
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, "\"\u{2713}\u{2713}\"".as_bytes())
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("minLength 3")));
    }

    #[test]
    fn validate_rejects_duplicate_items_when_unique_items_is_set() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#"[1, 1]"#)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    #[test]
    fn validate_rejects_value_matching_two_one_of_branches() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"oneOf":[{"type":"string"},{"type":"string","minLength":0}]}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#""hello""#)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("oneOf")));
    }

    #[test]
    fn validate_rejects_dependent_required_violation() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"object","dependentRequired":{"insurance":["policyId"]}}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#"{"insurance":"acme"}"#)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("policyId")));
    }

    #[test]
    fn validate_rejects_non_object_payload_against_object_schema() {
        let compiled = JSON_SCHEMA_OPS.compile(r#"{"type":"object"}"#).unwrap();
        let err = JSON_SCHEMA_OPS.validate(&compiled, b"42").unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("expected object")));
    }

    #[test]
    fn validate_error_message_includes_a_json_pointer_path() {
        let compiled = JSON_SCHEMA_OPS
            .compile(
                r#"{"type":"object","properties":{"patient":{"type":"object",
                "properties":{"dob":{"type":"string"}}}}}"#,
            )
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#"{"patient":{"dob":123}}"#)
            .unwrap_err();
        let SchemaError::Violation(m) = err else {
            panic!("expected Violation")
        };
        assert!(m.starts_with("/patient/dob:"), "message was: {m}");
    }

    // --- derive_subschema ---------------------------------------------------

    #[test]
    fn derive_drops_properties_not_in_the_allowed_set() {
        let out = JSON_SCHEMA_OPS
            .derive_subschema(PATIENT_SCHEMA, &allowed(&["id", "name"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let props = v["properties"].as_object().unwrap();
        assert!(props.contains_key("id"));
        assert!(props.contains_key("name"));
        assert!(!props.contains_key("dob"));
        assert!(!props.contains_key("age"));
    }

    #[test]
    fn derive_intersects_required_with_the_allowed_set() {
        let out = JSON_SCHEMA_OPS
            .derive_subschema(PATIENT_SCHEMA, &allowed(&["id"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let required: Vec<&str> = v["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(required, vec!["id"]);
    }

    #[test]
    fn derive_forces_additional_properties_false() {
        let text = r#"{"type":"object","properties":{"a":{"type":"string"}}}"#; // absent originally
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["additionalProperties"], Value::Bool(false));
    }

    #[test]
    fn derive_is_byte_stable_across_two_calls() {
        let a = JSON_SCHEMA_OPS
            .derive_subschema(PATIENT_SCHEMA, &allowed(&["id", "name"]))
            .unwrap();
        let b = JSON_SCHEMA_OPS
            .derive_subschema(PATIENT_SCHEMA, &allowed(&["id", "name"]))
            .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn derive_fails_closed_on_unrewritable_dependent_required() {
        let text = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "dependentRequired":{"a":["b"]}}"#;
        // "a" survives (allowed), but it depends on "b" which is removed.
        let err = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a"]))
            .unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(_)));
    }

    #[test]
    fn derive_fails_closed_on_root_all_of() {
        let text = r#"{"type":"object","allOf":[{"required":["a"]}],"properties":{"a":{"type":"string"}}}"#;
        let err = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a"]))
            .unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("allOf")));
    }

    // --- check_compatibility -------------------------------------------------

    #[test]
    fn compatibility_none_is_always_ok_even_for_garbage_text() {
        assert!(JSON_SCHEMA_OPS
            .check_compatibility("not json at all", "also not json", Compatibility::None)
            .is_ok());
    }

    #[test]
    fn compatibility_backward_accepts_adding_an_optional_property() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"],
            "additionalProperties":false}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a"],"additionalProperties":false}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .is_ok());
    }

    #[test]
    fn compatibility_backward_rejects_adding_a_required_property() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"],
            "additionalProperties":false}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a","b"],"additionalProperties":false}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(_)));
    }

    #[test]
    fn compatibility_backward_rejects_deleting_a_property_when_new_forbids_additional_properties() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a"],"additionalProperties":false}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"],
            "additionalProperties":false}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(_)));
    }

    #[test]
    fn compatibility_backward_accepts_widening_integer_to_number() {
        let old = r#"{"type":"object","properties":{"a":{"type":"integer"}}}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"number"}}}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .is_ok());
    }

    #[test]
    fn compatibility_backward_rejects_narrowing_number_to_integer() {
        let old = r#"{"type":"object","properties":{"a":{"type":"number"}}}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"integer"}}}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(_)));
    }

    #[test]
    fn compatibility_forward_accepts_removing_an_optional_property() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a"]}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Forward)
            .is_ok());
    }

    #[test]
    fn compatibility_forward_rejects_removing_a_required_property() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a","b"]}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Forward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(_)));
    }

    #[test]
    fn compatibility_full_requires_both_directions_to_hold() {
        // Backward-only compatible: widening integer -> number breaks Forward
        // (an old, integer-typed reader cannot safely read new float data).
        let old = r#"{"type":"object","properties":{"a":{"type":"integer"}}}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"number"}}}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .is_ok());
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(_)));
    }

    #[test]
    fn compatibility_full_accepts_adding_an_optional_property_when_unrestricted() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"]}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "required":["a"]}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .is_ok());
    }

    #[test]
    fn compatibility_rejects_a_deep_subschema_change_beyond_type_widening() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string","minLength":1}}}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string","minLength":5}}}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(m) if m.contains("beyond")));
    }

    // --- finding 3: compatibility fail-closed on everything outside the ---
    // --- structural properties/required/type/additionalProperties check ---

    #[test]
    fn compatibility_rejects_a_ref_mediated_type_change_hidden_behind_an_unchanged_property() {
        // Both sides look identical at `properties.id = {"$ref": "#/$defs/Id"}`
        // — only `$defs.Id` itself changed, string -> integer. Without
        // comparing the `$defs` maps this would be silently accepted.
        let old = r##"{"type":"object","$defs":{"Id":{"type":"string"}},
            "properties":{"id":{"$ref":"#/$defs/Id"}}}"##;
        let new = r##"{"type":"object","$defs":{"Id":{"type":"integer"}},
            "properties":{"id":{"$ref":"#/$defs/Id"}}}"##;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(m) if m.contains("$defs")));
    }

    #[test]
    fn compatibility_rejects_an_added_dependent_required_entry() {
        let old = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}}}"#;
        let new = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"}},
            "dependentRequired":{"a":["b"]}}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(m) if m.contains("dependentRequired")));
    }

    #[test]
    fn compatibility_accepts_a_defs_only_change_when_the_def_is_unreferenced() {
        // Finding 5 (F3 second-pass review): `Extra` is not reachable from
        // ANY property on either side (there is no `properties` key at
        // all), so it has zero effect on `validate`'s behavior for data
        // written under either schema — changing it must not be flagged.
        // (Before the fix, `check_other_root_keys_are_unchanged` required
        // the whole `$defs` map to be byte-for-byte equal regardless of
        // reachability, which rejected this genuinely no-op change too.)
        let old = r##"{"type":"object","$defs":{"Extra":{"type":"string","maxLength":5}}}"##;
        let new = r##"{"type":"object","$defs":{"Extra":{"type":"string","maxLength":50}}}"##;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .is_ok());
    }

    #[test]
    fn compatibility_backward_accepts_an_additive_property_introducing_a_new_defs_entry() {
        // Finding 5: `new` adds an optional property "b" whose subschema is
        // factored out into a brand-new `$defs` entry ("B") instead of being
        // inlined. "B" is unreachable from `old`'s properties (`old` has no
        // `$ref` to it at all, since it didn't exist yet), so it must not be
        // required to match anything on the `old` side — this is the exact
        // "purely additive optional property... rejected... when it adds a
        // `$defs` entry" case the finding names; before the fix this was
        // rejected because `old` had no `$defs` key at all while `new` did.
        let old = r#"{"type":"object","properties":{"a":{"type":"string"}},"required":["a"],
            "additionalProperties":false}"#;
        let new = r##"{"type":"object","properties":{"a":{"type":"string"},
            "b":{"$ref":"#/$defs/B"}},"required":["a"],"additionalProperties":false,
            "$defs":{"B":{"type":"string"}}}"##;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Backward)
            .is_ok());
    }

    #[test]
    fn compatibility_rejects_a_min_properties_change() {
        let old = r#"{"type":"object","minProperties":1}"#;
        let new = r#"{"type":"object","minProperties":2}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(m) if m.contains("minProperties")));
    }

    #[test]
    fn compatibility_accepts_an_annotation_only_change() {
        let old = r#"{"type":"object","title":"Old title"}"#;
        let new = r#"{"type":"object","title":"New title"}"#;
        assert!(JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .is_ok());
    }

    #[test]
    fn compatibility_rejects_a_schema_form_additional_properties_change() {
        let old = r#"{"type":"object","additionalProperties":{"type":"string"}}"#;
        let new = r#"{"type":"object","additionalProperties":{"type":"number"}}"#;
        let err = JSON_SCHEMA_OPS
            .check_compatibility(old, new, Compatibility::Full)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Incompatible(m) if m.contains("additionalProperties")));
    }

    // --- finding 1: bounded validation work ------------------------------

    #[test]
    fn compile_rejects_a_ref_chain_whose_expansion_estimate_is_too_large() {
        // 30 `$defs`, each `{"allOf": [{"$ref": prev}, {"$ref": prev}]}` —
        // a naive unmemoized walk performs ~2^30 `validate_node` calls for a
        // schema text well under 4 KiB.
        let mut defs = String::from(r#""def0": {"type": "number"}"#);
        for i in 1..30 {
            let prev = i - 1;
            defs.push_str(&format!(
                r##", "def{i}": {{"allOf": [{{"$ref": "#/$defs/def{prev}"}}, {{"$ref": "#/$defs/def{prev}"}}]}}"##
            ));
        }
        let text = format!(
            r##"{{"type": "object", "$defs": {{{defs}}}, "properties": {{"x": {{"$ref": "#/$defs/def29"}}}}}}"##
        );
        assert!(text.len() < 4096, "schema text was {} bytes", text.len());
        let err = JSON_SCHEMA_OPS.compile(&text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m) if m.contains("expansion")));
    }

    #[test]
    fn validate_enforces_a_runtime_step_budget_on_a_large_payload() {
        // Compiles fine (tiny, non-explosive schema) — the array's runtime
        // cardinality, not anything visible at compile time, is what drives
        // the step count past the budget.
        let compiled = JSON_SCHEMA_OPS
            .compile(
                r#"{"type":"array","items":{"allOf":[
                    {"type":"number"},{"type":"number"},{"type":"number"},
                    {"type":"number"},{"type":"number"},{"type":"number"}]}}"#,
            )
            .unwrap();
        let huge_array = format!("[{}]", vec!["1"; 60_000].join(","));
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, huge_array.as_bytes())
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("step budget")));
    }

    // --- second-pass finding 1: `not` fails closed on step-budget ---------
    // --- exhaustion, instead of the exhausted inner walk's `Err` being ----
    // --- read as "not" being satisfied ------------------------------------

    #[test]
    fn not_fails_closed_when_the_step_budget_is_exhausted() {
        // The inner `not` schema's own walk (validating every item against
        // `{"type":"object"}`) exhausts `MAX_VALIDATION_STEPS` before it can
        // finish, so `validate_node(not_schema, ...)` returns `Err`. Before
        // the fix, `not` read that `Err` as "did not match", i.e. `not`
        // satisfied, and accepted the record outright — even though the
        // payload plainly violates `not` (every item IS an object).
        let compiled = JSON_SCHEMA_OPS
            .compile(
                r#"{"type":"object","properties":{"payload":{"not":
                    {"type":"array","items":{"type":"object"}}}}}"#,
            )
            .unwrap();
        let items = vec!["{}"; 200_001].join(",");
        let payload = format!(r#"{{"payload":[{items}]}}"#);
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, payload.as_bytes())
            .expect_err("a step-budget exhaustion inside 'not' must fail closed as a violation");
        assert!(matches!(err, SchemaError::Violation(_)));
    }

    // --- second-pass finding 2: deterministic `$defs` compile order, ------
    // --- and `$ref` cache hits still enforce MAX_SCHEMA_DEPTH -------------

    #[test]
    fn compile_of_an_unreferenced_deep_defs_chain_is_deterministic() {
        // a1 -> a2 -> ... -> a80, none referenced from the schema root. 80
        // exceeds MAX_SCHEMA_DEPTH (64), so this must always be rejected.
        // Before the fix, `ctx.defs`/the coverage pass walked a `HashMap` in
        // hash-seed-dependent order, so the same schema text could compile
        // on one run and fail on the next (or vice versa); a `BTreeMap`
        // makes the coverage pass always walk the chain the same way.
        let mut defs = String::from(r#""a80": {"type": "string"}"#);
        for i in (1..80).rev() {
            let next = i + 1;
            defs.push_str(&format!(r##", "a{i}": {{"$ref": "#/$defs/a{next}"}}"##));
        }
        let text = format!(r#"{{"type": "object", "$defs": {{{defs}}}}}"#);

        let results: Vec<Result<(), SchemaError>> = (0..20)
            .map(|_| JSON_SCHEMA_OPS.compile(&text).map(|_| ()))
            .collect();
        assert!(
            results.iter().all(|r| *r == results[0]),
            "compiling the same 80-long unreferenced $defs chain repeatedly produced different \
             results: {results:?}"
        );
        match &results[0] {
            Err(SchemaError::Invalid(m)) => assert!(
                m.contains("depth"),
                "expected a depth-exceeded rejection, got: {m}"
            ),
            other => panic!(
                "an 80-long $defs chain (> MAX_SCHEMA_DEPTH of 64) must always be rejected, got \
                 {other:?}"
            ),
        }
    }

    #[test]
    fn ref_cache_hit_still_enforces_max_depth() {
        // "Leaf" has its own internal nesting 20 levels deep (well within
        // MAX_SCHEMA_DEPTH (64) when first compiled shallowly via the
        // "shallow" property, at absolute depth ~22) and is cached there.
        // "deep" then reaches the SAME "Leaf" through a 49-hop `$ref` chain
        // that ALSO never exceeds 64 on its own (its own deepest node sits
        // at absolute depth 50). Neither the shallow "Leaf" compile nor the
        // "deep" chain alone violates MAX_SCHEMA_DEPTH — but reusing
        // "Leaf"'s cached, 20-levels-deep subtree re-rooted at depth 50
        // would push its innermost node to depth ~70. Before the fix, a
        // `$ref` cache hit returned the cached node with no depth
        // accounting at all, so this schema compiled successfully despite
        // that.
        let mut leaf = String::from(r#"{"type": "string"}"#);
        for _ in 0..20 {
            leaf = format!(r#"{{"type": "object", "properties": {{"x": {leaf}}}}}"#);
        }
        let mut defs = format!(r##""Leaf": {leaf}"##);
        for i in 1..49 {
            let next = i + 1;
            defs.push_str(&format!(r##", "d{i}": {{"$ref": "#/$defs/d{next}"}}"##));
        }
        defs.push_str(r##", "d49": {"$ref": "#/$defs/Leaf"}"##);
        let text = format!(
            r##"{{"type": "object", "$defs": {{{defs}}},
                "properties": {{"shallow": {{"$ref": "#/$defs/Leaf"}},
                "deep": {{"$ref": "#/$defs/d1"}}}}}}"##
        );
        let err = JSON_SCHEMA_OPS.compile(&text).unwrap_err();
        assert!(
            matches!(err, SchemaError::Invalid(ref m) if m.contains("depth")),
            "a $ref cache hit reused from a much deeper position must still enforce \
             MAX_SCHEMA_DEPTH even when neither side alone would exceed it: {err:?}"
        );
    }

    // --- finding 2: O(n) uniqueItems --------------------------------------

    #[test]
    fn unique_items_detects_duplicates_deep_inside_objects() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(
                &compiled,
                br#"[{"a":{"b":[1,2,{"c":true}]}}, {"a":{"b":[1,2,{"c":true}]}}]"#,
            )
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    #[test]
    fn unique_items_accepts_deeply_similar_but_distinct_objects() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        JSON_SCHEMA_OPS
            .validate(
                &compiled,
                br#"[{"a":{"b":[1,2,{"c":true}]}}, {"a":{"b":[1,2,{"c":false}]}}]"#,
            )
            .expect("objects differing deep inside must not be flagged as duplicates");
    }

    #[test]
    fn unique_items_detects_duplicates_whose_object_keys_are_in_different_order() {
        // This crate builds `serde_json` with `preserve_order` enabled
        // (transitively), so `Map` is an insertion-ordered `IndexMap` — a
        // canonicalization that relied on that iteration order instead of
        // explicitly sorting keys would treat these two objects (equal per
        // JSON Schema / `json_deep_eq`) as distinct and miss the duplicate.
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#"[{"a":1,"b":2}, {"b":2,"a":1}]"#)
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    #[test]
    fn unique_items_detects_duplicates_with_reordered_keys_nested_inside_an_array() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(
                &compiled,
                br#"[{"outer":{"a":1,"b":2}}, {"outer":{"b":2,"a":1}}]"#,
            )
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    #[test]
    fn unique_items_completes_quickly_on_a_large_array_of_repeated_values() {
        // A naive O(n^2) pairwise scan over an array this size would not
        // complete in any reasonable test time; this is a regression guard
        // for the DoS the O(n) canonicalization fix closes.
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let huge_array = format!("[{}]", vec!["1"; 200_000].join(","));
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, huge_array.as_bytes())
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    // --- finding 4: exact numeric equality above 2^53 ---------------------

    #[test]
    fn const_rejects_an_adjacent_integer_beyond_2_pow_53() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"const": 9007199254740993}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, b"9007199254740992")
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("const")));
    }

    #[test]
    fn const_accepts_the_exact_large_integer_match() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"const": 9007199254740993}"#)
            .unwrap();
        JSON_SCHEMA_OPS
            .validate(&compiled, b"9007199254740993")
            .expect("exact match on a large integer must be accepted");
    }

    // --- second-pass finding 6: uniqueItems canonicalization must agree ---
    // --- with number_eq above 2^53 -----------------------------------------

    #[test]
    fn unique_items_treats_a_large_integer_and_its_float_rounding_as_duplicates() {
        // `9007199254740993` (an exact i64 literal) and `9007199254740992.0`
        // (a float literal) are different integers, but `number_eq`'s
        // fallback for this pair (neither side has a matching exact-integer
        // representation) compares them via `as_f64`, which rounds the odd
        // `9007199254740993` down to the same double as
        // `9007199254740992.0` — so `number_eq`/`const`/`enum` all treat
        // them as equal. Confirm that premise directly, then confirm
        // `uniqueItems` agrees: before the fix, canonicalization kept the
        // integer literal's own exact text, canonicalizing the two values
        // differently and missing the duplicate.
        assert!(number_eq(
            &Number::from(9007199254740993i64),
            &Number::from_f64(9007199254740992.0).unwrap()
        ));
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"array","uniqueItems":true}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, b"[9007199254740993, 9007199254740992.0]")
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("unique")));
    }

    // --- finding 5: multipleOf ---------------------------------------------

    #[test]
    fn multiple_of_rejects_a_huge_float_that_cannot_be_verified() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"number","multipleOf":7}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS.validate(&compiled, b"1e30").unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("multipleOf")));
    }

    #[test]
    fn multiple_of_accepts_an_exact_integer_multiple_of_a_float_divisor() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"number","multipleOf":0.1}"#)
            .unwrap();
        JSON_SCHEMA_OPS
            .validate(&compiled, b"10")
            .expect("10 is a multiple of 0.1 within float tolerance");
    }

    #[test]
    fn multiple_of_accepts_within_float_tolerance() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"number","multipleOf":0.1}"#)
            .unwrap();
        JSON_SCHEMA_OPS
            .validate(&compiled, b"0.3")
            .expect("0.3 is a multiple of 0.1 within float tolerance");
    }

    #[test]
    fn multiple_of_rejects_a_non_finite_ratio() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"number","multipleOf":1e-300}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, b"1.7976931348623157e308")
            .unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("multipleOf")));
    }

    #[test]
    fn multiple_of_rejects_an_exact_integer_non_multiple() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"integer","multipleOf":7}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS.validate(&compiled, b"10").unwrap_err();
        assert!(matches!(err, SchemaError::Violation(m) if m.contains("multipleOf")));
    }

    // --- finding 6: derive_subschema defs pruning + property-count clamp --

    #[test]
    fn derive_prunes_unreachable_defs_and_keeps_transitively_reachable_ones() {
        let text = r##"{
            "type": "object",
            "$defs": {
                "SSN": {"type": "string", "pattern": "^[0-9]{9}$"},
                "A": {"$ref": "#/$defs/B"},
                "B": {"type": "string"}
            },
            "properties": {
                "id": {"$ref": "#/$defs/A"},
                "ssn": {"$ref": "#/$defs/SSN"}
            }
        }"##;
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["id"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let defs = v["$defs"].as_object().unwrap();
        assert!(
            defs.contains_key("A"),
            "directly-referenced def must survive"
        );
        assert!(
            defs.contains_key("B"),
            "transitively-referenced def ($defs/A -> $defs/B) must survive"
        );
        assert!(
            !defs.contains_key("SSN"),
            "def only reachable via a dropped property must be pruned"
        );
    }

    #[test]
    fn derive_removes_the_defs_key_entirely_when_nothing_survives() {
        let text = r##"{"type":"object","$defs":{"SSN":{"type":"string"}},
            "properties":{"id":{"type":"string"},"ssn":{"$ref":"#/$defs/SSN"}}}"##;
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["id"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("$defs").is_none());
    }

    #[test]
    fn derive_clamps_min_and_max_properties_to_the_allowed_set_size() {
        let text = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"},
            "c":{"type":"string"}},"minProperties":3,"maxProperties":3}"#;
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a", "b"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["minProperties"], Value::Number(2.into()));
        assert_eq!(v["maxProperties"], Value::Number(2.into()));
    }

    // --- second-pass finding 3: derive_subschema must not leave a dangling
    // --- $ref when a non-`properties` root keyword (e.g. `items`) carries one

    #[test]
    fn derive_does_not_leave_a_dangling_ref_from_a_non_properties_root_keyword() {
        // `items` is a root keyword copied into the projection verbatim; its
        // `$ref` target must be kept alive by the reachability pass even
        // though it is not reachable through `properties` at all. Before the
        // fix, reachability was seeded only from (surviving) `properties`,
        // so `$defs/Item` was pruned out from under the still-present
        // `items: {"$ref": "#/$defs/Item"}`, leaving a dangling reference
        // that failed to recompile.
        let text = r##"{"properties":{"a":{"type":"string"}},
            "items":{"$ref":"#/$defs/Item"},"$defs":{"Item":{"type":"object"}}}"##;
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a"]))
            .unwrap();
        JSON_SCHEMA_OPS
            .compile(&out)
            .expect("derived schema must not contain a dangling $ref to a pruned $defs entry");
    }

    // --- second-pass finding 4: min/maxProperties must clamp against the --
    // --- surviving `properties` count, not `allowed.len()` ----------------

    #[test]
    fn derive_clamps_min_properties_to_the_surviving_property_count_not_the_allowed_set_size() {
        // `allowed` names "legacy_field", which the source schema never
        // declares under `properties` at all — it grants no room to satisfy
        // `additionalProperties: false` (forced on by `derive_subschema`),
        // since it was never eligible to survive `properties.retain`. Only
        // "a" actually survives, so `minProperties` must clamp to 1, not to
        // `allowed.len()` (2) — clamping to 2 would leave an unsatisfiable
        // projection (needs 2 properties, but only 1 can ever be present).
        let text = r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"string"},
            "c":{"type":"string"}},"minProperties":3}"#;
        let out = JSON_SCHEMA_OPS
            .derive_subschema(text, &allowed(&["a", "legacy_field"]))
            .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["minProperties"],
            Value::Number(1.into()),
            "minProperties must clamp to the 1 surviving property ('a'), not allowed.len() (2)"
        );
    }

    // --- finding 7: violation messages never embed payload content --------

    #[test]
    fn violation_messages_never_embed_the_offending_numeric_value() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"object","properties":{"x":{"type":"integer","minimum":0}}}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#"{"x":-12345678901}"#)
            .unwrap_err();
        let SchemaError::Violation(m) = err else {
            panic!("expected Violation")
        };
        assert!(
            !m.contains("12345678901"),
            "message leaked the payload value: {m}"
        );
        assert!(m.contains("minimum 0"), "message was: {m}");
    }

    #[test]
    fn violation_messages_never_embed_the_pattern_text_or_matched_string() {
        let compiled = JSON_SCHEMA_OPS
            .compile(r#"{"type":"string","pattern":"^SECRET-[0-9]+$"}"#)
            .unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, br#""not-a-match-classified""#)
            .unwrap_err();
        let SchemaError::Violation(m) = err else {
            panic!("expected Violation")
        };
        assert!(!m.contains("SECRET"), "message leaked the pattern: {m}");
        assert!(
            !m.contains("not-a-match-classified"),
            "message leaked the payload string: {m}"
        );
    }

    #[test]
    fn violation_message_for_invalid_json_payload_does_not_echo_the_payload() {
        let compiled = JSON_SCHEMA_OPS.compile(r#"{"type":"object"}"#).unwrap();
        let err = JSON_SCHEMA_OPS
            .validate(&compiled, b"{ not json \"secret-token\"")
            .unwrap_err();
        let SchemaError::Violation(m) = err else {
            panic!("expected Violation")
        };
        assert!(
            !m.contains("secret-token"),
            "message leaked the payload: {m}"
        );
    }

    // --- finding 9: root-level $ref -----------------------------------------

    #[test]
    fn compile_accepts_a_root_level_ref_with_defs_as_the_only_other_key() {
        let text = r##"{"$defs": {"Root": {"type": "object",
            "properties": {"a": {"type": "string"}}}}, "$ref": "#/$defs/Root"}"##;
        let compiled = JSON_SCHEMA_OPS
            .compile(text)
            .expect("root-level $ref with $defs as its only other key must compile");
        JSON_SCHEMA_OPS
            .validate(&compiled, br#"{"a":"x"}"#)
            .expect("payload matching the referenced root schema should validate");
    }

    #[test]
    fn compile_rejects_a_validating_sibling_next_to_a_root_level_ref() {
        let text = r##"{"$defs": {"Root": {"type": "object"}},
            "$ref": "#/$defs/Root", "type": "object"}"##;
        let err = JSON_SCHEMA_OPS.compile(text).unwrap_err();
        assert!(matches!(err, SchemaError::Invalid(m)
            if m.contains("'type'") && m.contains("'$ref'")));
    }
}
