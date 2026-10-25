// ===== File: tentaflow-codec-core/build.rs — derives the wire schema table from the protocol sources =====
//
// The browser neither decodes nor encodes bodies through a typed serde impl, so it
// cannot know from the bytes alone which map is a struct (keys get camelCased) and
// which is user data (keys must survive untouched), where a tag path ends and the
// payload starts, nor what type a JSON number or string has to become on the wire.
// This script reads the Rust definitions once and emits that knowledge as static
// data. Anything it cannot place (an unknown external type, an ambiguous name, a
// serde feature it does not model) fails the build instead of guessing.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use syn::{Fields, Item};

const ROOT_ENUM: &str = "MessageBody";
const PROTOCOL_SRC: &str = "../tentaflow-protocol/src";

fn scalar(name: &str) -> Option<Ty> {
    Some(match name {
        "String" | "str" | "char" => Ty::Str,
        "bool" => Ty::Bool,
        "u8" => Ty::Uint(8),
        "u16" => Ty::Uint(16),
        "u32" => Ty::Uint(32),
        "u64" | "usize" => Ty::Uint(64),
        "u128" => Ty::Uint(128),
        "i8" => Ty::Sint(8),
        "i16" => Ty::Sint(16),
        "i32" => Ty::Sint(32),
        "i64" | "isize" => Ty::Sint(64),
        "i128" => Ty::Sint(128),
        "f32" => Ty::Float(32),
        "f64" => Ty::Float(64),
        "Value" => Ty::Json,
        _ => return None,
    })
}

#[derive(Clone, PartialEq, Eq)]
enum Ty {
    Str,
    Bool,
    /// A unit struct: `null` on the wire.
    Unit,
    Uint(u8),
    Sint(u8),
    Float(u8),
    /// `serde_json::Value`: user data whose keys and shape stay as sent.
    Json,
    /// `Vec<u8>` behind `serde_bytes`: a CBOR byte string on the wire.
    Bytes,
    Named(usize),
    Opt(Box<Ty>),
    Seq(Box<Ty>),
    Map(Box<Ty>),
    Tuple(Vec<Ty>),
}

const REQUIRED: u8 = 1;
/// An absent `Option` still travels as `null` unless `skip_serializing_if` drops it:
/// a field with a custom `deserialize_with` is required even when it is nullable.
const NULL_WHEN_ABSENT: u8 = 2;
/// An explicit `null` is a value and must reach the wire: `Option<Option<T>>` ("clear")
/// and a field whose own `deserialize_with` reads a present `null` as `Some(null)`.
const KEEP_NULL: u8 = 4;

struct Field {
    name: String,
    ty: Ty,
    flags: u8,
}

enum Shape {
    Unit,
    Newtype(Ty),
    Struct(Vec<Field>),
}

