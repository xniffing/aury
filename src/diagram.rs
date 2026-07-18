//! Diagram generation: render an Aury module's design as text.
//!
//! `.aury` files carry enough explicit structure — resolved call targets, effect
//! rows, struct fields — that a design diagram is an exact read-only walk of the
//! typed AST, with no inference to reconstruct. We emit [Mermaid] because it
//! stays hermetic (a string, no `dot` binary) and renders natively in GitHub,
//! the README, and Markdown viewers.
//!
//! Two views share the same walk:
//!   - [`call_graph_mermaid`] — functions as nodes, calls as edges, with the
//!     effect row (`rng`, `fs`, …) shown as a badge and effectful functions
//!     styled. This is the on-thesis view: capability flow no ordinary call-graph
//!     tool can draw, because ordinary languages don't track effects.
//!   - [`types_mermaid`] — structs and their fields as a class diagram, with a
//!     composition edge wherever one struct's field references another.
//!
//! [Mermaid]: https://mermaid.js.org/

use crate::ast::{Expr, Module, ModuleItem};
use crate::types::{EffectRow, Type};

/// Which diagram to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Call graph with effect badges (the default).
    Call,
    /// Struct/data-model class diagram.
    Types,
}

impl Kind {
    pub fn parse(s: &str) -> Result<Kind, String> {
        match s {
            "call" => Ok(Kind::Call),
            "types" => Ok(Kind::Types),
            other => Err(format!(
                "unknown diagram kind '{}' (expected 'call' or 'types')",
                other
            )),
        }
    }
}

/// Render `m` as a Mermaid diagram of the requested `kind`.
pub fn render(m: &Module, kind: Kind) -> String {
    match kind {
        Kind::Call => call_graph_mermaid(m),
        Kind::Types => types_mermaid(m),
    }
}

// ---------------------------------------------------------------------------
// Call graph
// ---------------------------------------------------------------------------

