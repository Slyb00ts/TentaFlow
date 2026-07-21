// ===== File: builder.rs — grammar AST and lowering to the flat automaton =====
// The GBNF parser, JSON-schema converter and regex converter all build the
// same small AST, then lower it here into the `Grammar` element streams the
// runtime automaton walks. Repetitions and groups are desugared into fresh
// auxiliary rules so the automaton only ever sees single-codepoint terminals
// and rule references.

use crate::grammar::{Elem, ElemType, Grammar};

/// One inclusive codepoint range set (a `[...]` char class or a literal).
#[derive(Debug, Clone)]
pub struct CharSet {
    pub negated: bool,
    pub ranges: Vec<(u32, u32)>,
}

impl CharSet {
    pub fn single(cp: u32) -> Self {
        Self {
            negated: false,
            ranges: vec![(cp, cp)],
        }
    }

    /// `.` — any codepoint except a raw newline (regex semantics).
    pub fn any_but_newline() -> Self {
        Self {
            negated: true,
            ranges: vec![(0x0A, 0x0A)],
        }
    }
}

/// One matchable item inside an alternate.
#[derive(Debug, Clone)]
pub enum Item {
    /// A single codepoint drawn from a char class.
    Class(CharSet),
    /// A literal string: lowers to a sequence of exact single-char classes.
    Literal(Vec<u32>),
    /// Reference to a named rule (resolved to an index at lowering time).
    Ref(String),
    /// Parenthesized group of alternates.
    Group(Vec<Vec<Item>>),
    /// `item{min,max}` — `max = None` means unbounded (`*`/`+`/`{n,}`).
    Repeat {
        item: Box<Item>,
        min: u32,
        max: Option<u32>,
    },
}

impl Item {
    /// A literal string item from its Unicode scalars.
    pub fn literal(s: &str) -> Item {
        Item::Literal(s.chars().map(|c| c as u32).collect())
    }
}

/// A named rule: a list of alternates, each a sequence of items.
#[derive(Debug, Clone)]
pub struct AstRule {
    pub name: String,
    pub alternates: Vec<Vec<Item>>,
}

/// Lowers a set of named AST rules into a flat [`Grammar`].
pub struct Lowerer {
    ast: Vec<AstRule>,
    rules: Vec<Vec<Elem>>,
    /// Rule name → flat rule index (pre-assigned for every named AST rule).
    names: std::collections::HashMap<String, usize>,
    aux_counter: usize,
}

impl Lowerer {
    pub fn lower(ast: Vec<AstRule>, root_name: &str) -> forge_types::Result<Grammar> {
        let mut names = std::collections::HashMap::new();
        for (i, r) in ast.iter().enumerate() {
            if names.insert(r.name.clone(), i).is_some() {
                return Err(forge_types::ForgeError::Grammar(format!(
                    "duplicate rule name `{}`",
                    r.name
                )));
            }
        }
        let root = *names.get(root_name).ok_or_else(|| {
            forge_types::ForgeError::Grammar(format!("root rule `{root_name}` not defined"))
        })?;
        let n = ast.len();
        let mut me = Lowerer {
            ast,
            rules: vec![Vec::new(); n],
            names,
            aux_counter: 0,
        };
        // Lower every named rule into its pre-assigned slot.
        for i in 0..me.ast.len() {
            let alts = me.ast[i].alternates.clone();
            let elems = me.lower_alternates(&alts)?;
            me.rules[i] = elems;
        }
        Ok(Grammar {
            rules: me.rules,
            root,
        })
    }

    /// Build a rule element stream for a list of alternates.
    fn lower_alternates(&mut self, alternates: &[Vec<Item>]) -> forge_types::Result<Vec<Elem>> {
        let mut elems = Vec::new();
        for (i, alt) in alternates.iter().enumerate() {
            if i > 0 {
                elems.push(Elem::new(ElemType::Alt, 0));
            }
            for item in alt {
                let part = self.lower_item(item)?;
                elems.extend(part);
            }
        }
        elems.push(Elem::new(ElemType::End, 0));
        Ok(elems)
    }

    /// Allocate a fresh auxiliary rule from raw alternates and return its index.
    fn add_aux(&mut self, alternates: Vec<Vec<Item>>) -> forge_types::Result<usize> {
        self.aux_counter += 1;
        let name = format!("__aux_{}", self.aux_counter);
        let idx = self.rules.len();
        self.rules.push(Vec::new());
        self.names.insert(name, idx);
        let elems = self.lower_alternates(&alternates)?;
        self.rules[idx] = elems;
        Ok(idx)
    }