enum Def {
    Struct {
        fields: Vec<Field>,
        deny_unknown: bool,
    },
    Enum {
        tag: Option<String>,
        content: Option<String>,
        routing: bool,
        deny_unknown: bool,
        variants: Vec<(String, Shape)>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TypeKey(usize);

struct Raw {
    module: String,
    item: Item,
}

struct Universe {
    raws: Vec<Raw>,
    by_ident: BTreeMap<String, Vec<usize>>,
    /// Per module: imported ident -> path segments it was imported from.
    imports: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

fn main() {
    println!("cargo:rerun-if-changed={PROTOCOL_SRC}");
    println!("cargo:rerun-if-changed=build.rs");

    let universe = load_universe();
    let mut builder = Builder { universe: &universe, ids: BTreeMap::new(), order: Vec::new(), resolving: BTreeSet::new() };
    let root_candidates = universe.by_ident.get(ROOT_ENUM).unwrap_or_else(|| panic!("{ROOT_ENUM} not found"));
    assert_eq!(root_candidates.len(), 1, "{ROOT_ENUM} is defined more than once");
    let root = builder.id_of(TypeKey(root_candidates[0]));

    let mut defs: Vec<Option<Def>> = Vec::new();
    let mut cursor = 0;
    while cursor < builder.order.len() {
        let key = builder.order[cursor];
        let def = builder.define(key);
        defs.push(Some(def));
        cursor += 1;
    }

    let mut out = String::new();
    writeln!(out, "pub(crate) const ROOT: u16 = {root};").unwrap();
    writeln!(out, "pub(crate) static DEFS: &[Def] = &[").unwrap();
    for def in defs.iter().flatten() {
        writeln!(out, "    {},", def_expr(def)).unwrap();
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("wire_schema.rs");
    std::fs::write(dest, out).unwrap();
}

// ----- loading ---------------------------------------------------------------

fn load_universe() -> Universe {
    let mut files = Vec::new();
    collect_rs_files(Path::new(PROTOCOL_SRC), &mut files);
    files.sort();
    let mut universe = Universe { raws: Vec::new(), by_ident: BTreeMap::new(), imports: BTreeMap::new() };
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let parsed = syn::parse_file(&text).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let module = module_of(&file);
        gather(&parsed.items, &module, &mut universe);
    }
    universe
}

fn module_of(file: &Path) -> String {
    let relative = file.strip_prefix(PROTOCOL_SRC).unwrap().with_extension("");
    let mut parts: Vec<String> = relative.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    if parts.last().is_some_and(|p| p == "mod") {
        parts.pop();
    }
    parts.join("::")
}

fn gather(items: &[Item], module: &str, universe: &mut Universe) {
    for item in items {
        match item {
            Item::Struct(s) if !is_cfg_test(&s.attrs) => register(universe, module, &s.ident.to_string(), item),
            Item::Enum(e) if !is_cfg_test(&e.attrs) => register(universe, module, &e.ident.to_string(), item),
            Item::Use(u) => {
                let mut flat = Vec::new();
                flatten_use(&u.tree, &mut Vec::new(), &mut flat);
                let entry = universe.imports.entry(module.to_string()).or_default();
                for (ident, path) in flat {
                    entry.insert(ident, path);
                }
            }
            Item::Mod(m) if !is_cfg_test(&m.attrs) => {
                if let Some((_, inner)) = &m.content {
                    gather(inner, module, universe);
                }
            }
            _ => {}
        }
    }
}

fn register(universe: &mut Universe, module: &str, ident: &str, item: &Item) {
    let index = universe.raws.len();
    universe.raws.push(Raw { module: module.to_string(), item: item.clone() });
    universe.by_ident.entry(ident.to_string()).or_default().push(index);
}

fn flatten_use(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
    match tree {
        syn::UseTree::Path(p) => {
            prefix.push(p.ident.to_string());
            flatten_use(&p.tree, prefix, out);
            prefix.pop();
        }
        syn::UseTree::Name(n) => {
            let mut path = prefix.clone();
            path.push(n.ident.to_string());
            out.push((n.ident.to_string(), path));
        }
        syn::UseTree::Rename(r) => {
            let mut path = prefix.clone();
            path.push(r.ident.to_string());
            out.push((r.rename.to_string(), path));
        }
        syn::UseTree::Group(g) => {
            for inner in &g.items {
                flatten_use(inner, prefix, out);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

// ----- serde attributes ------------------------------------------------------

fn serde_pairs(attrs: &[syn::Attribute]) -> Vec<(String, Option<String>)> {
    let mut pairs = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("serde")) {
        let _ = attr.parse_nested_meta(|meta| {
            let key = meta.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
            let mut value = None;
            if meta.input.peek(syn::Token![=]) {
                if let syn::Lit::Str(s) = meta.value()?.parse::<syn::Lit>()? {
                    value = Some(s.value());
                }
            }
            pairs.push((key, value));
            Ok(())
        });
    }
    pairs
}

fn serde_value(attrs: &[syn::Attribute], key: &str) -> Option<String> {
    serde_pairs(attrs).into_iter().find(|(k, _)| k == key).and_then(|(_, v)| v)
}

fn derives_serialize(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("derive")
            && a.meta.require_list().map(|l| l.tokens.to_string().split(|c: char| !c.is_alphanumeric()).any(|w| w.ends_with("Serialize"))).unwrap_or(false)
    })
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let mut found = false;
        if a.path().is_ident("cfg") {
            let _ = a.parse_nested_meta(|meta| {
                found |= meta.path.is_ident("test");
                Ok(())
            });
        }
        found
    })
}

/// serde's `rename_all = "snake_case"` on a PascalCase variant name.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ----- resolution ------------------------------------------------------------

struct Builder<'a> {
    universe: &'a Universe,
    ids: BTreeMap<TypeKey, usize>,
    order: Vec<TypeKey>,
    resolving: BTreeSet<TypeKey>,
}

impl Builder<'_> {
    fn id_of(&mut self, key: TypeKey) -> usize {
        if let Some(id) = self.ids.get(&key) {
            return *id;
        }
        let id = self.order.len();
        self.ids.insert(key, id);
        self.order.push(key);
        id
    }