/// A Mermaid `graph TD` of the module's functions and the calls between them.
/// Builtin ops (`i64.add`, `rng.next`, …) are not nodes; only local functions
/// are. Effectful functions carry a `⚡ <caps>` badge and the `effectful` style.
pub fn call_graph_mermaid(m: &Module) -> String {
    let fns: Vec<&crate::ast::FnDef> = m
        .items
        .iter()
        .filter_map(|it| match it {
            ModuleItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();
    let local_names: std::collections::HashSet<&str> =
        fns.iter().map(|f| f.name.as_str()).collect();

    let mut out = String::new();
    out.push_str(&format!("%% call graph for module `{}`\n", m.name));
    out.push_str("graph TD\n");

    // Nodes: one per function, labelled with its signature and effect badge.
    let mut effectful: Vec<String> = Vec::new();
    for f in &fns {
        let node = node_id(&f.name);
        let params = f
            .params
            .iter()
            .map(|p| format!("{}: {}", p.name, ty_label(&p.ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let badge = effect_badge(&f.effects);
        let label = format!("{}({}) -&gt; {}{}", f.name, params, ty_label(&f.ret), badge);
        out.push_str(&format!("  {}[\"{}\"]\n", node, label));
        if !f.effects.is_pure() {
            effectful.push(node);
        }
    }

    // Edges: a call from one local function to another. Deduplicated, so a
    // function that calls another twice yields a single edge.
    let mut edges: Vec<(String, String)> = Vec::new();
    for f in &fns {
        let mut callees: Vec<String> = Vec::new();
        collect_calls(&f.body, &mut callees);
        let mut seen = std::collections::HashSet::new();
        for callee in callees {
            if local_names.contains(callee.as_str()) && seen.insert(callee.clone()) {
                edges.push((f.name.clone(), callee));
            }
        }
    }
    for (from, to) in &edges {
        out.push_str(&format!("  {} --> {}\n", node_id(from), node_id(to)));
    }

    // Style effectful functions distinctly so capability use is visible at a
    // glance. Both a light and dark-readable fill.
    if !effectful.is_empty() {
        out.push_str(
            "  classDef effectful fill:#fde68a,stroke:#d97706,color:#000;\n",
        );
        out.push_str(&format!("  class {} effectful;\n", effectful.join(",")));
    }
    out
}

/// A `⚡ rng, fs read` badge for an effect row, or empty for a pure function.
fn effect_badge(e: &EffectRow) -> String {
    if e.is_pure() || e.caps.is_empty() {
        String::new()
    } else {
        format!("<br/>⚡ {}", e.caps.join(", "))
    }
}

/// Walk an expression, pushing every `Call` op name onto `out` (builtins and
/// locals alike; the caller filters to locals). Exhaustive over `Expr` so a new
/// variant is a compile error here rather than a silently missing edge.
fn collect_calls(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Call { op, args, .. } => {
            out.push(op.clone());
            for a in args {
                collect_calls(a, out);
            }
        }
        Expr::Lit { .. } | Expr::Ref { .. } => {}
        Expr::Let { init, body, .. } => {
            collect_calls(init, out);
            collect_calls(body, out);
        }
        Expr::If { cond, then, els, .. } => {
            collect_calls(cond, out);
            collect_calls(then, out);
            collect_calls(els, out);
        }
        Expr::Match { scrut, arms, .. } => {
            collect_calls(scrut, out);
            for arm in arms {
                collect_calls(&arm.body, out);
            }
        }
        Expr::Loop { body, .. } => collect_calls(body, out),
        Expr::Break { value, .. } => collect_calls(value, out),
        Expr::Set { value, .. } => collect_calls(value, out),
        Expr::Return { value, .. } => collect_calls(value, out),
        Expr::Block { stmts, tail, .. } => {
            for s in stmts {
                collect_calls(s, out);
            }
            collect_calls(tail, out);
        }
        Expr::Region { body, .. } => collect_calls(body, out),
        Expr::With { body, .. } => collect_calls(body, out),
        Expr::Copy { value, .. } => collect_calls(value, out),
        Expr::VecNew { elems, .. } => {
            for el in elems {
                collect_calls(el, out);
            }
        }
        Expr::Index { target, index, .. } => {
            collect_calls(target, out);
            collect_calls(index, out);
        }
        Expr::VecPush { target, value, .. } => {
            collect_calls(target, out);
            collect_calls(value, out);
        }
        Expr::Len { target, .. } => collect_calls(target, out),
        Expr::StructNew { fields, .. } => {
            for (_, v) in fields {
                collect_calls(v, out);
            }
        }
        Expr::Field { target, .. } => collect_calls(target, out),
        Expr::Cast { value, .. } => collect_calls(value, out),
    }
}

// ---------------------------------------------------------------------------
// Type / data model
// ---------------------------------------------------------------------------

/// A Mermaid `classDiagram` of the module's structs and their fields, with a
/// composition edge (`A --> B : field`) wherever one struct's field type
/// references another struct.
pub fn types_mermaid(m: &Module) -> String {
    let structs: Vec<&crate::ast::StructDef> = m
        .items
        .iter()
        .filter_map(|it| match it {
            ModuleItem::Struct(s) => Some(s),
            _ => None,
        })
        .collect();

    let mut out = String::new();
    out.push_str(&format!("%% data model for module `{}`\n", m.name));
    out.push_str("classDiagram\n");

    if structs.is_empty() {
        // A module with no structs still produces a valid (empty) diagram plus a
        // note, so the command never emits something Mermaid rejects.
        out.push_str(&format!("  note \"module {} defines no structs\"\n", m.name));
        return out;
    }

    for s in &structs {
        out.push_str(&format!("  class {} {{\n", node_id(&s.name)));
        for (field, ty) in &s.fields {
            out.push_str(&format!("    +{} {}\n", ty_token(ty), field));
        }
        out.push_str("  }\n");
    }

    // Composition edges to other structs referenced by a field's type.
    for s in &structs {
        let mut seen = std::collections::HashSet::new();
        for (field, ty) in &s.fields {
            for referenced in referenced_structs(ty) {
                if referenced != s.name && seen.insert((referenced.clone(), field.clone())) {
                    out.push_str(&format!(
                        "  {} --> {} : {}\n",
                        node_id(&s.name),
                        node_id(&referenced),
                        field
                    ));
                }
            }
        }
    }
    out
}

/// Every struct name reachable through a type (through vecs, results, refs).
fn referenced_structs(ty: &Type) -> Vec<String> {
    let mut acc = Vec::new();
    walk_struct_names(ty, &mut acc);
    acc
}

fn walk_struct_names(ty: &Type, acc: &mut Vec<String>) {
    match ty {
        Type::Struct(n) => acc.push(n.clone()),
        Type::Vec(inner) => walk_struct_names(inner, acc),
        Type::Ref { ty, .. } => walk_struct_names(ty, acc),
        Type::Result(ok, err) => {
            walk_struct_names(ok, acc);
            walk_struct_names(err, acc);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Shared rendering helpers
// ---------------------------------------------------------------------------

/// A Mermaid-safe node id derived from a function/struct name. Mermaid node ids
/// must be free of spaces and punctuation, so non-alphanumerics collapse to `_`.
fn node_id(name: &str) -> String {
    let mut id: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    // A leading digit is not a valid id start; prefix if needed.
    if id.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        id.insert(0, 'n');
    }
    id
}

/// A readable type label for a quoted flowchart node (parentheses are fine
/// inside quotes; only `"` and raw `<`/`>` need escaping, which our forms avoid).
fn ty_label(ty: &Type) -> String {
    match ty {
        Type::I64 => "i64".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        Type::Unit => "unit".into(),
        Type::Region => "region".into(),
        Type::Vec(t) => format!("vec[{}]", ty_label(t)),
        Type::Struct(n) => n.clone(),
        Type::Ref { region, mutable, ty } => format!(
            "ref[{} {} {}]",
            region,
            if *mutable { "mut" } else { "ref" },
            ty_label(ty)
        ),
        Type::Result(ok, err) => format!("result[{}, {}]", ty_label(ok), ty_label(err)),
    }
}

/// A single-token type for a `classDiagram` member line (no spaces, no
/// parentheses — those break the class-body grammar), e.g. `vec_i64`.
fn ty_token(ty: &Type) -> String {
    match ty {
        Type::I64 => "i64".into(),
        Type::F64 => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Str => "str".into(),
        Type::Unit => "unit".into(),
        Type::Region => "region".into(),
        Type::Vec(t) => format!("vec_{}", ty_token(t)),
        Type::Struct(n) => n.clone(),
        Type::Ref { ty, .. } => format!("ref_{}", ty_token(ty)),
        Type::Result(ok, err) => format!("result_{}_{}", ty_token(ok), ty_token(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::build_module;
    use crate::sexpr::parse;

    fn module(src: &str) -> Module {
        let xs = parse(src).unwrap();
        build_module(&xs[0]).unwrap()
    }

    #[test]
    fn call_graph_lists_local_edges_not_builtins() {
        let m = module(
            "(module m
               (fn helper (params (x i64)) (ret i64)
                 (body (call i64.add (ref x) (lit 1))))
               (fn main (params (x i64)) (ret i64)
                 (body (helper (ref x)))))",
        );
        let d = call_graph_mermaid(&m);
        assert!(d.contains("graph TD"));
        // Edge to the local function, by node id.
        assert!(d.contains("main --> helper"), "missing local edge:\n{}", d);
        // Builtins are never nodes or edges.
        assert!(!d.contains("i64.add"), "builtin leaked into graph:\n{}", d);
    }

    #[test]
    fn call_graph_badges_effectful_functions() {
        let m = module(
            "(module m
               (fn draw (params) (ret i64) (effects rng)
                 (body (call rng.next))))",
        );
        let d = call_graph_mermaid(&m);
        assert!(d.contains("⚡ rng"), "missing effect badge:\n{}", d);
        assert!(d.contains("class draw effectful;"), "missing style:\n{}", d);
    }

    #[test]
    fn types_diagram_renders_fields_and_composition() {
        let m = module(
            "(module m
               (struct Leaf (v i64))
               (struct Tree (name str) (leaves (vec (struct Leaf))))
               (fn touch (params (t (struct Tree))) (ret str)
                 (body (get (ref t) name))))",
        );
        let d = types_mermaid(&m);
        assert!(d.contains("classDiagram"));
        assert!(d.contains("class Tree"), "missing struct class:\n{}", d);
        assert!(d.contains("+vec_Leaf leaves"), "missing field token:\n{}", d);
        // Composition edge from Tree to the struct inside its vec field.
        assert!(d.contains("Tree --> Leaf : leaves"), "missing composition:\n{}", d);
    }

    #[test]
    fn types_diagram_handles_a_module_with_no_structs() {
        let m = module("(module m (fn f (params) (ret i64) (body (lit 0))))");
        let d = types_mermaid(&m);
        assert!(d.contains("classDiagram"));
        assert!(d.contains("no structs"), "missing empty-note:\n{}", d);
    }
}
