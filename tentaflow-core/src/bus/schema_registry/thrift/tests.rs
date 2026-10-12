// =============================================================================
// File: bus/schema_registry/thrift/tests.rs — tests of the Thrift registry kind
// =============================================================================
use super::walker::Wt;
use super::*;
use crate::bus::schema_registry::{fixtures, SchemaType};
use std::time::{Duration, Instant};

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn compile(text: &str) -> CompiledSchema {
    THRIFT_OPS
        .compile(text)
        .unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn refusal(text: &str) -> String {
    match THRIFT_OPS.compile(text) {
        Err(SchemaError::Invalid(m)) => m,
        other => panic!("expected a refusal of {text:?}, got {other:?}"),
    }
}

fn check(compiled: &CompiledSchema, payload: &[u8]) -> Result<(), SchemaError> {
    THRIFT_OPS.validate(compiled, payload)
}

fn violation(compiled: &CompiledSchema, payload: &[u8]) -> String {
    match check(compiled, payload) {
        Err(SchemaError::Violation(m)) => m,
        other => panic!("expected a violation for {payload:?}, got {other:?}"),
    }
}

fn limit(compiled: &CompiledSchema, payload: &[u8]) -> String {
    match check(compiled, payload) {
        Err(SchemaError::LimitExceeded(m)) => m,
        other => panic!("expected a limit for {payload:?}, got {other:?}"),
    }
}

/// Runs `f` on a thread with a stack this small, so a recursion that the caps
/// are supposed to prevent aborts the test process loudly.
fn on_stack<T: Send + 'static>(kib: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(kib * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

fn timed<T>(what: &str, limit: Duration, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    eprintln!("timing: {what}: {took:?}");
    assert!(took < limit, "{what} took {took:?}, limit {limit:?}");
    out
}

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// A value to encode, in either protocol.
#[derive(Clone)]
enum V {
    Bool(bool),
    Byte(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    Double(f64),
    Bin(Vec<u8>),
    Uuid,
    Struct(Vec<(i16, V)>),
    List(Wt, Vec<V>),
    Set(Wt, Vec<V>),
    Map(Wt, Wt, Vec<(V, V)>),
}

fn s(text: &str) -> V {
    V::Bin(text.as_bytes().to_vec())
}

impl V {
    fn wt(&self) -> Wt {
        match self {
            V::Bool(_) => Wt::Bool,
            V::Byte(_) => Wt::Byte,
            V::I16(_) => Wt::I16,
            V::I32(_) => Wt::I32,
            V::I64(_) => Wt::I64,
            V::Double(_) => Wt::Double,
            V::Bin(_) => Wt::Binary,
            V::Uuid => Wt::Uuid,
            V::Struct(_) => Wt::Struct,
            V::List(..) => Wt::List,
            V::Set(..) => Wt::Set,
            V::Map(..) => Wt::Map,
        }
    }
}

fn binary_code(wt: Wt) -> u8 {
    match wt {
        Wt::Bool => 2,
        Wt::Byte => 3,
        Wt::Double => 4,
        Wt::I16 => 6,
        Wt::I32 => 8,
        Wt::I64 => 10,
        Wt::Binary => 11,
        Wt::Struct => 12,
        Wt::Map => 13,
        Wt::Set => 14,
        Wt::List => 15,
        Wt::Uuid => 16,
    }
}

fn compact_code(wt: Wt) -> u8 {
    match wt {
        Wt::Bool => 1,
        Wt::Byte => 3,
        Wt::I16 => 4,
        Wt::I32 => 5,
        Wt::I64 => 6,
        Wt::Double => 7,
        Wt::Binary => 8,
        Wt::List => 9,
        Wt::Set => 10,
        Wt::Map => 11,
        Wt::Struct => 12,
        Wt::Uuid => 13,
    }
}

fn binary_encode(v: &V, out: &mut Vec<u8>) {
    match v {
        V::Bool(b) => out.push(u8::from(*b)),
        V::Byte(n) => out.push(*n as u8),
        V::I16(n) => out.extend(n.to_be_bytes()),
        V::I32(n) => out.extend(n.to_be_bytes()),
        V::I64(n) => out.extend(n.to_be_bytes()),
        V::Double(n) => out.extend(n.to_be_bytes()),
        V::Bin(b) => {
            out.extend((b.len() as i32).to_be_bytes());
            out.extend(b);
        }
        V::Uuid => out.extend([7u8; 16]),
        V::Struct(fields) => {
            for (id, value) in fields {
                out.push(binary_code(value.wt()));
                out.extend(id.to_be_bytes());
                binary_encode(value, out);
            }
            out.push(0);
        }
        V::List(wt, items) | V::Set(wt, items) => {
            out.push(binary_code(*wt));
            out.extend((items.len() as i32).to_be_bytes());
            items.iter().for_each(|i| binary_encode(i, out));
        }
        V::Map(k, val, items) => {
            out.push(binary_code(*k));
            out.push(binary_code(*val));
            out.extend((items.len() as i32).to_be_bytes());
            for (key, value) in items {
                binary_encode(key, out);
                binary_encode(value, out);
            }
        }
    }
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn zigzag(n: i64) -> u64 {
    ((n << 1) ^ (n >> 63)) as u64
}

fn compact_encode(v: &V, out: &mut Vec<u8>) {
    match v {
        V::Bool(b) => out.push(if *b { 1 } else { 2 }),
        V::Byte(n) => out.push(*n as u8),
        V::I16(n) => varint(zigzag(i64::from(*n)), out),
        V::I32(n) => varint(zigzag(i64::from(*n)), out),
        V::I64(n) => varint(zigzag(*n), out),
        V::Double(n) => out.extend(n.to_le_bytes()),
        V::Bin(b) => {
            varint(b.len() as u64, out);
            out.extend(b);
        }
        V::Uuid => out.extend([7u8; 16]),
        V::Struct(fields) => {
            let mut last = 0i16;
            for (id, value) in fields {
                let code = match value {
                    V::Bool(true) => 1,
                    V::Bool(false) => 2,
                    other => compact_code(other.wt()),
                };
                let delta = i32::from(*id) - i32::from(last);
                if (1..=15).contains(&delta) {
                    out.push((delta as u8) << 4 | code);
                } else {
                    out.push(code);
                    varint(zigzag(i64::from(*id)), out);
                }
                if !matches!(value, V::Bool(_)) {
                    compact_encode(value, out);
                }
                last = *id;
            }
            out.push(0);
        }
        V::List(wt, items) | V::Set(wt, items) => {
            if items.len() < 15 {
                out.push((items.len() as u8) << 4 | compact_code(*wt));
            } else {
                out.push(0xf0 | compact_code(*wt));
                varint(items.len() as u64, out);
            }
            items.iter().for_each(|i| compact_encode(i, out));
        }
        V::Map(k, val, items) => {
            if items.is_empty() {
                out.push(0);
                return;
            }
            varint(items.len() as u64, out);
            out.push(compact_code(*k) << 4 | compact_code(*val));
            for (key, value) in items {
                compact_encode(key, out);
                compact_encode(value, out);
            }
        }
    }
}

fn bin(v: &V) -> Vec<u8> {
    let mut out = Vec::new();
    binary_encode(v, &mut out);
    out
}

fn cmp(v: &V) -> Vec<u8> {
    let mut out = Vec::new();
    compact_encode(v, &mut out);
    out
}

fn st(fields: Vec<(i16, V)>) -> V {
    V::Struct(fields)
}

const COMPACT: &str = "// tentabus:protocol compact\n";

/// A schema pair: the same text under both protocols.
type Encoder = fn(&V) -> Vec<u8>;

fn both(body: &str) -> [(CompiledSchema, Encoder); 2] {
    [
        (compile(body), bin as fn(&V) -> Vec<u8>),
        (
            compile(&format!("{COMPACT}{body}")),
            cmp as fn(&V) -> Vec<u8>,
        ),
    ]
}

const ORDER: &str = "
enum Status { NEW, PAID = 5, SHIPPED }
struct Line { 1: required string sku, 2: i32 qty }
union Payment { 1: string card, 2: i64 voucher }
// tentabus:struct Order
struct Order {
  1: required i64 id,
  2: optional string customer,
  3: Status status,
  4: list<Line> lines,
  5: map<string, i32> tags,
  6: set<i16> flags,
  7: bool urgent,
  8: double total,
  9: binary note,
  10: uuid ref,
  11: Payment payment,
  12: byte grade,
  13: list<bool> checks,
}";

// -----------------------------------------------------------------------------
// Golden fixtures (written by the Apache Thrift Python library)
// -----------------------------------------------------------------------------

fn golden(format: &str) {
    let text = fixtures::read(format, "order.thrift");
    let compiled = compile(&text);
    for case in fixtures::cases(format, "datum") {
        let got = check(&compiled, &case.payload);
        match &case.expect {
            None => assert_eq!(got, Ok(()), "{}", case.name),
            Some(expect) => assert!(
                matches!(&got, Err(SchemaError::Violation(m)) if m.contains(expect.as_str())),
                "{}: expected a violation containing {expect:?}, got {got:?}",
                case.name
            ),
        }
    }
}

#[test]
fn golden_binary_documents_validate_and_refusals_carry_their_phrase() {
    golden("thrift");
}

#[test]
fn golden_compact_documents_validate_and_refusals_carry_their_phrase() {
    golden("thrift/compact");
}

// -----------------------------------------------------------------------------
// Schema text
// -----------------------------------------------------------------------------

#[test]
fn every_documented_construct_is_accepted() {
    compile(
        r#"
        # shell comment
        /* block
           comment */
        // line comment
        namespace java com.example.shop (a = "b")
        namespace * shop;
        const i32 MAX = 0x10
        const list<string> NAMES = ["a", 'b'; "c"]
        const map<string, double> RATES = {"x": 1.5, "y": -2e3, "z": +.5}
        typedef list<Line> Lines
        typedef i64 Ts (python.immutable)
        enum Color { RED, GREEN = 5 (x = "y"), BLUE, NEG = -3; }
        struct Line {
          1: required string sku = "none" (a = "b"),
          2: optional i32 qty = 1;
          3: Lines more
          4: map<string, list<set<i8>>> nest
          5: Color color = Color.RED
        } (deprecated)
        union Pick { 1: i32 a, 2: string b }
        exception Boom { 1: string why, 2: Line line }
        struct Late { 7: Early e }
        struct Early { 1: optional Late l, 2: uuid u, 3: binary raw, 4: bool ok, 5: i16 s, 6: double d }
        "#,
    );
}

#[test]
fn schema_text_refusals_are_stable() {
    let long_name = "a".repeat(129);
    let many_parts = ["a"; 9].join(".");
    let long_string = "x".repeat(257);
    for (text, phrase) in [
        ("   ".to_string(), "schema text is empty"),
        (
            "// only a comment\n".to_string(),
            "the schema declares no struct, union or exception",
        ),
        (
            "enum E { A }".to_string(),
            "the schema declares no struct, union or exception",
        ),
        (
            r#"include "x.thrift" struct S {}"#.to_string(),
            "include is not supported",
        ),
        (
            r#"cpp_include "x.h" struct S {}"#.to_string(),
            "cpp_include is not supported",
        ),
        (
            "service Svc { void ping() }".to_string(),
            "service is not supported",
        ),
        ("senum E { \"a\" }".to_string(), "senum is not supported"),
        (
            "struct S { 1: slist x }".to_string(),
            "slist is not supported",
        ),
        (
            "struct S { 1: i32 a, 2: i32 b } stream X {}".to_string(),
            "'stream' is not part of the supported Thrift IDL subset",
        ),
        (
            "struct S { string name }".to_string(),
            "field 'name' of struct 'S' has no explicit id",
        ),
        (
            "struct S { 0: string name }".to_string(),
            "has an id outside 1 to 32767",
        ),
        (
            "struct S { -1: string name }".to_string(),
            "has an id outside 1 to 32767",
        ),
        (
            "struct S { 32768: string name }".to_string(),
            "has an id outside 1 to 32767",
        ),
        (
            "union U { 1: required i32 a }".to_string(),
            "a member of union 'U' cannot be required",
        ),
        (
            "struct S { 1: shared.Thing t }".to_string(),
            "points into an included file",
        ),
        (
            "struct S { 1: i32 list }".to_string(),
            "'list' is a reserved word",
        ),
        (
            "struct required { 1: i32 a }".to_string(),
            "'required' is a reserved word",
        ),
        (
            "struct a.b { 1: i32 a }".to_string(),
            "cannot contain a dot",
        ),
        (
            "struct S {} struct S {}".to_string(),
            "type 'S' is defined more than once",
        ),
        (
            "struct S {} enum S { A }".to_string(),
            "type 'S' is defined more than once",
        ),
        (
            "struct S { 1: i32 a, 1: i32 b }".to_string(),
            "has two fields with the id 1",
        ),
        (
            "struct S { 1: i32 a, 2: i32 a }".to_string(),
            "has two fields named 'a'",
        ),
        (
            "enum E { A, A } struct S {}".to_string(),
            "has two values named 'A'",
        ),
        (
            "enum E { A = 2147483648 } struct S {}".to_string(),
            "does not fit a 32-bit integer",
        ),
        (
            "enum E { A = 2147483647, B } struct S {}".to_string(),
            "does not fit a 32-bit integer",
        ),
        (
            "struct S { 1: Nope n }".to_string(),
            "type 'Nope' is not defined in the schema",
        ),
        (
            "typedef A B typedef B A struct S { 1: A a }".to_string(),
            "refers to itself",
        ),
        (
            "typedef list<A> A struct S { 1: A a }".to_string(),
            "typedef 'A' refers to itself",
        ),
        (
            "typedef A A struct S {}".to_string(),
            "typedef 'A' refers to itself",
        ),
        ("struct S { 1: i32 a xsd_all }".to_string(), "expected"),
        ("struct S xsd_all { 1: i32 a }".to_string(), "expected '{'"),
        (
            "struct S { 1: list<i32> cpp_type \"x\" a }".to_string(),
            "'cpp_type' is a reserved word",
        ),
        ("struct S { 1: i32 a = }".to_string(), "expected a value"),
        ("struct S { 1: void a }".to_string(), "expected a type"),
        (
            "struct S { 1: i32 a } ;".to_string(),
            "expected a declaration",
        ),
        ("struct S { 1: i32 a".to_string(), "the end of the schema"),
        (
            "/* never closed struct S {}".to_string(),
            "a comment is never closed",
        ),
        (
            "struct S { 1: i32 a = \"abc }".to_string(),
            "a string is never closed on its line",
        ),
        (
            "struct S { 1: i32 a = \"a\\qb\" }".to_string(),
            "escape sequence",
        ),
        (
            "struct S { 1: i32 a = \"a\u{202e}b\" }".to_string(),
            "character a Thrift file cannot hold inside a string",
        ),
        (
            "struct S { 1: i32 a = \"a\u{1}b\" }".to_string(),
            "character a Thrift file cannot hold inside a string",
        ),
        (
            format!("struct S {{ 1: i32 a = \"{long_string}\" }}"),
            "a string is longer than 256 bytes",
        ),
        (
            "struct S { 1: i32 a } é".to_string(),
            "character a Thrift file cannot",
        ),
        (
            "struct S { 1: i32 a } @".to_string(),
            "character a Thrift file cannot",
        ),
        (
            "struct S { 1: i32 a = 99999999999999999999 }".to_string(),
            "integer literal is out of range",
        ),
        (
            "struct S { 1: i32 a = 12ab }".to_string(),
            "a number is malformed",
        ),
        (
            format!("struct S {{ 1: i32 {long_name} }}"),
            "a name is longer than 128 characters",
        ),
        (
            format!("struct S {{ 1: {many_parts} a }}"),
            "a name has more than 8 parts",
        ),
        (
            "struct S { 1: a..b x }".to_string(),
            "a name has an empty part",
        ),
        (
            "// tentabus:struct Nope\nstruct S {}".to_string(),
            "the designated struct 'Nope' is not a top-level struct, union or exception",
        ),
        (
            "// tentabus:struct E\nenum E { A } struct S {}".to_string(),
            "the designated struct 'E' is not a top-level struct",
        ),
        (
            "// tentabus:struct\nstruct S {}".to_string(),
            "must be followed by the name of a struct",
        ),
        (
            "// tentabus:struct 9x\nstruct S {}".to_string(),
            "must be followed by the name of a struct",
        ),
        (
            "// tentabus:struct S\n# tentabus:struct S\nstruct S {}".to_string(),
            "appears more than once",
        ),
        (
            "// tentabus:protocol json\nstruct S {}".to_string(),
            "must be followed by 'binary' or 'compact'",
        ),
        (
            "// tentabus:protocol\nstruct S {}".to_string(),
            "must be followed by 'binary' or 'compact'",
        ),
        (
            "// tentabus:protocol compact\n// tentabus:protocol binary\nstruct S {}".to_string(),
            "appears more than once",
        ),
        (
            "struct S { 1: required S next }".to_string(),
            "the schema can never produce a value",
        ),
        (
            "enum E {} struct S { 1: required E e }".to_string(),
            "the schema can never produce a value",
        ),
        (
            "struct A { 1: required B b } struct B { 1: required A a }".to_string(),
            "the schema can never produce a value",
        ),
    ] {
        let got = refusal(&text);
        assert!(
            got.contains(phrase),
            "{text:?}: {got:?} should contain {phrase:?}"
        );
    }
}

#[test]
fn a_refusal_names_the_line_and_never_carries_a_literal() {
    let got = refusal("struct S {\n  1: i32 a,\n  2: i32 b = \"SECRET\\q\"\n}");
    assert!(got.contains("(line 3)"), "{got}");
    assert!(!got.contains("SECRET"), "{got}");
}

#[test]
fn the_root_is_the_first_struct_or_the_one_a_directive_names() {
    let body = "enum E { A } exception First { 1: i64 f } struct Second { 1: string s }";
    let first = compile(body);
    assert_eq!(check(&first, &bin(&st(vec![(1, V::I64(1))]))), Ok(()));
    assert_eq!(
        violation(&first, &bin(&st(vec![(1, s("x"))]))),
        "/f: wire type does not match the type declared for the field"
    );
    for directive in [
        "// tentabus:struct Second\n",
        "#   tentabus:struct   Second  \n",
    ] {
        let named = compile(&format!("{directive}{body}"));
        assert_eq!(check(&named, &bin(&st(vec![(1, s("x"))]))), Ok(()));
    }
    // A block comment or a differently worded comment is no directive.
    let ignored = compile(&format!(
        "/* tentabus:struct Second */\n// tentabus:structs Second\n{body}"
    ));
    assert_eq!(check(&ignored, &bin(&st(vec![(1, V::I64(1))]))), Ok(()));
    compile("// tentabus:protocol binary\nstruct S {}");
}

#[test]
fn a_typedef_is_a_name_and_chains_stay_linear() {
    let mut text = String::from("typedef i64 T0\n");
    for i in 1..=31 {
        text.push_str(&format!("typedef T{} T{i}\n", i - 1));
    }
    text.push_str("struct S { 1: T31 a }");
    let ok = compile(&text);
    assert_eq!(check(&ok, &bin(&st(vec![(1, V::I64(1))]))), Ok(()));
    let mut over = String::from("typedef i64 T0\n");
    for i in 1..=32 {
        over.push_str(&format!("typedef T{} T{i}\n", i - 1));
    }
    over.push_str("struct S { 1: T32 a }");
    assert!(refusal(&over).contains("a chain of typedefs is longer than 32"));

    // A map of the previous name at every step doubles a tree; here it stays an arena.
    let mut bomb = String::from("typedef i32 M0\n");
    for i in 1..=200 {
        bomb.push_str(&format!("typedef map<M{}, M{}> M{i}\n", i - 1, i - 1));
    }
    bomb.push_str("struct S { 1: M200 m }");
    let compiled = timed("typedef doubling chain", Duration::from_secs(2), || {
        on_stack(512, {
            let bomb = bomb.clone();
            move || THRIFT_OPS.compile(&bomb)
        })
    });
    // Each typedef is one name in the arena, so the bomb compiles but no payload can be nested that far.
    let compiled = compiled.unwrap();
    let compat = timed("typedef doubling compare", Duration::from_secs(5), || {
        on_stack(512, {
            let bomb = bomb.clone();
            move || THRIFT_OPS.check_compatibility(&bomb, &bomb, Compatibility::Full)
        })
    });
    assert!(matches!(
        compat,
        Ok(()) | Err(SchemaError::LimitExceeded(_))
    ));
    let derived = THRIFT_OPS
        .derive_subschema(&bomb, &BTreeSet::from(["m".to_string()]))
        .unwrap();
    assert!(derived.contains("typedef map<M199, M199> M200"));
    assert_eq!(check(&compiled, &bin(&st(vec![]))), Ok(()));
}

#[test]
fn declaration_caps_hold_exactly_at_their_limit() {
    let types = |n: usize| {
        (0..n)
            .map(|i| format!("struct T{i} {{}}\n"))
            .collect::<String>()
    };
    compile(&types(256));
    assert!(refusal(&types(257)).contains("more than 256 types"));

    let fields = |n: usize| {
        let body: String = (0..n).map(|i| format!("{}: i32 f{i}, ", i + 1)).collect();
        format!("struct S {{ {body} }}")
    };
    compile(&fields(1024));
    assert!(refusal(&fields(1025)).contains("has more than 1024 fields"));

    let spread = |structs: usize| {
        (0..structs)
            .map(|k| {
                let body: String = (0..1024)
                    .map(|i| format!("{}: i32 f{i}, ", i + 1))
                    .collect();
                format!("struct S{k} {{ {body} }}\n")
            })
            .collect::<String>()
    };
    compile(&spread(4));
    assert!(
        refusal(&format!("{}struct Extra {{ 1: i32 a }}", spread(4)))
            .contains("more than 4096 fields")
    );

    let values = |n: usize| {
        let body: String = (0..n).map(|i| format!("V{i}, ")).collect();
        format!("enum E {{ {body} }} struct S {{}}")
    };
    compile(&values(1024));
    assert!(refusal(&values(1025)).contains("has more than 1024 values"));
    let enums = |extra: usize| {
        let mut text = String::from("struct S {}\n");
        for k in 0..4 {
            let body: String = (0..1024).map(|i| format!("V{i}, ")).collect();
            text.push_str(&format!("enum E{k} {{ {body} }}\n"));
        }
        if extra > 0 {
            text.push_str("enum Extra { One }");
        }
        text
    };
    compile(&enums(0));
    assert!(refusal(&enums(1)).contains("more than 4096 enum values"));

    let required = |n: usize| {
        let body: String = (0..n)
            .map(|i| format!("{}: required i32 f{i}, ", i + 1))
            .collect();
        format!("struct S {{ {body} }}")
    };
    compile(&required(64));
    assert!(refusal(&required(65)).contains("more than 64 required fields"));

    let name = |n: usize| format!("struct S {{ 1: i32 {} }}", "a".repeat(n));
    compile(&name(128));
    assert!(refusal(&name(129)).contains("longer than 128"));
    let parts = |n: usize| format!("namespace py {}\nstruct S {{}}", vec!["a"; n].join("."));
    compile(&parts(8));
    assert!(refusal(&parts(9)).contains("more than 8 parts"));

    let string = |n: usize| format!("struct S {{ 1: string a = \"{}\" }}", "é".repeat(n));
    compile(&string(128));
    assert!(refusal(&string(129)).contains("longer than 256 bytes"));

    let nest = |n: usize| {
        format!(
            "struct S {{ 1: {}i32{} a }}",
            "list<".repeat(n),
            ">".repeat(n)
        )
    };
    compile(&nest(32));
    assert!(refusal(&nest(33)).contains("nested deeper than 32 levels"));
    let value = |n: usize| {
        format!(
            "const list<i32> C = {}1{}\nstruct S {{}}",
            "[".repeat(n),
            "]".repeat(n)
        )
    };
    compile(&value(16));
    assert!(refusal(&value(17)).contains("nested deeper than 16 levels"));

    let consts = |n: usize| {
        let body: String = (0..n).map(|i| format!("const i32 C{i} = 1\n")).collect();
        format!("{body}struct S {{}}")
    };
    compile(&consts(256));
    assert!(refusal(&consts(257)).contains("more than 256 constants"));
    let spaces = |n: usize| {
        let body: String = (0..n).map(|_| "namespace py a\n").collect();
        format!("{body}struct S {{}}")
    };
    compile(&spaces(64));
    assert!(refusal(&spaces(65)).contains("more than 64 namespaces"));
    let notes = |n: usize| {
        let body: String = (0..n).map(|i| format!("k{i}, ")).collect();
        format!("struct S {{}} (a, {body} z)")
    };
    assert!(refusal(&notes(15)).contains("annotation has more than 16 entries"));
    compile(&notes(14));
    let exprs = |n: usize| {
        let mut text = String::new();
        for k in 0..n / 1024 {
            let body: String = (0..1024)
                .map(|i| format!("{}: i32 f{i}, ", i + 1))
                .collect();
            text.push_str(&format!("struct S{k} {{ {body} }}\n"));
        }
        text
    };
    compile(&exprs(4096));
    let lists = format!(
        "struct S {{ {} }}",
        (0..7)
            .map(|i| format!(
                "{}: {}i32{} a{i}, ",
                i + 1,
                "list<".repeat(32),
                ">".repeat(32)
            ))
            .collect::<String>()
            + &(0..40)
                .map(|i| format!("{}: list<i32> b{i}, ", i + 20))
                .collect::<String>()
    );
    compile(&lists);
    let huge: String = (0..520)
        .map(|i| {
            format!(
                "const {}i32{} C{i} = 1\n",
                "list<".repeat(32),
                ">".repeat(32)
            )
        })
        .collect();
    assert!(refusal(&format!("{huge}struct S {{}}")).contains("more than 256 constants"));
}

#[test]
fn the_largest_accepted_schema_compiles_and_derives_in_bounded_time() {
    let mut text = String::new();
    for k in 0..4 {
        let body: String = (0..1024)
            .map(|i| format!("{}: list<map<string, i8>> field_{k}_{i}, ", i + 1))
            .collect();
        text.push_str(&format!("struct S{k} {{ {body} }}\n"));
    }
    let big = (0..4)
        .map(|k| {
            format!(
                "enum E{k} {{ {} }}\n",
                (0..1024).map(|i| format!("V{k}_{i}, ")).collect::<String>()
            )
        })
        .collect::<String>();
    text.push_str(&big);
    assert!(text.len() < MAX_SCHEMA_TEXT_BYTES, "{}", text.len());
    timed("largest schema", Duration::from_secs(5), || {
        assert!(THRIFT_OPS.compile(&text).is_ok());
        let all: BTreeSet<String> = (0..1024).map(|i| format!("field_0_{i}")).collect();
        let derived = THRIFT_OPS.derive_subschema(&text, &all).unwrap();
        assert!(derived.len() < MAX_DERIVED_TEXT_BYTES);
    });
}

#[test]
fn a_projection_of_a_schema_full_of_long_names_still_fits_its_allowance() {
    // 128-character field names; every enum value and field id is written out in canonical text.
    let mut text = String::from("struct S {");
    let mut n = 0;
    while text.len() < MAX_SCHEMA_TEXT_BYTES - 200 && n < 1024 {
        n += 1;
        text.push_str(&format!("{n}:i8 {:a>128},", n));
    }
    text.push('}');
    let all: BTreeSet<String> = (1..=n).map(|i| format!("{i:a>128}")).collect();
    let derived = THRIFT_OPS.derive_subschema(&text, &all).unwrap();
    assert!(derived.len() < MAX_DERIVED_TEXT_BYTES);
}

#[test]
fn pathological_schema_texts_are_refused_or_accepted_but_never_panic() {
    let deep_lists = format!("struct S {{ 1: {} }}", "list<".repeat(60_000));
    let deep_values = format!("const i32 C = {}\nstruct S {{}}", "[".repeat(60_000));
    let deep_maps = format!("const i32 C = {}\nstruct S {{}}", "{1:".repeat(60_000));
    let many_tokens = format!("struct S {{ {} }}", "1 ".repeat(120_000));
    let many_semis = format!("struct S {{ {} }}", ";".repeat(200_000));
    let long_comment = format!("// {}\nstruct S {{}}", "x".repeat(200_000));
    let long_word = format!("struct {} {{}}", "a".repeat(200_000));
    let many_parens = format!("struct S (a=\"b\") {}", "(".repeat(100_000));
    for text in [
        deep_lists,
        deep_values,
        deep_maps,
        many_tokens,
        many_semis,
        long_comment,
        long_word,
        many_parens,
        "\u{feff}struct S {}".to_string(),
        "struct".to_string(),
        "{".to_string(),
        "}".to_string(),
        "struct S { 1: map<i32> a }".to_string(),
        "struct S { 1: map<i32,> a }".to_string(),
        "struct S { 1: list<> a }".to_string(),
        "struct S { 1: i32 a = [1, 2 }".to_string(),
        "struct S { 1: i32 a = {1 2} }".to_string(),
        "struct S { 1: i32 a = - }".to_string(),
        "struct S { 1: i32 a = 0x }".to_string(),
        "struct S { 1: i32 a = 1e }".to_string(),
        "struct S { 1: i32 a = 1.e5 }".to_string(),
        "struct S { 1: i32 a = \"\\".to_string(),
        "struct S { 1: i32 a (k = ) }".to_string(),
        "struct S { 1: i32 a (k = 1) }".to_string(),
        "namespace".to_string(),
        "namespace * ".to_string(),
        "typedef".to_string(),
        "const".to_string(),
        "const i32 x".to_string(),
        "enum".to_string(),
        "enum E {".to_string(),
        "enum E { A = }".to_string(),
        "enum E { A = x }".to_string(),
        "struct S { 1: }".to_string(),
        "struct S { :1 i32 a }".to_string(),
        "struct S { 1 i32 a }".to_string(),
        "struct S { 1: required }".to_string(),
        "// tentabus:struct \u{0}".to_string(),
        "struct S { 1: i32 a = 9223372036854775807 }".to_string(),
        "struct S { 1: i32 a = -9223372036854775808 }".to_string(),
        "struct S { 1: i32 a = 0xffffffffffffffff }".to_string(),
    ] {
        let _ = timed("pathological text", Duration::from_secs(2), || {
            on_stack(512, move || THRIFT_OPS.compile(&text).map(|_| ()))
        });
    }
}

#[test]
fn the_deepest_accepted_nesting_fits_a_small_stack() {
    let nest = format!(
        "struct S {{ 1: {}i32{} a }}",
        "list<".repeat(32),
        ">".repeat(32)
    );
    on_stack(256, move || {
        let c = THRIFT_OPS.compile(&nest).unwrap();
        let mut v = V::I32(1);
        for _ in 0..32 {
            v = V::List(v.wt(), vec![v]);
        }
        assert_eq!(THRIFT_OPS.validate(&c, &bin(&st(vec![(1, v)]))), Ok(()));
    });
}

// -----------------------------------------------------------------------------
// Fuzz
// -----------------------------------------------------------------------------

fn fuzz_seeds() -> Vec<String> {
    vec![
        fixtures::read("thrift", "order.thrift"),
        fixtures::read("thrift/compact", "order.thrift"),
        ORDER.to_string(),
        "namespace py x\ntypedef list<map<string, Color>> Grid\nenum Color { RED, GREEN = 4, }\n\
         const map<string, list<i32>> K = {\"a\": [1, 2]; \"b\": []}\n\
         union U { 1: Grid g, 2: set<U> inner }\nexception X { 1: required string why (a = \"b\") }\n\
         struct R { 1: optional R next, 2: U u, 3: X x = 5 }"
            .to_string(),
        "// tentabus:struct B\nstruct A { 1: i32 a } struct B { 1: A a, 2: list<A> as }".to_string(),
    ]
}

fn mutate_schema(text: &mut String, rng: &mut Xorshift) {
    const VOCABULARY: &[&str] = &[
        "struct ",
        "union ",
        "exception ",
        "enum ",
        "typedef ",
        "const ",
        "namespace ",
        "include ",
        "service ",
        "list<",
        "set<",
        "map<",
        ">",
        "<",
        "{",
        "}",
        "[",
        "]",
        "(",
        ")",
        ";",
        ",",
        "=",
        ":",
        "1",
        "-1",
        "99999999999",
        "required ",
        "optional ",
        "i8 ",
        "string ",
        "uuid ",
        "\"",
        "'",
        "/*",
        "*/",
        "//",
        "#",
        "\n",
        ".",
        "a.b",
        "0x",
        "1e5",
        "\\",
        "tentabus:struct A\n",
        "// tentabus:protocol compact\n",
        "// tentabus:struct ",
    ];
    let boundary_before = |text: &String, at: usize| {
        (0..=at.min(text.len()))
            .rev()
            .find(|i| text.is_char_boundary(*i))
            .unwrap_or(0)
    };
    match rng.below(7) {
        0 | 1 => {
            let at = boundary_before(text, rng.below(text.len() + 1));
            text.insert_str(at, VOCABULARY[rng.below(VOCABULARY.len())]);
        }
        2 => {
            let at = boundary_before(text, rng.below(text.len().max(1)));
            let end = (at + 1 + rng.below(8)).min(text.len());
            if text.is_char_boundary(end) {
                text.replace_range(at..end, "");
            }
        }
        3 => {
            let lines: Vec<String> = text.lines().map(str::to_string).collect();
            if lines.len() > 2 {
                let mut v = lines;
                let (i, j) = (rng.below(v.len()), rng.below(v.len()));
                v.swap(i, j);
                *text = v.join("\n");
            }
        }
        4 => {
            let digits: Vec<usize> = text
                .bytes()
                .enumerate()
                .filter(|(_, b)| b.is_ascii_digit())
                .map(|(i, _)| i)
                .collect();
            if !digits.is_empty() {
                let at = digits[rng.below(digits.len())];
                let number = ["0", "1", "2", "9", "32767", "32768", "99999999999"][rng.below(7)];
                text.replace_range(at..at + 1, number);
            }
        }
        5 => {
            // Make two names the same.
            let bytes = text.as_bytes();
            let (mut idents, mut i) = (Vec::new(), 0);
            while i < bytes.len() {
                if bytes[i].is_ascii_alphabetic() {
                    let start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                    idents.push((start, i));
                } else {
                    i += 1;
                }
            }
            if idents.len() > 2 {
                let (from, to) = (
                    idents[rng.below(idents.len())],
                    idents[rng.below(idents.len())],
                );
                let word = text[to.0..to.1].to_string();
                text.replace_range(from.0..from.1, &word);
            }
        }
        _ => {
            let at = boundary_before(text, rng.below(text.len() + 1));
            let mut end = (at + 1 + rng.below(60)).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            let chunk = text[at..end].to_string();
            let to = boundary_before(text, rng.below(text.len() + 1));
            text.insert_str(to, &chunk);
        }
    }
}

fn fuzz_schemas(iterations: usize, seed: u64) {
    let seeds = fuzz_seeds();
    let mut rng = Xorshift(seed);
    let started = Instant::now();
    let mut accepted = 0;
    for _ in 0..iterations {
        let mut text = seeds[rng.below(seeds.len())].clone();
        for _ in 0..1 + rng.below(4) {
            mutate_schema(&mut text, &mut rng);
        }
        let one = Instant::now();
        let shown = text.clone();
        let verdict = on_stack(512, move || {
            let compiled = THRIFT_OPS.compile(&text)?;
            // Whatever compiles must compare with itself, project and answer a payload.
            THRIFT_OPS.check_compatibility(&text, &text, Compatibility::Full)?;
            let all: BTreeSet<String> = (1..64).map(|i| format!("f{i}")).collect();
            THRIFT_OPS.derive_subschema(&text, &all)?;
            let _ = THRIFT_OPS.validate(&compiled, &[0]);
            let _ = THRIFT_OPS.validate(&compiled, &[12, 0, 1, 0, 0]);
            Ok::<(), SchemaError>(())
        });
        assert!(
            one.elapsed() < Duration::from_secs(2),
            "one mutation took {:?}: {shown}",
            one.elapsed()
        );
        match verdict {
            Ok(()) => accepted += 1,
            Err(SchemaError::Invalid(_) | SchemaError::LimitExceeded(_)) => {}
            Err(other) => panic!("{other:?} on: {shown}"),
        }
    }
    eprintln!(
        "timing: {iterations} mutated schemas: {:?}, {accepted} accepted",
        started.elapsed()
    );
    assert!(
        accepted > iterations / 100 && accepted < iterations,
        "{accepted}"
    );
}

#[test]
fn mutated_schema_texts_never_panic_and_stay_fast() {
    fuzz_schemas(2_000, 0x9e37_79b9_7f4a_7c15);
}

/// `cargo test -p tentaflow-core --lib -- --ignored thrift::tests::long_fuzz`
#[test]
#[ignore = "long campaign: several minutes"]
fn long_fuzz_of_schema_texts() {
    let iterations: usize = std::env::var("TF_THRIFT_FUZZ_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300_000);
    fuzz_schemas(iterations, 0x2545_f491_4f6c_dd1d);
}

#[test]
fn a_comment_between_any_two_tokens_changes_no_verdict() {
    let verdict = |text: &str| match THRIFT_OPS.compile(text) {
        Ok(_) => "ok".to_string(),
        Err(e) => e.to_string(),
    };
    let mut seeds = fuzz_seeds();
    seeds.push("// tentabus:struct B\nstruct A { 1: i32 a } struct B { 1: A a, 2: list<A> as, 3: Missing m }".to_string());
    seeds.push("struct A { 1: i32 a } struct A { 1: i32 b }".to_string());
    seeds.push("include \"x\" struct A {}".to_string());
    let mut tried = 0;
    for seed in seeds {
        let base = verdict(&seed);
        for (at, c) in seed.char_indices() {
            if !(c.is_ascii_whitespace() || "{}()[]<>,;=:".contains(c)) {
                continue;
            }
            // Inside a line comment the text is the comment (or a directive).
            let line = &seed[seed[..at].rfind('\n').map_or(0, |n| n + 1)..at];
            if line.contains("//") || line.contains('#') {
                continue;
            }
            for comment in ["/**/", "/* c */"] {
                let mut text = seed.clone();
                text.insert_str(at, comment);
                assert_eq!(verdict(&text), base, "{comment} at {at} in {seed}");
                tried += 1;
            }
        }
    }
    assert!(tried > 1000, "{tried}");
}

fn fuzz_payloads(iterations: usize, seed: u64) {
    let mut rng = Xorshift(seed);
    let started = Instant::now();
    for (format, protocol) in [("thrift", "binary"), ("thrift/compact", "compact")] {
        let compiled = compile(&fixtures::read(format, "order.thrift"));
        let seeds: Vec<Vec<u8>> = fixtures::cases(format, "datum")
            .into_iter()
            .map(|c| c.payload)
            .collect();
        for _ in 0..iterations {
            let mut payload = seeds[rng.below(seeds.len())].clone();
            for _ in 0..1 + rng.below(4) {
                match rng.below(5) {
                    0 if !payload.is_empty() => {
                        let at = rng.below(payload.len());
                        payload[at] = rng.next() as u8;
                    }
                    1 if !payload.is_empty() => {
                        payload.truncate(rng.below(payload.len()));
                    }
                    2 => {
                        let at = rng.below(payload.len() + 1);
                        payload.insert(at, rng.next() as u8);
                    }
                    3 if !payload.is_empty() => {
                        let at = rng.below(payload.len());
                        payload[at] = [0x00, 0x7f, 0x80, 0xff][rng.below(4)];
                    }
                    _ => payload.extend((0..rng.below(6)).map(|_| rng.next() as u8)),
                }
            }
            let one = Instant::now();
            let verdict = check(&compiled, &payload);
            assert!(
                one.elapsed() < Duration::from_millis(500),
                "{protocol}: {payload:?} took {:?}",
                one.elapsed()
            );
            if let Err(SchemaError::Violation(m)) = &verdict {
                assert!(m.len() < 400, "{m}");
            }
        }
    }
    eprintln!(
        "timing: {iterations}x2 mutated payloads: {:?}",
        started.elapsed()
    );
}

#[test]
fn mutated_payloads_never_panic_and_stay_fast() {
    fuzz_payloads(20_000, 0x1234_5678_9abc_def1);
}

#[test]
#[ignore = "long campaign: a minute or more"]
fn long_fuzz_of_payloads() {
    fuzz_payloads(2_000_000, 0x0fed_cba9_8765_4321);
}

// -----------------------------------------------------------------------------
// Validation, both protocols
// -----------------------------------------------------------------------------

fn full_order() -> V {
    st(vec![
        (1, V::I64(-5)),
        (2, s("Ala")),
        (3, V::I32(5)),
        (
            4,
            V::List(
                Wt::Struct,
                vec![
                    st(vec![(1, s("ab")), (2, V::I32(3))]),
                    st(vec![(1, s("cd"))]),
                ],
            ),
        ),
        (
            5,
            V::Map(
                Wt::Binary,
                Wt::I32,
                vec![(s("x"), V::I32(1)), (s("y"), V::I32(-2))],
            ),
        ),
        (6, V::Set(Wt::I16, vec![V::I16(1), V::I16(-2), V::I16(300)])),
        (7, V::Bool(true)),
        (8, V::Double(12.5)),
        (9, V::Bin(vec![0, 255, 16])),
        (10, V::Uuid),
        (11, st(vec![(1, s("4111"))])),
        (12, V::Byte(-3)),
        (13, V::List(Wt::Bool, vec![V::Bool(true), V::Bool(false)])),
    ])
}

#[test]
fn a_full_document_validates_in_both_protocols() {
    for (c, enc) in both(ORDER) {
        assert_eq!(check(&c, &enc(&full_order())), Ok(()));
        assert_eq!(check(&c, &enc(&st(vec![(1, V::I64(1))]))), Ok(()));
    }
}

#[test]
fn every_base_type_is_accepted_with_its_wire_type_and_refused_with_another() {
    let cases: [(&str, V, V); 9] = [
        ("bool", V::Bool(true), V::Byte(1)),
        ("byte", V::Byte(1), V::I16(1)),
        ("i16", V::I16(1), V::I32(1)),
        ("i32", V::I32(1), V::I64(1)),
        ("i64", V::I64(1), V::I32(1)),
        ("double", V::Double(1.0), V::I64(1)),
        ("string", s("x"), V::Uuid),
        ("binary", s("x"), V::Double(1.0)),
        ("uuid", V::Uuid, s("x")),
    ];
    for (ty, good, bad) in cases {
        for (c, enc) in both(&format!("struct S {{ 1: {ty} v }}")) {
            assert_eq!(
                check(&c, &enc(&st(vec![(1, good.clone())]))),
                Ok(()),
                "{ty}"
            );
            assert_eq!(
                violation(&c, &enc(&st(vec![(1, bad.clone())]))),
                "/v: wire type does not match the type declared for the field",
                "{ty}"
            );
        }
    }
}

#[test]
fn framing_violations_are_named_with_their_path() {
    for (c, enc) in both(ORDER) {
        let full = enc(&full_order());
        assert_eq!(
            violation(&c, &[]),
            "<root>: payload ends before the value is complete"
        );
        assert_eq!(
            violation(&c, &full[..full.len() - 1]),
            "<root>: payload ends before the value is complete"
        );
        let mut trailing = full.clone();
        trailing.push(0);
        assert_eq!(
            violation(&c, &trailing),
            "<root>: bytes remain after the value"
        );
        assert_eq!(
            violation(&c, &enc(&st(vec![(2, s("x"))]))),
            "/id: required field is missing"
        );
        assert_eq!(
            violation(&c, &enc(&st(vec![(1, V::I64(1)), (3, V::I32(2))]))),
            "/status: enum value is not declared in the enum"
        );
        assert_eq!(
            violation(&c, &enc(&st(vec![(1, V::I64(1)), (2, V::Bin(vec![0xff]))]))),
            "/customer: string is not valid UTF-8"
        );
        assert_eq!(
            violation(
                &c,
                &enc(&st(vec![
                    (1, V::I64(1)),
                    (4, V::List(Wt::Struct, vec![st(vec![])]))
                ]))
            ),
            "/lines/[]/sku: required field is missing"
        );
        assert_eq!(
            violation(
                &c,
                &enc(&st(vec![
                    (1, V::I64(1)),
                    (4, V::List(Wt::I32, vec![V::I32(1)]))
                ]))
            ),
            "/lines: element wire type does not match the type declared for the field"
        );
        assert_eq!(
            violation(
                &c,
                &enc(&st(vec![
                    (1, V::I64(1)),
                    (5, V::Map(Wt::Binary, Wt::I64, vec![(s("k"), V::I64(1))]))
                ]))
            ),
            "/tags: element wire type does not match the type declared for the field"
        );
        assert_eq!(
            violation(
                &c,
                &enc(&st(vec![
                    (1, V::I64(1)),
                    (
                        5,
                        V::Map(Wt::Binary, Wt::I32, vec![(V::Bin(vec![0xfe]), V::I32(1))])
                    )
                ]))
            ),
            "/tags/<key>: string is not valid UTF-8"
        );
    }
}

#[test]
fn a_union_carries_at_most_one_member_and_may_be_empty() {
    for (c, enc) in both(ORDER) {
        let payment = |members: Vec<(i16, V)>| enc(&st(vec![(1, V::I64(1)), (11, st(members))]));
        assert_eq!(check(&c, &payment(vec![])), Ok(()));
        assert_eq!(check(&c, &payment(vec![(2, V::I64(9))])), Ok(()));
        assert_eq!(
            violation(&c, &payment(vec![(1, s("a")), (2, V::I64(9))])),
            "/payment/voucher: more than one member of the union is set"
        );
        assert_eq!(
            violation(&c, &payment(vec![(1, s("a")), (1, s("b"))])),
            "/payment/card: more than one member of the union is set"
        );
        // An unknown member does not count.
        assert_eq!(
            check(&c, &payment(vec![(1, s("a")), (9, V::I64(9))])),
            Ok(())
        );
    }
}

#[test]
fn a_field_may_repeat_and_an_unknown_field_is_skipped_with_its_framing_checked() {
    for (c, enc) in both(ORDER) {
        assert_eq!(
            check(
                &c,
                &enc(&st(vec![
                    (1, V::I64(1)),
                    (1, V::I64(2)),
                    (2, s("a")),
                    (2, s("b"))
                ]))
            ),
            Ok(())
        );
        let unknown = st(vec![
            (1, V::I64(1)),
            (
                50,
                V::List(
                    Wt::Map,
                    vec![V::Map(
                        Wt::Binary,
                        Wt::List,
                        vec![(
                            s("k"),
                            V::List(Wt::Struct, vec![st(vec![(1, V::Double(1.0))])]),
                        )],
                    )],
                ),
            ),
            (51, s("hi")),
            (52, V::Bool(false)),
            (53, V::Uuid),
            (30000, V::I16(5)),
        ]);
        let bytes = enc(&unknown);
        assert_eq!(check(&c, &bytes), Ok(()));
        for cut in 1..bytes.len() {
            let got = check(&c, &bytes[..cut]);
            assert!(
                matches!(got, Err(SchemaError::Violation(_))),
                "cut at {cut}: {got:?}"
            );
        }
    }
    // An unknown field whose type code is not a Thrift type is a violation.
    let c = compile(ORDER);
    assert_eq!(
        violation(&c, &[0x09, 0, 50, 0]),
        "<root>: wire type is not valid"
    );
    assert_eq!(
        violation(&c, &[0x11, 0, 50, 0]),
        "<root>: wire type is not valid"
    );
}

#[test]
fn nested_unknown_fields_report_the_unknown_segment() {
    for (c, enc) in both("struct S { 1: i32 a }") {
        let mut bytes = enc(&st(vec![(
            7,
            st(vec![(1, V::List(Wt::I32, vec![V::I32(1)]))]),
        )]));
        bytes.truncate(bytes.len() - 2);
        let got = violation(&c, &bytes);
        assert!(got.starts_with("/<unknown>"), "{got}");
    }
}

// Binary protocol.

#[test]
fn the_binary_protocol_checks_booleans_lengths_and_counts() {
    let c = compile("struct S { 1: bool b, 2: string s, 3: list<i32> l, 4: map<i32, i32> m }");
    assert_eq!(
        violation(&c, &[2, 0, 1, 2, 0]),
        "/b: boolean is not a valid value"
    );
    assert_eq!(
        violation(&c, &[2, 0, 1, 1]),
        "<root>: payload ends before the value is complete"
    );
    // string length: negative, longer than the rest
    assert_eq!(
        violation(&c, &[11, 0, 2, 0xff, 0xff, 0xff, 0xff, 0]),
        "/s: length or size is negative"
    );
    assert_eq!(
        violation(&c, &[11, 0, 2, 0, 0, 0, 9, b'a', 0]),
        "/s: declared length exceeds the remaining payload"
    );
    assert_eq!(
        violation(&c, &[11, 0, 2, 0x7f, 0xff, 0xff, 0xff, 0]),
        "/s: declared length exceeds the remaining payload"
    );
    // list: negative size, size past the bytes left
    assert_eq!(
        violation(&c, &[15, 0, 3, 8, 0xff, 0xff, 0xff, 0xff, 0]),
        "/l: length or size is negative"
    );
    assert_eq!(
        violation(&c, &[15, 0, 3, 8, 0x7f, 0xff, 0xff, 0xff, 0]),
        "/l: declared size exceeds the remaining payload"
    );
    assert_eq!(
        violation(&c, &[13, 0, 4, 8, 8, 0x7f, 0xff, 0xff, 0xff, 0]),
        "/m: declared size exceeds the remaining payload"
    );
    // An empty container's element types say nothing.
    assert_eq!(check(&c, &[15, 0, 3, 0x55, 0, 0, 0, 0, 0]), Ok(()));
    assert_eq!(check(&c, &[13, 0, 4, 0x55, 0x66, 0, 0, 0, 0, 0]), Ok(()));
    // ... but a non-empty one with a type that is no Thrift type is refused.
    assert_eq!(
        violation(&c, &[15, 0, 3, 0x55, 0, 0, 0, 1, 0]),
        "/l: wire type is not valid"
    );
}

// Compact protocol.

#[test]
fn the_compact_protocol_reads_deltas_long_ids_and_bool_nibbles() {
    let c = compile(&format!(
        "{COMPACT}struct S {{ 1: bool a, 3: bool b, 40: i32 c, 100: string d }}"
    ));
    let doc = |fields: Vec<(i16, V)>| cmp(&st(fields));
    assert_eq!(
        check(
            &c,
            &doc(vec![
                (1, V::Bool(true)),
                (3, V::Bool(false)),
                (40, V::I32(-1)),
                (100, s("x"))
            ])
        ),
        Ok(())
    );
    // A bool written as a byte (the binary way) is a wire mismatch in a struct.
    assert_eq!(
        violation(&c, &[0x13, 0x01, 0]),
        "/a: wire type does not match the type declared for the field"
    );
    // delta 15 from 0 is id 15 (unknown, skipped); delta pushing past i16 is refused.
    assert_eq!(check(&c, &[0xf3, 0x02, 0]), Ok(()));
    assert_eq!(
        violation(
            &c,
            &[0x03, 0xfe, 0xff, 0x03, 0x01, 0xf3, 0x02, 0xf3, 0x02, 0]
        ),
        "<root>: field id is out of range"
    );
    // Type 0 with a delta, and types 14 and 15, are no Thrift types.
    assert_eq!(violation(&c, &[0x10, 0]), "<root>: wire type is not valid");
    assert_eq!(violation(&c, &[0x1e, 0]), "<root>: wire type is not valid");
    assert_eq!(violation(&c, &[0x1f, 0]), "<root>: wire type is not valid");
    // The long form carries a zigzag i16: five bytes of it are too many.
    assert_eq!(
        violation(&c, &[0x05, 0x80, 0x80, 0x80, 0x80, 0x01]),
        "<root>: integer encoding is longer than allowed or overflows its type"
    );
}

#[test]
fn compact_varints_are_bounded_by_their_type() {
    let c = compile(&format!(
        "{COMPACT}struct S {{ 1: i16 a, 2: i32 b, 3: i64 c, 4: list<i32> l }}"
    ));
    let field = |delta_type: u8, bytes: &[u8]| {
        let mut out = vec![delta_type];
        out.extend(bytes);
        out.push(0);
        out
    };
    let long = "integer encoding is longer than allowed or overflows its type";
    // i16: three bytes fit up to 0xffff, a fourth byte or a larger value does not.
    assert_eq!(check(&c, &field(0x14, &[0xff, 0xff, 0x03])), Ok(()));
    assert_eq!(
        violation(&c, &field(0x14, &[0xff, 0xff, 0x07])),
        format!("/a: {long}")
    );
    assert_eq!(
        violation(&c, &field(0x14, &[0x80, 0x80, 0x80, 0x00])),
        format!("/a: {long}")
    );
    // i32: five bytes up to u32::MAX.
    assert_eq!(
        check(&c, &field(0x25, &[0xff, 0xff, 0xff, 0xff, 0x0f])),
        Ok(())
    );
    assert_eq!(
        violation(&c, &field(0x25, &[0xff, 0xff, 0xff, 0xff, 0x1f])),
        format!("/b: {long}")
    );
    assert_eq!(
        violation(&c, &field(0x25, &[0x80, 0x80, 0x80, 0x80, 0x80, 0x00])),
        format!("/b: {long}")
    );
    // i64: ten bytes, the last one carries only bit 63.
    let mut ten = vec![0xff; 9];
    ten.push(0x01);
    assert_eq!(check(&c, &field(0x36, &ten)), Ok(()));
    *ten.last_mut().unwrap() = 0x02;
    assert_eq!(violation(&c, &field(0x36, &ten)), format!("/c: {long}"));
    assert_eq!(
        violation(&c, &field(0x36, &[0x80; 11])),
        format!("/c: {long}")
    );
    // Cut short.
    assert_eq!(
        violation(&c, &field(0x25, &[0x80])),
        "<root>: payload ends before the value is complete"
    );
}

#[test]
fn compact_collections_carry_a_size_and_an_element_type() {
    let c = compile(&format!(
        "{COMPACT}struct S {{ 1: list<bool> l, 2: map<string, i32> m, 3: list<i32> n, 4: list<string> t }}"
    ));
    // Elements of a bool list are 1 or 2, never 0 or 3.
    assert_eq!(check(&c, &[0x19, 0x21, 0x01, 0x02, 0]), Ok(()));
    assert_eq!(check(&c, &[0x19, 0x22, 0x01, 0x02, 0]), Ok(()));
    assert_eq!(
        violation(&c, &[0x19, 0x21, 0x00, 0]),
        "/l/[]: boolean is not a valid value"
    );
    assert_eq!(
        violation(&c, &[0x19, 0x21, 0x03, 0]),
        "/l/[]: boolean is not a valid value"
    );
    // The long size form, and a size past the bytes left.
    let mut long = vec![0x19, 0xf1, 0x14];
    long.extend(vec![1u8; 20]);
    long.push(0);
    assert_eq!(check(&c, &long), Ok(()));
    assert_eq!(
        violation(&c, &[0x19, 0xf1, 0xff, 0xff, 0xff, 0xff, 0x07, 0]),
        "/l: declared size exceeds the remaining payload"
    );
    assert_eq!(
        violation(&c, &[0x19, 0xf1, 0xff, 0xff, 0xff, 0xff, 0x0f, 0]),
        "/l: length or size is negative"
    );
    // Element type that is not the declared one, and one that is no type.
    assert_eq!(
        violation(&c, &[0x39, 0x14, 0x02, 0]),
        "/n: element wire type does not match the type declared for the field"
    );
    assert_eq!(
        violation(&c, &[0x39, 0x1e, 0x02, 0]),
        "/n: wire type is not valid"
    );
    // A map: the empty form is one zero byte, the other has a size and a type byte.
    assert_eq!(check(&c, &[0x2b, 0x00, 0]), Ok(()));
    assert_eq!(check(&c, &[0x2b, 0x01, 0x85, 0x01, b'a', 0x04, 0]), Ok(()));
    assert_eq!(
        violation(&c, &[0x2b, 0x01, 0x88, 0x01, b'a', 0x04, 0]),
        "/m: element wire type does not match the type declared for the field"
    );
    assert_eq!(
        violation(&c, &[0x2b, 0x01, 0x85, 0x09, b'a', 0x04, 0]),
        "/m/<key>: declared length exceeds the remaining payload"
    );
    // Binary lengths are varints.
    assert_eq!(
        violation(&c, &[0x49, 0x18, 0x05, b'a', 0]),
        "/t/[]: declared length exceeds the remaining payload"
    );
    assert_eq!(check(&c, &[0x49, 0x18, 0x01, b'a', 0]), Ok(()));
}

// Limits.

#[test]
fn nesting_deeper_than_the_walk_allows_is_a_limit_not_a_violation() {
    for (c, enc) in both("struct N { 1: optional N next }") {
        let mut v = st(vec![]);
        for _ in 0..63 {
            v = st(vec![(1, v)]);
        }
        assert_eq!(check(&c, &enc(&v)), Ok(()));
        let deep = st(vec![(1, v)]);
        assert!(limit(&c, &enc(&deep)).ends_with(": value is nested too deeply to check"));
    }
    // The same through unknown fields and containers.
    for (c, enc) in both("struct S { 1: i32 a }") {
        let mut v = V::I32(1);
        for _ in 0..100 {
            v = V::List(v.wt(), vec![v]);
        }
        assert!(limit(&c, &enc(&st(vec![(9, v)]))).ends_with("nested too deeply to check"));
    }
    // Recursion by struct and by container, 5000 deep, on a small stack, refused at once.
    let c = compile("struct N { 1: optional list<N> kids }");
    let mut payload = Vec::new();
    for _ in 0..5000 {
        payload.extend([15, 0, 1, 12, 0, 0, 0, 1, 12, 0, 1]);
    }
    let c = std::sync::Arc::new(c);
    let verdict = timed("deep payload", Duration::from_secs(1), || {
        let c = c.clone();
        on_stack(256, move || THRIFT_OPS.validate(&c, &payload))
    });
    assert!(matches!(
        verdict,
        Err(SchemaError::Violation(_) | SchemaError::LimitExceeded(_))
    ));
}

#[test]
fn bombs_are_refused_before_anything_is_iterated() {
    let c = compile("struct S { 1: list<i32> l, 2: map<string, list<i64>> m, 3: set<string> t, 4: list<list<list<byte>>> d }");
    let compact = compile(&format!(
        "{COMPACT}struct S {{ 1: list<i32> l, 2: map<string, list<i64>> m, 3: set<string> t }}"
    ));
    let cases: Vec<(&CompiledSchema, Vec<u8>)> = vec![
        (&c, vec![15, 0, 1, 8, 0x7f, 0xff, 0xff, 0xff]),
        (&c, vec![13, 0, 2, 11, 15, 0x7f, 0xff, 0xff, 0xff]),
        (&c, vec![14, 0, 3, 11, 0x3f, 0xff, 0xff, 0xff]),
        (&c, vec![15, 0, 4, 15, 0x00, 0xff, 0xff, 0xff, 15, 15]),
        (&compact, vec![0x19, 0xf5, 0xff, 0xff, 0xff, 0xff, 0x07]),
        (&compact, vec![0x2b, 0xff, 0xff, 0xff, 0xff, 0x07, 0x89]),
        (&compact, vec![0x3a, 0xf8, 0xff, 0xff, 0xff, 0x07]),
    ];
    for (schema, payload) in cases {
        let verdict = timed("length bomb", Duration::from_millis(50), || {
            check(schema, &payload)
        });
        assert!(
            matches!(verdict, Err(SchemaError::Violation(_))),
            "{payload:?}: {verdict:?}"
        );
    }
}

#[test]
fn a_run_of_tiny_values_is_walked_in_bounded_time_and_charged() {
    for (c, enc) in both("struct S { 1: list<bool> l, 2: list<struct_t> s } struct struct_t {}") {
        let items = vec![V::Bool(true); 1_000_000];
        let doc = enc(&st(vec![(1, V::List(Wt::Bool, items))]));
        let verdict = timed("a million booleans", Duration::from_secs(2), || {
            check(&c, &doc)
        });
        assert_eq!(verdict, Ok(()));
        let empties = enc(&st(vec![(
            2,
            V::List(Wt::Struct, vec![st(vec![]); 1_000_000]),
        )]));
        timed("a million empty structs", Duration::from_secs(2), || {
            assert_eq!(check(&c, &empties), Ok(()));
        });
    }
}

#[test]
fn a_run_of_fixed_size_values_is_skipped_but_still_charged() {
    let c = compile("struct S { 1: list<byte> b }");
    let bytes = bin(&st(vec![(1, V::List(Wt::Byte, vec![V::Byte(1); 1000]))]));
    assert_eq!(check(&c, &bytes), Ok(()));
    let mut shared = ValidationBudget::for_batch("org", 0);
    shared.spend(ValidationBudget::BATCH_BASE_UNITS);
    assert!(matches!(
        THRIFT_OPS.validate_metered(&c, &bytes, &mut shared),
        Err(SchemaError::LimitExceeded(m)) if m == "<root>: document exceeds the validation work budget"
    ));
}

#[test]
fn a_batch_shares_one_validation_budget() {
    let c = compile("struct S { 1: list<bool> l }");
    let items = V::List(Wt::Bool, vec![V::Bool(true); 100]);
    let doc = bin(&st(vec![(1, items)]));
    let mut shared = ValidationBudget::for_batch("org", 0);
    shared.spend(ValidationBudget::BATCH_BASE_UNITS - 150);
    assert_eq!(THRIFT_OPS.validate_metered(&c, &doc, &mut shared), Ok(()));
    assert!(matches!(
        THRIFT_OPS.validate_metered(&c, &doc, &mut shared),
        Err(SchemaError::LimitExceeded(_))
    ));
}

#[test]
fn a_document_over_the_per_document_cap_is_a_limit() {
    let c = compile("struct S { 1: list<list<bool>> l }");
    // 40 000 lists of 500 booleans: well under a million bytes, ~20M elements.
    let inner = V::List(Wt::Bool, vec![V::Bool(true); 500]);
    let doc = bin(&st(vec![(1, V::List(Wt::List, vec![inner; 40_000]))]));
    let verdict = timed("over the cap", Duration::from_secs(20), || check(&c, &doc));
    assert!(
        matches!(verdict, Err(SchemaError::LimitExceeded(m)) if m.ends_with("validation work budget"))
    );
}

#[test]
fn a_long_string_is_checked_for_utf8_without_copying_it() {
    let c = compile("struct S { 1: string s, 2: binary b }");
    let big = vec![b'a'; 5_000_000];
    let mut doc = bin(&st(vec![
        (1, V::Bin(big.clone())),
        (2, V::Bin(vec![0xff; 100])),
    ]));
    timed("5 MB string", Duration::from_secs(1), || {
        assert_eq!(check(&c, &doc), Ok(()));
    });
    let at = 2 + 3 + 4 + 2_500_000;
    doc[at] = 0xff;
    assert_eq!(violation(&c, &doc), "/s: string is not valid UTF-8");
}

// -----------------------------------------------------------------------------
// Compatibility
// -----------------------------------------------------------------------------

fn compat(old: &str, new: &str, mode: Compatibility) -> Result<(), SchemaError> {
    THRIFT_OPS.check_compatibility(old, new, mode)
}

fn incompatible(old: &str, new: &str, mode: Compatibility) -> String {
    match compat(old, new, mode) {
        Err(SchemaError::Incompatible(m)) => m,
        other => panic!("expected an incompatibility, got {other:?}"),
    }
}

#[test]
fn compatibility_matrix_for_fields() {
    use Compatibility::{Backward, Forward, Full};
    let old = "struct S { 1: required i32 a, 2: optional string b }";
    // (new text, [backward, forward, full] compatible?)
    for (new, expect) in [
        (
            "struct S { 1: required i32 a, 2: optional string b }",
            [true, true, true],
        ),
        // renaming a field keeps its id
        (
            "struct S { 1: required i32 renamed, 2: optional string b }",
            [true, true, true],
        ),
        // an optional field added
        (
            "struct S { 1: required i32 a, 2: optional string b, 3: i64 c }",
            [true, true, true],
        ),
        // a required field added
        (
            "struct S { 1: required i32 a, 2: optional string b, 3: required i64 c }",
            [false, true, false],
        ),
        // a required field removed
        ("struct S { 2: optional string b }", [true, false, false]),
        // an optional field removed
        ("struct S { 1: required i32 a }", [true, true, true]),
        // required relaxed, optional tightened
        (
            "struct S { 1: i32 a, 2: optional string b }",
            [true, false, false],
        ),
        (
            "struct S { 1: required i32 a, 2: required string b }",
            [false, true, false],
        ),
        // type changes
        (
            "struct S { 1: required i64 a, 2: optional string b }",
            [false, false, false],
        ),
        (
            "struct S { 1: required i32 a, 2: optional binary b }",
            [true, false, false],
        ),
        (
            "struct S { 1: required i32 a, 2: optional i32 b }",
            [false, false, false],
        ),
    ] {
        for (mode, want) in [Backward, Forward, Full].into_iter().zip(expect) {
            let got = compat(old, new, mode);
            assert_eq!(got.is_ok(), want, "{mode:?}: {new} -> {got:?}");
        }
    }
    assert_eq!(compat(old, "garbage", Compatibility::None), Ok(()));
}

#[test]
fn compatibility_messages_name_the_field_the_types_and_the_direction() {
    use Compatibility::{Backward, Forward};
    let got = incompatible(
        "struct S { 1: list<i32> a }",
        "struct S { 1: list<string> a }",
        Backward,
    );
    assert!(
        got.starts_with("backward: data written under the old schema"),
        "{got}"
    );
    assert!(
        got.contains(
            "/a/[]: i32 written by the old schema cannot be read as string by the new schema"
        ),
        "{got}"
    );
    let got = incompatible(
        "struct S { 1: i32 a }",
        "struct S { 1: i32 a, 2: required i32 b }",
        Backward,
    );
    assert!(got.contains("/b: the new schema requires field 'b' (id 2) of struct 'S', which the old schema does not always write"), "{got}");
    let got = incompatible(
        "struct S { 1: required i32 a }",
        "struct S { 2: i32 b }",
        Forward,
    );
    assert!(got.contains("/a: the old schema requires field 'a' (id 1) of struct 'S', which the new schema does not always write"), "{got}");
    let got = incompatible(
        "struct S { 1: map<string, i32> a }",
        "struct S { 1: map<string, i64> a }",
        Backward,
    );
    assert!(
        got.contains("/a/<value>: i32 written by the old schema cannot be read as i64"),
        "{got}"
    );
    let got = incompatible(
        "struct S { 1: set<i32> a }",
        "struct S { 1: list<i32> a }",
        Backward,
    );
    assert!(
        got.contains("/a: set<i32> written by the old schema cannot be read as list<i32>"),
        "{got}"
    );
}

#[test]
fn enums_structs_and_kinds_are_compared_by_what_is_on_the_wire() {
    use Compatibility::{Backward, Forward};
    let old = "enum E { A, B } struct S { 1: E e }";
    let new = "enum E { A } struct S { 1: E e }";
    // The reader needs every number the writer declares.
    assert!(incompatible(old, new, Backward)
        .contains("/e: enum 'E': value 1 of the old schema is unknown to the new schema"));
    assert_eq!(compat(old, new, Forward), Ok(()));
    assert!(incompatible(new, old, Forward)
        .contains("value 1 of the new schema is unknown to the old schema"));
    // Enum names do not matter, numbers do.
    assert_eq!(
        compat(
            old,
            "enum Other { X, Y } struct S { 1: Other e }",
            Compatibility::Full
        ),
        Ok(())
    );
    assert!(compat(
        "struct S { 1: E e } enum E { A }",
        "struct S { 1: i32 e }",
        Backward
    )
    .is_err());
    // Nested structs are compared by ids.
    let nested =
        |inner: &str| format!("struct S {{ 1: In in, 2: list<In> ins }} struct In {{ {inner} }}");
    assert_eq!(
        compat(
            &nested("1: i32 a"),
            &nested("1: i32 b, 2: i32 c"),
            Compatibility::Full
        ),
        Ok(())
    );
    let got = incompatible(&nested("1: i32 a"), &nested("1: string a"), Backward);
    assert!(
        got.contains("/in/a: i32 written by the old schema cannot be read as string"),
        "{got}"
    );
    // struct, union and exception do not mix.
    let got = incompatible("struct S { 1: i32 a }", "union S { 1: i32 a }", Backward);
    assert!(got.contains("<root>: struct 'S' written by the old schema cannot be read as union 'S' by the new schema"), "{got}");
    let got = incompatible(
        "struct S { 1: In x } struct In { 1: i32 a }",
        "struct S { 1: In x } exception In { 1: i32 a }",
        Backward,
    );
    assert!(
        got.contains("/x: struct 'In' written by the old schema cannot be read as exception 'In'"),
        "{got}"
    );
    // Typedefs are followed.
    assert_eq!(
        compat(
            "typedef i32 N struct S { 1: N a }",
            "struct S { 1: i32 a }",
            Compatibility::Full
        ),
        Ok(())
    );
}

#[test]
fn the_compared_root_follows_the_directive_and_recursion_terminates() {
    let a = "struct Other { 1: string x }\n// tentabus:struct R\nstruct R { 1: i32 a, 2: optional R next, 3: list<R> kids }";
    assert_eq!(compat(a, a, Compatibility::Full), Ok(()));
    assert!(compat(
        a,
        &a.replace("1: i32 a", "1: i64 a"),
        Compatibility::Backward
    )
    .is_err());
}

#[test]
fn a_comparison_that_runs_out_of_budget_is_limit_exceeded_not_compatible() {
    let mut text = String::from("struct S {");
    for i in 1..=1024 {
        text.push_str(&format!("{i}: list<map<string, list<i32>>> f{i}, "));
    }
    text.push('}');
    // Many schemas of the same shape: each compare is cheap, so this passes ...
    assert_eq!(compat(&text, &text, Compatibility::Full), Ok(()));
    // ... while a typedef lattice of depth 200 is cut off by the depth cap, never "compatible".
    let mut lattice = String::from("typedef i32 M0\n");
    for i in 1..=200 {
        lattice.push_str(&format!("typedef list<M{}> M{i}\n", i - 1));
    }
    lattice.push_str("struct S { 1: M200 m }");
    let verdict = compat(&lattice, &lattice, Compatibility::Full);
    assert!(
        matches!(verdict, Err(SchemaError::LimitExceeded(ref m)) if m.contains("too complex")),
        "{verdict:?}"
    );
}

// -----------------------------------------------------------------------------
// Projection
// -----------------------------------------------------------------------------

fn allowed(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn a_projection_keeps_the_allowed_fields_and_the_types_they_reach() {
    let derived = THRIFT_OPS
        .derive_subschema(
            ORDER,
            &allowed(&["id", "status", "lines", "payment", "nope"]),
        )
        .unwrap();
    let c = compile(&derived);
    assert!(derived.starts_with("struct Order {"), "{derived}");
    for kept in [
        "enum Status",
        "struct Line",
        "union Payment",
        "1: required i64 id,",
        "4: list<Line> lines,",
    ] {
        assert!(derived.contains(kept), "{kept} in {derived}");
    }
    for gone in ["customer", "tags", "uuid", "13:"] {
        assert!(!derived.contains(gone), "{gone} in {derived}");
    }
    let full = bin(&full_order());
    // The projection is a schema of its own: the full document passes (unknown fields are skipped).
    assert_eq!(check(&c, &full), Ok(()));
    // A required field that is allowed stays required.
    assert_eq!(
        violation(&c, &bin(&st(vec![(3, V::I32(0))]))),
        "/id: required field is missing"
    );
    // Deterministic.
    assert_eq!(
        derived,
        THRIFT_OPS
            .derive_subschema(
                ORDER,
                &allowed(&["nope", "payment", "lines", "status", "id"])
            )
            .unwrap()
    );
}

#[test]
fn a_removed_required_field_is_dropped_not_kept_optional() {
    let derived = THRIFT_OPS
        .derive_subschema(ORDER, &allowed(&["customer"]))
        .unwrap();
    assert!(!derived.contains("required"), "{derived}");
    assert!(!derived.contains("struct Line"), "{derived}");
    assert_eq!(check(&compile(&derived), &bin(&st(vec![]))), Ok(()));
}

#[test]
fn a_projection_keeps_kind_protocol_typedefs_and_writes_the_root_first() {
    let text = "// tentabus:protocol compact\ntypedef list<E> Es\nenum E { A = 3, B }\nstruct Helper { 1: i8 h }\nunion Root { 1: Es es, 2: Helper h, 3: string gone }";
    let derived = THRIFT_OPS
        .derive_subschema(
            &format!("// tentabus:struct Root\n{text}"),
            &allowed(&["es", "h"]),
        )
        .unwrap();
    assert!(
        derived.starts_with("// tentabus:protocol compact\nunion Root {"),
        "{derived}"
    );
    for kept in [
        "typedef list<E> Es",
        "enum E {\n  A = 3,\n  B = 4,\n}",
        "1: Es es,",
        "2: Helper h,",
        "1: i8 h,",
    ] {
        assert!(derived.contains(kept), "{kept} in {derived}");
    }
    assert!(!derived.contains("gone"));
    let c = compile(&derived);
    assert_eq!(
        check(&c, &cmp(&st(vec![(1, V::List(Wt::I32, vec![V::I32(3)]))]))),
        Ok(())
    );
    // A projection of a projection is itself.
    assert_eq!(
        THRIFT_OPS
            .derive_subschema(&derived, &allowed(&["es", "h"]))
            .unwrap(),
        derived
    );
}

// -----------------------------------------------------------------------------
// Ops
// -----------------------------------------------------------------------------

#[test]
fn validate_refuses_a_foreign_compiled_schema() {
    let other = SchemaType::Protobuf
        .ops()
        .compile(r#"syntax = "proto3"; message M {}"#)
        .unwrap();
    assert!(matches!(
        THRIFT_OPS.validate(&other, &[0]),
        Err(SchemaError::Invalid(_))
    ));
}

#[test]
fn oversized_schema_text_is_refused() {
    let text = format!("struct S {{}} // {}", "x".repeat(MAX_SCHEMA_TEXT_BYTES));
    assert!(refusal(&text).contains("the limit is"));
}