    fn name_of(&self, key: TypeKey) -> String {
        match &self.universe.raws[key.0].item {
            Item::Struct(s) => s.ident.to_string(),
            Item::Enum(e) => e.ident.to_string(),
            _ => unreachable!(),
        }
    }

    fn resolve_ident(&self, ident: &str, path_hint: &[String], from_module: &str) -> TypeKey {
        let candidates = self
            .universe
            .by_ident
            .get(ident)
            .unwrap_or_else(|| panic!("type `{ident}` (used in module `{from_module}`) is neither defined in the protocol nor a known scalar/container"));
        if candidates.len() == 1 {
            return TypeKey(candidates[0]);
        }
        let in_module = |hint: &str| {
            let matching: Vec<usize> = candidates
                .iter()
                .copied()
                .filter(|c| {
                    let module = &self.universe.raws[*c].module;
                    module == hint || module.ends_with(&format!("::{hint}"))
                })
                .collect();
            (matching.len() == 1).then(|| TypeKey(matching[0]))
        };
        let same: Vec<usize> = candidates.iter().copied().filter(|c| self.universe.raws[*c].module == from_module).collect();
        if same.len() == 1 {
            return TypeKey(same[0]);
        }
        let prefixed = path_hint.iter().rev().find(|s| !matches!(s.as_str(), "self" | "super" | "crate"));
        if let Some(found) = prefixed.and_then(|hint| in_module(hint)) {
            return found;
        }
        if let Some(path) = self.universe.imports.get(from_module).and_then(|m| m.get(ident)) {
            let hint = path.iter().rev().skip(1).find(|s| !matches!(s.as_str(), "self" | "super" | "crate"));
            if let Some(found) = hint.and_then(|hint| in_module(hint)) {
                return found;
            }
        }
        panic!("type name `{ident}` is defined in several modules and `{from_module}` does not say which one");
    }

    fn resolve_ty(&mut self, ty: &syn::Type, from_module: &str) -> Ty {
        match ty {
            syn::Type::Path(p) => {
                let hint: Vec<String> = p.path.segments.iter().rev().skip(1).map(|s| s.ident.to_string()).collect();
                let last = p.path.segments.last().unwrap();
                let ident = last.ident.to_string();
                let args: Vec<&syn::Type> = match &last.arguments {
                    syn::PathArguments::AngleBracketed(a) => a
                        .args
                        .iter()
                        .filter_map(|g| if let syn::GenericArgument::Type(t) = g { Some(t) } else { None })
                        .collect(),
                    _ => Vec::new(),
                };
                match ident.as_str() {
                    "Box" | "Rc" | "Arc" | "Cow" => self.resolve_ty(args[0], from_module),
                    "Option" => opt(self.resolve_ty(args[0], from_module)),
                    "Vec" | "VecDeque" | "HashSet" | "BTreeSet" => Ty::Seq(Box::new(self.resolve_ty(args[0], from_module))),
                    "BTreeMap" | "HashMap" | "IndexMap" => {
                        assert!(
                            self.resolve_ty(args[0], from_module) == Ty::Str,
                            "map with non-text keys in module `{from_module}` is not modelled by the wire schema"
                        );
                        Ty::Map(Box::new(self.resolve_ty(args[1], from_module)))
                    }
                    name => match scalar(name) {
                        Some(ty) => ty,
                        None => {
                            let key = self.resolve_ident(name, &hint, from_module);
                            self.named(key)
                        }
                    },
                }
            }
            syn::Type::Array(a) => Ty::Seq(Box::new(self.resolve_ty(&a.elem, from_module))),
            syn::Type::Slice(s) => Ty::Seq(Box::new(self.resolve_ty(&s.elem, from_module))),
            syn::Type::Reference(r) => self.resolve_ty(&r.elem, from_module),
            syn::Type::Paren(p) => self.resolve_ty(&p.elem, from_module),
            syn::Type::Group(g) => self.resolve_ty(&g.elem, from_module),
            syn::Type::Tuple(t) => Ty::Tuple(t.elems.iter().map(|e| self.resolve_ty(e, from_module)).collect()),
            _ => panic!("unsupported field type kind in module `{from_module}` (only paths, arrays, slices, references and tuples are modelled)"),
        }
    }