    /// Lower one item into the elements representing a single occurrence.
    fn lower_item(&mut self, item: &Item) -> forge_types::Result<Vec<Elem>> {
        match item {
            Item::Class(cs) => Ok(lower_charset(cs)),
            Item::Literal(cps) => {
                let mut out = Vec::with_capacity(cps.len());
                for &cp in cps {
                    out.push(Elem::new(ElemType::Char, cp));
                }
                Ok(out)
            }
            Item::Ref(name) => {
                let idx = *self.names.get(name).ok_or_else(|| {
                    forge_types::ForgeError::Grammar(format!("undefined rule `{name}`"))
                })?;
                Ok(vec![Elem::new(ElemType::RuleRef, idx as u32)])
            }
            Item::Group(alts) => {
                let idx = self.add_aux(alts.clone())?;
                Ok(vec![Elem::new(ElemType::RuleRef, idx as u32)])
            }
            Item::Repeat { item, min, max } => self.lower_repeat(item, *min, *max),
        }
    }

    fn lower_repeat(
        &mut self,
        item: &Item,
        min: u32,
        max: Option<u32>,
    ) -> forge_types::Result<Vec<Elem>> {
        let mut out = Vec::new();
        // Mandatory prefix: `item` repeated `min` times inline.
        for _ in 0..min {
            out.extend(self.lower_item(item)?);
        }
        match max {
            None => {
                // Unbounded tail: `star ::= item star | ε`.
                let star = self.add_star(item)?;
                out.push(Elem::new(ElemType::RuleRef, star as u32));
            }
            Some(max) => {
                // `max - min` optional occurrences, nested so that a present
                // occurrence may be followed only by further optionals.
                let extra = max.saturating_sub(min);
                if extra > 0 {
                    let opt = self.add_optional_chain(item, extra)?;
                    out.push(Elem::new(ElemType::RuleRef, opt as u32));
                }
            }
        }
        Ok(out)
    }

    /// `S ::= item S | ε`.
    fn add_star(&mut self, item: &Item) -> forge_types::Result<usize> {
        self.aux_counter += 1;
        let name = format!("__star_{}", self.aux_counter);
        let idx = self.rules.len();
        self.rules.push(Vec::new());
        self.names.insert(name, idx);
        let mut first = self.lower_item(item)?;
        first.push(Elem::new(ElemType::RuleRef, idx as u32));
        let mut elems = first;
        elems.push(Elem::new(ElemType::Alt, 0));
        elems.push(Elem::new(ElemType::End, 0));
        self.rules[idx] = elems;
        Ok(idx)
    }

    /// A chain of `count` nested optionals: `O_k ::= item O_{k-1} | ε`.
    fn add_optional_chain(&mut self, item: &Item, count: u32) -> forge_types::Result<usize> {
        // Innermost first.
        let mut inner: Option<usize> = None;
        for _ in 0..count {
            self.aux_counter += 1;
            let name = format!("__opt_{}", self.aux_counter);
            let idx = self.rules.len();
            self.rules.push(Vec::new());
            self.names.insert(name, idx);
            let mut first = self.lower_item(item)?;
            if let Some(prev) = inner {
                first.push(Elem::new(ElemType::RuleRef, prev as u32));
            }
            let mut elems = first;
            elems.push(Elem::new(ElemType::Alt, 0));
            elems.push(Elem::new(ElemType::End, 0));
            self.rules[idx] = elems;
            inner = Some(idx);
        }
        Ok(inner.expect("count >= 1"))
    }
}

/// Lower a char set into its flat element encoding.
fn lower_charset(cs: &CharSet) -> Vec<Elem> {
    let mut out = Vec::new();
    if cs.ranges.is_empty() {
        // An empty positive class matches nothing; an empty negated class
        // matches anything. Encode "anything" as CharNot over an impossible
        // range, "nothing" as an impossible positive singleton.
        if cs.negated {
            out.push(Elem::new(ElemType::CharNot, 0x110000));
            out.push(Elem::new(ElemType::CharRangeUpper, 0x110000));
        } else {
            out.push(Elem::new(ElemType::Char, 0x110000));
        }
        return out;
    }
    for (i, &(a, b)) in cs.ranges.iter().enumerate() {
        let ty = if i == 0 {
            if cs.negated {
                ElemType::CharNot
            } else {
                ElemType::Char
            }
        } else {
            ElemType::CharAlt
        };
        out.push(Elem::new(ty, a));
        if b != a {
            out.push(Elem::new(ElemType::CharRangeUpper, b));
        }
    }
    out
}
