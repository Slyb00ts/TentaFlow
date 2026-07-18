// ===== File: schema.rs — JSON Schema (common subset) → grammar AST =====
// Converts a JSON Schema into grammar rules whose language is exactly the set
// of JSON documents conforming to the schema. Supported: object (properties,
// required, nested), array (items), string (+ `pattern` via the regex
// converter), number, integer, boolean, null, enum, const. Documented
// limitations (see INFER_CONFIGURATION.md): `additionalProperties: false` is
// not separately enforced (unlisted keys are simply not generated);
// `anyOf`/`oneOf`/`$ref`/tuple `items` are not supported; a property with no
// explicit `required` list is treated as required (the constraint forces a
// canonical, schema-valid form rather than accepting every ordering).

use forge_types::{ForgeError, Result};
use serde_json::Value;

use crate::builder::{AstRule, Item};
use crate::gbnf;

/// Shared JSON prelude rules referenced by generated schema rules.
// `ws` is a single OPTIONAL whitespace char, not `[...]*`: an unbounded
// insignificant-whitespace rule lets a greedy model stall forever emitting
// spaces (it never has to commit to a structural token). One optional space
// keeps the output readable while guaranteeing forward progress.
const PRELUDE: &str = r#"
ws ::= [ \t\n\r]?
js-string ::= "\"" js-char* "\""
js-char ::= [^"\\] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F])
js-integer ::= "-"? ("0" | [1-9] [0-9]*)
js-number ::= "-"? ("0" | [1-9] [0-9]*) ("." [0-9]+)? ([eE] [-+]? [0-9]+)?
js-boolean ::= "true" | "false"
js-null ::= "null"
js-value ::= js-object | js-array | js-string | js-number | js-boolean | js-null
js-member ::= js-string ws ":" ws js-value
js-object ::= "{" ws (js-member (ws "," ws js-member)*)? ws "}"
js-array ::= "[" ws (js-value (ws "," ws js-value)*)? ws "]"
"#;

/// Accumulates the rules generated for one or more schemas, so several tool
/// schemas can share one prelude and be assembled under a custom root.
pub struct SchemaConverter {
    rules: Vec<AstRule>,
    counter: usize,
}

impl Default for SchemaConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaConverter {
    pub fn new() -> Self {
        let rules = gbnf::parse(PRELUDE).expect("static JSON prelude parses");
        Self { rules, counter: 0 }
    }

    /// Add a custom rule (e.g. an assembled root).
    pub fn push_rule(&mut self, rule: AstRule) {
        self.rules.push(rule);
    }

    pub fn into_rules(self) -> Vec<AstRule> {
        self.rules
    }

    fn fresh(&mut self, hint: &str) -> String {
        self.counter += 1;
        format!("sc-{hint}-{}", self.counter)
    }

    /// Reserve a unique rule name (used by the regex converter when embedding
    /// a `string.pattern` sub-rule).
    pub fn reserve_name(&mut self, hint: &str) -> String {
        self.fresh(hint)
    }

    /// Build (or reuse) a rule matching `schema` and return its name to
    /// reference from a parent rule.
    pub fn value_rule(&mut self, schema: &Value) -> Result<String> {
        let obj = match schema {
            Value::Object(m) => m,
            Value::Bool(true) => return Ok("js-value".into()),
            Value::Bool(false) => {
                return Err(ForgeError::Grammar("schema `false` matches nothing".into()))
            }
            _ => return Err(ForgeError::Grammar("schema must be an object".into())),
        };

        if let Some(c) = obj.get("const") {
            let name = self.fresh("const");
            let lit = Item::literal(&serde_json::to_string(c).expect("json"));
            self.rules.push(AstRule {
                name: name.clone(),
                alternates: vec![vec![lit]],
            });
            return Ok(name);
        }
        if let Some(Value::Array(vals)) = obj.get("enum") {
            let name = self.fresh("enum");
            let alternates = vals
                .iter()
                .map(|v| vec![Item::literal(&serde_json::to_string(v).expect("json"))])
                .collect();
            self.rules.push(AstRule { name: name.clone(), alternates });
            return Ok(name);
        }

        let ty = match obj.get("type") {
            Some(Value::String(s)) => s.as_str(),
            Some(Value::Array(a)) => a
                .first()
                .and_then(|v| v.as_str())
                .ok_or_else(|| ForgeError::Grammar("empty `type` array".into()))?,
            None => return Ok("js-value".into()),
            _ => return Err(ForgeError::Grammar("`type` must be a string or array".into())),
        };

        match ty {
            "string" => self.string_rule(obj),
            "integer" => Ok("js-integer".into()),
            "number" => Ok("js-number".into()),
            "boolean" => Ok("js-boolean".into()),
            "null" => Ok("js-null".into()),
            "object" => self.object_rule(obj),
            "array" => self.array_rule(obj),
            other => Err(ForgeError::Grammar(format!("unsupported schema type `{other}`"))),
        }
    }

    fn string_rule(&mut self, obj: &serde_json::Map<String, Value>) -> Result<String> {
        let Some(Value::String(pattern)) = obj.get("pattern") else {
            return Ok("js-string".into());
        };
        // `pattern` constrains the string CONTENT; embed a regex-derived rule
        // between JSON quotes. The pattern's alphabet must not itself need JSON
        // escaping (documented limitation).
        let content = crate::regex::convert_into(self, pattern)?;
        let name = self.fresh("patstr");
        self.rules.push(AstRule {
            name: name.clone(),
            alternates: vec![vec![
                Item::literal("\""),
                Item::Ref(content),
                Item::literal("\""),
            ]],
        });
        Ok(name)
    }

    fn array_rule(&mut self, obj: &serde_json::Map<String, Value>) -> Result<String> {
        let Some(items) = obj.get("items") else {
            return Ok("js-array".into());
        };
        if items.is_array() {
            return Err(ForgeError::Grammar(
                "tuple-form `items` arrays are not supported".into(),
            ));
        }
        let itemrule = self.value_rule(items)?;
        let name = self.fresh("arr");
        // "[" ws ( item ( ws "," ws item )* )? ws "]"
        let tail = Item::Repeat {
            item: Box::new(Item::Group(vec![vec![
                Item::Ref("ws".into()),
                Item::literal(","),
                Item::Ref("ws".into()),
                Item::Ref(itemrule.clone()),
            ]])),
            min: 0,
            max: None,
        };
        let body = Item::Repeat {
            item: Box::new(Item::Group(vec![vec![Item::Ref(itemrule), tail]])),
            min: 0,
            max: Some(1),
        };
        self.rules.push(AstRule {
            name: name.clone(),
            alternates: vec![vec![
                Item::literal("["),
                Item::Ref("ws".into()),
                body,
                Item::Ref("ws".into()),
                Item::literal("]"),
            ]],
        });
        Ok(name)
    }

    fn object_rule(&mut self, obj: &serde_json::Map<String, Value>) -> Result<String> {
        let Some(Value::Object(props)) = obj.get("properties") else {
            return Ok("js-object".into());
        };
        if props.is_empty() {
            return Ok("js-object".into());
        }
        // Required order: explicit `required` array, else every property.
        let explicit_required: Vec<String> = match obj.get("required") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            _ => props.keys().cloned().collect(),
        };
        let required: Vec<String> = explicit_required
            .iter()
            .filter(|k| props.contains_key(*k))
            .cloned()
            .collect();
        let optional: Vec<String> = props
            .keys()
            .filter(|k| !required.contains(*k))
            .cloned()
            .collect();

        // With no required keys but present optionals, a leading-comma-free
        // regular encoding needs alternation over "which key is first"; fall
        // back to a generic object (documented).
        if required.is_empty() {
            return Ok("js-object".into());
        }

        let mut seq: Vec<Item> = vec![Item::literal("{"), Item::Ref("ws".into())];
        for (i, key) in required.iter().enumerate() {
            if i > 0 {
                seq.push(Item::Ref("ws".into()));
                seq.push(Item::literal(","));
                seq.push(Item::Ref("ws".into()));
            }
            let vr = self.value_rule(&props[key])?;
            seq.extend(self.member_items(key, &vr));
        }
        // Independent optional members: each carries its own leading comma
        // (always preceded by a required member, so this is valid JSON).
        for key in &optional {
            let vr = self.value_rule(&props[key])?;
            let mut member = vec![Item::Ref("ws".into()), Item::literal(","), Item::Ref("ws".into())];
            member.extend(self.member_items(key, &vr));
            seq.push(Item::Repeat {
                item: Box::new(Item::Group(vec![member])),
                min: 0,
                max: Some(1),
            });
        }
        seq.push(Item::Ref("ws".into()));
        seq.push(Item::literal("}"));

        let name = self.fresh("obj");
        self.rules.push(AstRule {
            name: name.clone(),
            alternates: vec![seq],
        });
        Ok(name)
    }

    fn member_items(&self, key: &str, value_rule: &str) -> Vec<Item> {
        vec![
            Item::literal(&serde_json::to_string(key).expect("json string")),
            Item::Ref("ws".into()),
            Item::literal(":"),
            Item::Ref("ws".into()),
            Item::Ref(value_rule.to_string()),
        ]
    }
}

/// Convert a whole schema into grammar rules with root rule `root`.
pub fn convert(schema: &Value) -> Result<Vec<AstRule>> {
    let mut c = SchemaConverter::new();
    let vr = c.value_rule(schema)?;
    // No leading/trailing insignificant whitespace: the value must start
    // immediately so a greedy model cannot stall before committing.
    c.push_rule(AstRule {
        name: "root".into(),
        alternates: vec![vec![Item::Ref(vr)]],
    });
    Ok(c.into_rules())
}