    /// A reference to a defined type. Newtype structs are their inner type on the
    /// wire (`#[serde(transparent)]` or not), so they never get an entry of their own.
    fn named(&mut self, key: TypeKey) -> Ty {
        let module = self.universe.raws[key.0].module.clone();
        match &self.universe.raws[key.0].item.clone() {
            Item::Struct(s) => match &s.fields {
                Fields::Unit => Ty::Unit,
                Fields::Unnamed(u) if u.unnamed.len() == 1 => {
                    assert!(
                        self.resolving.insert(key),
                        "newtype struct `{}` contains itself without indirection",
                        self.name_of(key)
                    );
                    let inner = self.resolve_ty(&u.unnamed[0].ty, &module);
                    self.resolving.remove(&key);
                    inner
                }
                _ => Ty::Named(self.id_of(key)),
            },
            Item::Enum(_) => Ty::Named(self.id_of(key)),
            _ => unreachable!(),
        }
    }

    fn fields_of(&mut self, fields: &Fields, module: &str, owner: &str, container_default: bool) -> Vec<Field> {
        let Fields::Named(named) = fields else { return Vec::new() };
        let mut out = Vec::new();
        for field in &named.named {
            let name = field.ident.as_ref().unwrap().to_string();
            let pairs = serde_pairs(&field.attrs);
            let mut bytes = false;
            for (key, value) in &pairs {
                match key.as_str() {
                    "default" | "skip_serializing_if" | "serialize_with" | "deserialize_with" => {}
                    "with" => bytes |= value.as_deref() == Some("serde_bytes"),
                    other => panic!("{owner}.{name}: field attribute `{other}` is not modelled by the wire schema"),
                }
            }
            let has = |key: &str| pairs.iter().any(|(k, _)| k == key);
            let nullable = is_option(&field.ty);
            let mut ty = self.resolve_ty(&field.ty, module);
            if bytes {
                ty = bytesify(ty, &format!("{owner}.{name}"));
            }
            let mut flags = 0;
            if !nullable && !has("default") && !container_default {
                flags |= REQUIRED;
            }
            if nullable && !has("skip_serializing_if") {
                flags |= NULL_WHEN_ABSENT;
            }
            if is_nested_option(&field.ty) || has("deserialize_with") {
                flags |= KEEP_NULL;
            }
            out.push(Field { name, ty, flags });
        }
        out
    }

    /// Wire keys of types whose `Serialize` is written by hand, so their declared
    /// fields say nothing about what travels. Anything else without a derived
    /// `Serialize` fails the build instead of being modelled from its fields.
    fn hand_written_fields(&mut self, owner: &str, module: &str) -> Option<Vec<Field>> {
        match owner {
            "ProcessCallActivity" => {
                let reference = self.resolve_ident("ProcessCallableReference", &[], module);
                let mapping = || Ty::Map(Box::new(Ty::Str));
                let field = |name: &str, ty: Ty, flags: u8| Field { name: name.to_string(), ty, flags };
                Some(vec![
                    field("called_definition_id", Ty::Str, 0),
                    field("called_version", Ty::Uint(32), 0),
                    field("called_element", self.named(reference), 0),
                    field("local_body", self.named(reference), 0),
                    field("input_mapping", mapping(), REQUIRED),
                    field("output_mapping", mapping(), REQUIRED),
                ])
            }
            _ => None,
        }
    }

    fn define(&mut self, key: TypeKey) -> Def {
        let module = self.universe.raws[key.0].module.clone();
        let owner = self.name_of(key);
        let attrs = match &self.universe.raws[key.0].item {
            Item::Struct(s) => s.attrs.clone(),
            Item::Enum(e) => e.attrs.clone(),
            _ => unreachable!(),
        };
        if !derives_serialize(&attrs) {
            let fields = self
                .hand_written_fields(&owner, &module)
                .unwrap_or_else(|| panic!("{owner}: no derived `Serialize`, so its wire keys are unknown; describe them in `hand_written_fields`"));
            return Def::Struct { fields, deny_unknown: true };
        }
        match self.universe.raws[key.0].item.clone() {
            Item::Struct(s) => {
                let pairs = serde_pairs(&s.attrs);
                for (name, value) in &pairs {
                    assert!(
                        matches!(name.as_str(), "default" | "deny_unknown_fields" | "transparent") && value.is_none() || name == "default",
                        "{owner}: container attribute `{name}` is not modelled by the wire schema"
                    );
                }
                let container_default = pairs.iter().any(|(name, _)| name == "default");
                let deny_unknown = pairs.iter().any(|(name, _)| name == "deny_unknown_fields");
                match &s.fields {
                    Fields::Named(_) => Def::Struct { fields: self.fields_of(&s.fields, &module, &owner, container_default), deny_unknown },
                    Fields::Unnamed(_) => panic!("{owner}: tuple structs with several fields are not modelled by the wire schema"),
                    Fields::Unit => unreachable!("unit structs are rejected when the type is referenced"),
                }
            }
            Item::Enum(e) => {
                let pairs = serde_pairs(&e.attrs);
                for (name, value) in &pairs {
                    let known = match name.as_str() {
                        "rename_all" => value.as_deref() == Some("snake_case"),
                        "tag" | "content" => value.is_some(),
                        "deny_unknown_fields" | "default" => true,
                        _ => false,
                    };
                    assert!(known, "{owner}: container attribute `{name}` is not modelled by the wire schema");
                }
                let rename_snake = serde_value(&e.attrs, "rename_all").is_some();
                let tag = serde_value(&e.attrs, "tag");
                let content = serde_value(&e.attrs, "content");
                assert!(content.is_none() || tag.is_some(), "{owner}: `content` without `tag`");
                let mut variants = Vec::new();
                for variant in &e.variants {
                    for (name, _) in serde_pairs(&variant.attrs) {
                        assert!(
                            matches!(name.as_str(), "rename" | "default"),
                            "{owner}::{}: variant attribute `{name}` is not modelled by the wire schema",
                            variant.ident
                        );
                    }
                    let ident = variant.ident.to_string();
                    let wire = serde_value(&variant.attrs, "rename")
                        .unwrap_or_else(|| if rename_snake { snake_case(&ident) } else { ident.clone() });
                    let shape = match &variant.fields {
                        Fields::Unit => Shape::Unit,
                        Fields::Unnamed(u) if u.unnamed.len() == 1 => Shape::Newtype(self.resolve_ty(&u.unnamed[0].ty, &module)),
                        Fields::Unnamed(_) => panic!("{owner}::{ident}: tuple variants with several fields are not modelled by the wire schema"),
                        Fields::Named(_) => Shape::Struct(self.fields_of(&variant.fields, &module, &format!("{owner}::{ident}"), false)),
                    };
                    variants.push((wire, shape));
                }
                let has_data = e.variants.iter().any(|v| !matches!(v.fields, Fields::Unit));
                let deny_unknown = pairs.iter().any(|(name, _)| name == "deny_unknown_fields");
                Def::Enum { routing: has_data && tag.is_none(), tag, content, deny_unknown, variants }
            }
            _ => unreachable!(),
        }
    }
}

/// `Option<Option<T>>` (a patch field: absent / null / value) has the same wire
/// values as `Option<T>`.
fn opt(inner: Ty) -> Ty {
    match inner {
        Ty::Opt(_) => inner,
        other => Ty::Opt(Box::new(other)),
    }
}

fn is_nested_option(ty: &syn::Type) -> bool {
    let syn::Type::Path(p) = ty else { return false };
    let Some(last) = p.path.segments.last() else { return false };
    let syn::PathArguments::AngleBracketed(args) = &last.arguments else { return false };
    last.ident == "Option"
        && args.args.iter().any(|a| matches!(a, syn::GenericArgument::Type(inner) if is_option(inner)))
}

fn is_option(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Option"))
}

/// `#[serde(with = "serde_bytes")]` turns the byte vector (or an optional one) into a CBOR byte string.
fn bytesify(ty: Ty, owner: &str) -> Ty {
    match ty {
        Ty::Opt(inner) => Ty::Opt(Box::new(bytesify(*inner, owner))),
        Ty::Seq(inner) if *inner == Ty::Uint(8) => Ty::Bytes,
        _ => panic!("{owner}: `serde_bytes` on something other than a byte vector"),
    }
}

// ----- emission --------------------------------------------------------------

fn ty_expr(ty: &Ty) -> String {
    match ty {
        Ty::Str => "Ty::Str".into(),
        Ty::Bool => "Ty::Bool".into(),
        Ty::Unit => "Ty::Unit".into(),
        Ty::Uint(bits) => format!("Ty::Uint({bits})"),
        Ty::Sint(bits) => format!("Ty::Sint({bits})"),
        Ty::Float(bits) => format!("Ty::Float({bits})"),
        Ty::Json => "Ty::Json".into(),
        Ty::Bytes => "Ty::Bytes".into(),
        Ty::Named(id) => format!("Ty::Named({id})"),
        Ty::Opt(inner) => format!("Ty::Opt(&{})", ty_expr(inner)),
        Ty::Seq(inner) => format!("Ty::Seq(&{})", ty_expr(inner)),
        Ty::Map(inner) => format!("Ty::Map(&{})", ty_expr(inner)),
        Ty::Tuple(items) => format!("Ty::Tuple(&[{}])", items.iter().map(ty_expr).collect::<Vec<_>>().join(", ")),
    }
}

fn fields_expr(fields: &[Field]) -> String {
    let items: Vec<String> =
        fields.iter().map(|f| format!("Field {{ name: {:?}, ty: {}, flags: {} }}", f.name, ty_expr(&f.ty), f.flags)).collect();
    format!("&[{}]", items.join(", "))
}

fn shape_expr(shape: &Shape) -> String {
    match shape {
        Shape::Unit => "Shape::Unit".into(),
        Shape::Newtype(ty) => format!("Shape::Newtype({})", ty_expr(ty)),
        Shape::Struct(fields) => format!("Shape::Struct({})", fields_expr(fields)),
    }
}

fn def_expr(def: &Def) -> String {
    match def {
        Def::Struct { fields, deny_unknown } => format!("Def::Struct {{ fields: {}, deny_unknown: {deny_unknown} }}", fields_expr(fields)),
        Def::Enum { tag, content, routing, deny_unknown, variants } => {
            let opt = |v: &Option<String>| v.as_ref().map_or("None".to_string(), |s| format!("Some({s:?})"));
            let items: Vec<String> = variants.iter().map(|(name, shape)| format!("({name:?}, {})", shape_expr(shape))).collect();
            format!(
                "Def::Enum {{ tag: {}, content: {}, routing: {routing}, deny_unknown: {deny_unknown}, variants: &[{}] }}",
                opt(tag),
                opt(content),
                items.join(", ")
            )
        }
    }
}
