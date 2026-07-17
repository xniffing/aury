//! Typed AST. Each node carries a Merkle [`NodeId`] so repair patches address
//! nodes, not source lines. Conversion from raw s-expressions assigns ids
//! deterministically.

use crate::id::{sexpr_id, NodeId};
use crate::sexpr::Sexpr;
use crate::types::{EffectRow, Type};

#[derive(Clone, Debug)]
pub struct Module {
    pub id: NodeId,
    pub name: String,
    pub items: Vec<ModuleItem>,
}

#[derive(Clone, Debug)]
pub enum ModuleItem {
    Fn(FnDef),
    Spec(Spec),
    Extern(ExternDecl),
    Struct(StructDef),
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub id: NodeId,
    pub name: String,
    pub fields: Vec<(String, Type)>,
}

#[derive(Clone, Debug)]
pub struct FnDef {
    pub id: NodeId,
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub effects: EffectRow,
    /// Preconditions: bool expressions over the parameters, checked on entry.
    /// Multiple clauses are conjoined. See [`crate::spec`] for enforcement.
    pub requires: Vec<Expr>,
    /// Postconditions: bool expressions over the parameters plus the special
    /// binding `result` (the return value), checked on exit. Conjoined.
    pub ensures: Vec<Expr>,
    pub body: Expr,
}

/// The reserved name bound to a function's return value inside `ensures`
/// expressions.
pub const RESULT_BINDING: &str = "result";

#[derive(Clone, Debug)]
pub struct Param {
    pub id: NodeId,
    pub name: String,
    pub ty: Type,
    /// Capability parameters arrive in v1; reserved here.
    #[allow(dead_code)] pub is_cap: bool,
}

#[derive(Clone, Debug)]
pub struct ExternDecl {
    pub id: NodeId,
    pub name: String,
    pub abi: String,
    pub params: Vec<Param>,
    pub ret: Type,
    pub effects: EffectRow,
}

/// A spec block: contracts and properties attached to the containing module
/// or function.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub id: NodeId,
    pub contracts: Vec<Contract>,
    pub properties: Vec<Property>,
}

#[derive(Clone, Debug)]
pub struct Contract {
    pub id: NodeId,
    pub pre: Option<Expr>,
    pub post: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct Property {
    pub id: NodeId,
    pub name: String,
    /// (forall ((a i64) (b i64)) body) — bound names + their types
    pub forall: Vec<(String, Type)>,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Lit {
        id: NodeId,
        value: Lit,
    },
    /// Variable reference.
    Ref {
        id: NodeId,
        name: String,
    },
    /// Local binding: (let name type init body)
    Let {
        id: NodeId,
        name: String,
        ty: Type,
        init: Box<Expr>,
        body: Box<Expr>,
    },
    /// (call op arg1 arg2 ...) — op is a dotted name like i64.add or a local fn
    Call {
        id: NodeId,
        op: String,
        args: Vec<Expr>,
    },
    If {
        id: NodeId,
        cond: Box<Expr>,
        then: Box<Expr>,
        els: Box<Expr>,
    },
    /// (match scrutinee ((pat1) arm1) ((pat2) arm2) (else armElse))
    Match {
        id: NodeId,
        scrut: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    /// (loop body) — evaluates `body` repeatedly. A `break` inside exits the
    /// nearest enclosing loop and makes the loop expression evaluate to the
    /// break value; without a `break`, the loop diverges (runs until a
    /// `return`).
    Loop {
        id: NodeId,
        body: Box<Expr>,
    },
    /// (break value) — exit the nearest enclosing loop, yielding `value` as the
    /// loop's result.
    Break {
        id: NodeId,
        value: Box<Expr>,
    },
    /// (set name value) — reassign an existing mutable local binding. Yields
    /// unit. The target must be a `let`-bound local (not a parameter).
    Set {
        id: NodeId,
        name: String,
        value: Box<Expr>,
    },
    Return {
        id: NodeId,
        value: Box<Expr>,
    },
    Block {
        id: NodeId,
        stmts: Vec<Expr>,
        tail: Box<Expr>,
    },
    /// (region name body) — introduces a memory region; allocations live in it.
    Region {
        id: NodeId,
        name: String,
        body: Box<Expr>,
    },
    /// Explicit copy of an affine value.
    Copy {
        id: NodeId,
        value: Box<Expr>,
    },
    /// (vec-new t elem1 elem2 ...)
    VecNew {
        id: NodeId,
        ty: Type,
        elems: Vec<Expr>,
    },
    /// (idx target index)
    Index {
        id: NodeId,
        target: Box<Expr>,
        index: Box<Expr>,
    },
    /// (vec-push target value) — append `value`, yielding the grown vec. The
    /// `target` is consumed (moved): when it is a bare `(ref v)` the binding `v`
    /// is marked moved, so a later plain use is `USE_AFTER_MOVE`. Wrap the target
    /// in `(copy v)` to keep the original live.
    VecPush {
        id: NodeId,
        target: Box<Expr>,
        value: Box<Expr>,
    },
    /// (len target)
    Len {
        id: NodeId,
        target: Box<Expr>,
    },
    /// (new-struct Name (field1 val1) (field2 val2))
    StructNew {
        id: NodeId,
        name: String,
        fields: Vec<(String, Expr)>,
    },
    /// (get target field)
    Field {
        id: NodeId,
        target: Box<Expr>,
        field: String,
    },
    /// (cast target-type value)
    Cast {
        id: NodeId,
        target: Type,
        value: Box<Expr>,
    },
}

impl Expr {
    pub fn id(&self) -> NodeId {
        match self {
            Expr::Lit { id, .. } => *id,
            Expr::Ref { id, .. } => *id,
            Expr::Let { id, .. } => *id,
            Expr::Call { id, .. } => *id,
            Expr::If { id, .. } => *id,
            Expr::Match { id, .. } => *id,
            Expr::Loop { id, .. } => *id,
            Expr::Break { id, .. } => *id,
            Expr::Set { id, .. } => *id,
            Expr::Return { id, .. } => *id,
            Expr::Block { id, .. } => *id,
            Expr::Region { id, .. } => *id,
            Expr::Copy { id, .. } => *id,
            Expr::VecNew { id, .. } => *id,
            Expr::Index { id, .. } => *id,
            Expr::VecPush { id, .. } => *id,
            Expr::Len { id, .. } => *id,
            Expr::StructNew { id, .. } => *id,
            Expr::Field { id, .. } => *id,
            Expr::Cast { id, .. } => *id,
        }
    }

    /// True if this expression is a reference to a capability parameter.
    /// (Used by the effect checker to thread capabilities.)
    pub fn is_cap_value(&self) -> bool {
        matches!(self, Expr::Ref { name, .. } if name.starts_with("cap_"))
    }
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub id: NodeId,
    pub pattern: Pattern,
    pub body: Expr,
}

#[derive(Clone, Debug)]
pub enum Pattern {
    Lit(Lit),
    Wild,
    Bind(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Lit {
    I64(i64),
    /// Float literal, stored as its IEEE-754 bit pattern so `Lit` can keep its
    /// `Eq`/`Hash` derives (raw `f64` is neither). Recover the value with
    /// `f64::from_bits`. Native lowering emits the same bits as an LLVM
    /// `double 0x…` constant, so the literal is byte-identical across backends.
    F64(u64),
    Bool(bool),
    Str(String),
    Unit,
}

/// True if `a` should be read as an `f64` literal: it must fail to parse as an
/// `i64` and carry a decimal point (so bare `inf`/`nan`/identifiers are never
/// silently captured, and integers stay `i64`). Exponent-only forms like
/// `1e10` are intentionally *not* floats — write `1.0e10`.
pub fn parse_f64_literal(a: &str) -> Option<u64> {
    if a.parse::<i64>().is_ok() || !a.contains('.') {
        return None;
    }
    a.parse::<f64>().ok().map(f64::to_bits)
}

// ---------------------------------------------------------------------------
// Conversion from raw s-expressions into the typed AST, assigning Merkle ids.
// ---------------------------------------------------------------------------

pub fn build_module(s: &Sexpr) -> Result<Module, String> {
    let xs = s.list().ok_or("module must be a list")?;
    if xs.first().and_then(|x| x.atom()) != Some("module") {
        return Err("top-level form must be (module name ...)".into());
    }
    let name = xs
        .get(1)
        .and_then(|x| x.atom())
        .ok_or("module needs a name")?
        .to_string();
    let mut items = Vec::new();
    for item in &xs[2..] {
        items.push(build_item(item)?);
    }
    let id = sexpr_id(s);
    Ok(Module { id, name, items })
}

fn build_item(s: &Sexpr) -> Result<ModuleItem, String> {
    let head = s.head().ok_or("item must be a list")?;
    match head {
        "fn" => Ok(ModuleItem::Fn(build_fn(s)?)),
        "spec" => Ok(ModuleItem::Spec(build_spec(s)?)),
        "extern" => Ok(ModuleItem::Extern(build_extern(s)?)),
        "struct" => Ok(ModuleItem::Struct(build_struct(s)?)),
        other => Err(format!("unknown module item: {}", other)),
    }
}

fn build_fn(s: &Sexpr) -> Result<FnDef, String> {
    let xs = s.list().ok_or("fn must be a list")?;
    // (fn name (params ...) (ret T) [(effects ...)] (body ...))
    let name = xs.get(1).and_then(|x| x.atom()).ok_or("fn name")?.to_string();
    let mut idx = 2;
    let mut params = Vec::new();
    if xs.get(idx).and_then(|x| x.head()) == Some("params") {
        let px = xs[idx].list().ok_or("params list")?;
        for p in &px[1..] {
            params.push(build_param(p)?);
        }
        idx += 1;
    }
    let mut ret = Type::Unit;
    if xs.get(idx).and_then(|x| x.head()) == Some("ret") {
        ret = Type::parse(&xs.get(idx).and_then(|x| x.list()).ok_or("ret form")?[1])?;
        idx += 1;
    }
    let mut effects = EffectRow::pure_row();
    if xs.get(idx).and_then(|x| x.head()) == Some("effects") {
        effects = EffectRow::parse(&xs[idx])?;
        idx += 1;
    }
    // Contract clauses: zero or more (requires E) / (ensures E) between the
    // effect row and the body. Order-independent; multiple clauses conjoin.
    let mut requires = Vec::new();
    let mut ensures = Vec::new();
    while let Some(head) = xs.get(idx).and_then(|x| x.head()) {
        match head {
            "requires" => {
                let e = xs[idx].list().ok_or("requires list")?.get(1).ok_or("requires expr")?;
                requires.push(build_expr(e)?);
                idx += 1;
            }
            "ensures" => {
                let e = xs[idx].list().ok_or("ensures list")?.get(1).ok_or("ensures expr")?;
                ensures.push(build_expr(e)?);
                idx += 1;
            }
            _ => break,
        }
    }
    if xs.get(idx).and_then(|x| x.head()) != Some("body") {
        return Err("fn needs a body".into());
    }
    let body_s = xs[idx].list().ok_or("body list")?[1].clone();
    let body = build_expr(&body_s)?;
    let id = sexpr_id(s);
    Ok(FnDef {
        id,
        name,
        params,
        ret,
        effects,
        requires,
        ensures,
        body,
    })
}

fn build_param(s: &Sexpr) -> Result<Param, String> {
    // (name type) or (cap name cap-name) for capability parameters
    let xs = s.list().ok_or("param must be a list")?;
    if xs.first().and_then(|x| x.atom()) == Some("cap") {
        let name = xs.get(1).and_then(|x| x.atom()).ok_or("cap name")?.to_string();
        let id = sexpr_id(s);
        Ok(Param {
            id,
            name,
            ty: Type::Region, // capabilities are region-like handles, simplified in v0
            is_cap: true,
        })
    } else {
        let name = xs.get(0).and_then(|x| x.atom()).ok_or("param name")?.to_string();
        let ty = Type::parse(xs.get(1).ok_or("param type")?)?;
        let id = sexpr_id(s);
        Ok(Param { id, name, ty, is_cap: false })
    }
}

fn build_extern(s: &Sexpr) -> Result<ExternDecl, String> {
    let xs = s.list().ok_or("extern must be a list")?;
    let name = xs
        .get(1)
        .and_then(|x| x.atom())
        .ok_or("extern name")?
        .to_string();
    let mut idx = 2;
    let mut abi = "c".to_string();
    if xs.get(idx).and_then(|x| x.head()) == Some("abi") {
        abi = xs[idx]
            .list()
            .ok_or("abi form")?
            .get(1)
            .and_then(|x| x.atom())
            .ok_or("abi value")?
            .to_string();
        idx += 1;
    }
    let mut params = Vec::new();
    if xs.get(idx).and_then(|x| x.head()) == Some("params") {
        for p in xs[idx].list().ok_or("params")?[1..].iter() {
            params.push(build_param(p)?);
        }
        idx += 1;
    }
    let mut ret = Type::Unit;
    if xs.get(idx).and_then(|x| x.head()) == Some("ret") {
        ret = Type::parse(xs[idx].list().ok_or("ret form")?.get(1).ok_or("ret type")?)?;
        idx += 1;
    }
    let mut effects = EffectRow::default();
    if xs.get(idx).and_then(|x| x.head()) == Some("effects") {
        effects = EffectRow::parse(&xs[idx])?;
    }
    let id = sexpr_id(s);
    Ok(ExternDecl {
        id,
        name,
        abi,
        params,
        ret,
        effects,
    })
}

fn build_struct(s: &Sexpr) -> Result<StructDef, String> {
    let xs = s.list().ok_or("struct must be a list")?;
    let name = xs.get(1).and_then(|x| x.atom()).ok_or("struct name")?.to_string();
    let mut fields = Vec::new();
    for f in xs.get(2..).into_iter().flatten() {
        let fx = f.list().ok_or("struct field is a list")?;
        let fname = fx.get(0).and_then(|x| x.atom()).ok_or("field name")?.to_string();
        let fty = Type::parse(fx.get(1).ok_or("field type")?)?;
        fields.push((fname, fty));
    }
    let id = sexpr_id(s);
    Ok(StructDef { id, name, fields })
}

fn build_spec(s: &Sexpr) -> Result<Spec, String> {
    let xs = s.list().ok_or("spec must be a list")?;
    let mut contracts = Vec::new();
    let mut properties = Vec::new();
    for item in xs.get(1..).into_iter().flatten() {
        let head = item.head().ok_or("spec item")?;
        match head {
            "pre" => {
                // attach a pre-only contract; we accumulate into a single
                // contract per spec for v0
                let body = build_expr(item.list().ok_or("pre body")?.get(1).ok_or("pre expr")?)?;
                let id = sexpr_id(item);
                contracts.push(Contract { id, pre: Some(body), post: None });
            }
            "post" => {
                let body = build_expr(item.list().ok_or("post body")?.get(1).ok_or("post expr")?)?;
                let id = sexpr_id(item);
                contracts.push(Contract { id, pre: None, post: Some(body) });
            }
            "property" => {
                // (property name (forall ((a i64) (b i64)) body))
                let inner = item.list().ok_or("property body")?;
                let pname = inner.get(1).and_then(|x| x.atom()).ok_or("property name")?.to_string();
                let forall_s = inner.get(2).ok_or("forall clause")?;
                let (forall, body) = build_forall(forall_s)?;
                let id = sexpr_id(item);
                properties.push(Property { id, name: pname, forall, body });
            }
            _ => return Err(format!("unknown spec item: {}", head)),
        }
    }
    let id = sexpr_id(s);
    Ok(Spec { id, contracts, properties })
}

fn build_forall(s: &Sexpr) -> Result<(Vec<(String, Type)>, Expr), String> {
    // (forall ((a T) (b T)) body)
    let xs = s.list().ok_or("forall must be a list")?;
    if xs.first().and_then(|x| x.atom()) != Some("forall") {
        return Err("expected (forall ((a T) ...) body)".into());
    }
    let bindings = xs.get(1).ok_or("forall bindings")?.list().ok_or("bindings list")?;
    let mut out = Vec::new();
    for b in bindings {
        let bx = b.list().ok_or("binding is a list")?;
        let name = bx.get(0).and_then(|x| x.atom()).ok_or("binding name")?.to_string();
        let ty = Type::parse(bx.get(1).ok_or("binding type")?)?;
        out.push((name, ty));
    }
    let body = build_expr(xs.get(2).ok_or("forall body")?)?;
    Ok((out, body))
}

/// If `s` is `(kw inner)`, return `inner`; otherwise return `s` unchanged.
/// Lets `if`/`match` arms accept either bare expressions or keyword-wrapped
/// forms like `(then E)` / `(else E)`.
fn unwrap_kw<'a>(s: &'a Sexpr, kw: &str) -> Result<&'a Sexpr, String> {
    if let Sexpr::List(xs) = s {
        if xs.len() == 2 && xs[0].atom() == Some(kw) {
            return Ok(&xs[1]);
        }
    }
    Ok(s)
}

fn build_expr(s: &Sexpr) -> Result<Expr, String> {
    match s {
        Sexpr::Atom(a) => {
            // bare atom: true/false/unit are literals; everything else is a
            // variable reference.
            if a == "true" {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::Bool(true) });
            }
            if a == "false" {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::Bool(false) });
            }
            if a == "unit" {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::Unit });
            }
            // integers are i64 literals
            if let Ok(n) = a.parse::<i64>() {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::I64(n) });
            }
            // decimals are f64 literals
            if let Some(bits) = parse_f64_literal(a) {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::F64(bits) });
            }
            let id = sexpr_id(s);
            Ok(Expr::Ref { id, name: a.clone() })
        }
        Sexpr::List(xs) => {
            if xs.is_empty() {
                let id = sexpr_id(s);
                return Ok(Expr::Lit { id, value: Lit::Unit });
            }
            let head = xs[0].atom().ok_or_else(|| {
                // If head is itself a list, treat the whole form as a (call ...).
                format!("expected a head atom, got {:?}", xs[0])
            })?;
            match head {
                "lit" => build_lit(s),
                "true" => {
                    let id = sexpr_id(s);
                    Ok(Expr::Lit { id, value: Lit::Bool(true) })
                }
                "false" => {
                    let id = sexpr_id(s);
                    Ok(Expr::Lit { id, value: Lit::Bool(false) })
                }
                "ref" => {
                    let name = xs.get(1).and_then(|x| x.atom()).ok_or("ref name")?.to_string();
                    let id = sexpr_id(s);
                    Ok(Expr::Ref { id, name })
                }
                "let" => {
                    let name = xs.get(1).and_then(|x| x.atom()).ok_or("let name")?.to_string();
                    let ty = Type::parse(xs.get(2).ok_or("let type")?)?;
                    let init = Box::new(build_expr(xs.get(3).ok_or("let init")?)?);
                    let body = Box::new(build_expr(xs.get(4).ok_or("let body")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Let { id, name, ty, init, body })
                }
                "call" => {
                    let op = xs.get(1).and_then(|x| x.atom()).ok_or("call op")?.to_string();
                    let args: Vec<Expr> = xs[2..]
                        .iter()
                        .map(build_expr)
                        .collect::<Result<_, _>>()?;
                    let _arg_ids: Vec<NodeId> = args.iter().map(|a| a.id()).collect();
                    let id = sexpr_id(s);
                    Ok(Expr::Call { id, op, args })
                }
                "if" => {
                    let cond = Box::new(build_expr(xs.get(1).ok_or("if cond")?)?);
                    // Accept either bare (if cond then else) or keyword-wrapped
                    // (if cond (then E) (else E)).
                    let then = Box::new(build_expr(unwrap_kw(xs.get(2).ok_or("if then")?, "then")?)?);
                    let els = if xs.len() >= 4 {
                        build_expr(unwrap_kw(xs.get(3).ok_or("if else")?, "else")?)?
                    } else {
                        let id = sexpr_id(s);
                        Expr::Lit { id, value: Lit::Unit }
                    };
                    let id = sexpr_id(s);
                    Ok(Expr::If { id, cond, then, els: Box::new(els) })
                }
                "match" => {
                    let scrut = Box::new(build_expr(xs.get(1).ok_or("match scrutinee")?)?);
                    let mut arms = Vec::new();
                    for arm in xs.get(2..).into_iter().flatten() {
                        let ax = arm.list().ok_or("match arm is a list")?;
                        let pat_s = &ax[0];
                        let body = build_expr(ax.get(1).ok_or("match arm body")?)?;
                        let pattern = build_pattern(pat_s)?;
                        let id = sexpr_id(s);
                        arms.push(MatchArm { id, pattern, body });
                    }
                    let _arm_ids: Vec<NodeId> = arms.iter().map(|a| a.id).collect();
                    let id = sexpr_id(s);
                    Ok(Expr::Match { id, scrut, arms })
                }
                "loop" => {
                    let body = Box::new(build_expr(xs.get(1).ok_or("loop body")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Loop { id, body })
                }
                "break" => {
                    // (break value); (break) yields unit.
                    let value = match xs.get(1) {
                        Some(v) => Box::new(build_expr(v)?),
                        None => Box::new(Expr::Lit { id: sexpr_id(s), value: Lit::Unit }),
                    };
                    let id = sexpr_id(s);
                    Ok(Expr::Break { id, value })
                }
                "set" => {
                    let name = xs.get(1).and_then(|x| x.atom()).ok_or("set name")?.to_string();
                    let value = Box::new(build_expr(xs.get(2).ok_or("set value")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Set { id, name, value })
                }
                "return" => {
                    let value = Box::new(build_expr(xs.get(1).ok_or("return value")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Return { id, value })
                }
                "block" => {
                    let stmts: Vec<Expr> = xs[1..xs.len().saturating_sub(1)]
                        .iter()
                        .map(build_expr)
                        .collect::<Result<_, _>>()?;
                    let tail = Box::new(build_expr(xs.last().ok_or("block tail")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Block { id, stmts, tail })
                }
                "region" => {
                    let name = xs.get(1).and_then(|x| x.atom()).ok_or("region name")?.to_string();
                    let body = Box::new(build_expr(xs.get(2).ok_or("region body")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Region { id, name, body })
                }
                "copy" => {
                    let value = Box::new(build_expr(xs.get(1).ok_or("copy value")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Copy { id, value })
                }
                "vec-new" => {
                    let ty = Type::parse(xs.get(1).ok_or("vec-new type")?)?;
                    let elems: Vec<Expr> = xs[2..]
                        .iter()
                        .map(build_expr)
                        .collect::<Result<_, _>>()?;
                    let _ids: Vec<NodeId> = elems.iter().map(|e| e.id()).collect();
                    let id = sexpr_id(s);
                    Ok(Expr::VecNew { id, ty, elems })
                }
                "idx" => {
                    let target = Box::new(build_expr(xs.get(1).ok_or("idx target")?)?);
                    let index = Box::new(build_expr(xs.get(2).ok_or("idx index")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Index { id, target, index })
                }
                "len" => {
                    let target = Box::new(build_expr(xs.get(1).ok_or("len target")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Len { id, target })
                }
                "vec-push" => {
                    let target = Box::new(build_expr(xs.get(1).ok_or("vec-push target")?)?);
                    let value = Box::new(build_expr(xs.get(2).ok_or("vec-push value")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::VecPush { id, target, value })
                }
                "new-struct" => {
                    let name = xs.get(1).and_then(|x| x.atom()).ok_or("struct name")?.to_string();
                    let mut fields = Vec::new();
                    for f in xs.get(2..).into_iter().flatten() {
                        let fx = f.list().ok_or("struct field")?;
                        let fname = fx.get(0).and_then(|x| x.atom()).ok_or("field name")?.to_string();
                        let val = build_expr(fx.get(1).ok_or("field value")?)?;
                        fields.push((fname, val));
                    }
                    let _ids: Vec<NodeId> = fields.iter().map(|(_, v)| v.id()).collect();
                    let id = sexpr_id(s);
                    Ok(Expr::StructNew { id, name, fields })
                }
                "get" => {
                    let target = Box::new(build_expr(xs.get(1).ok_or("get target")?)?);
                    let field = xs.get(2).and_then(|x| x.atom()).ok_or("get field")?.to_string();
                    let id = sexpr_id(s);
                    Ok(Expr::Field { id, target, field })
                }
                "cast" => {
                    let target = Type::parse(xs.get(1).ok_or("cast target type")?)?;
                    let value = Box::new(build_expr(xs.get(2).ok_or("cast value")?)?);
                    let id = sexpr_id(s);
                    Ok(Expr::Cast { id, target, value })
                }
                // A bare list whose head isn't a keyword is a call by head name.
                op => {
                    let args: Vec<Expr> = xs[1..]
                        .iter()
                        .map(build_expr)
                        .collect::<Result<_, _>>()?;
                    let _arg_ids: Vec<NodeId> = args.iter().map(|a| a.id()).collect();
                    let id = sexpr_id(s);
                    Ok(Expr::Call { id, op: op.to_string(), args })
                }
            }
        }
    }
}

fn build_pattern(s: &Sexpr) -> Result<Pattern, String> {
    match s {
        Sexpr::Atom(a) if a == "_" => Ok(Pattern::Wild),
        Sexpr::Atom(a) => {
            // try as integer literal
            if let Ok(n) = a.parse::<i64>() {
                return Ok(Pattern::Lit(Lit::I64(n)));
            }
            if let Some(bits) = parse_f64_literal(a) {
                return Ok(Pattern::Lit(Lit::F64(bits)));
            }
            if a == "true" {
                return Ok(Pattern::Lit(Lit::Bool(true)));
            }
            if a == "false" {
                return Ok(Pattern::Lit(Lit::Bool(false)));
            }
            Ok(Pattern::Bind(a.clone()))
        }
        Sexpr::List(xs) => {
            if xs.first().and_then(|x| x.atom()) == Some("lit") {
                let lit_s = xs.get(1).ok_or("lit value")?;
                match lit_s {
                    Sexpr::Atom(a) => {
                        if let Ok(n) = a.parse::<i64>() {
                            return Ok(Pattern::Lit(Lit::I64(n)));
                        }
                        if let Some(bits) = parse_f64_literal(a) {
                            return Ok(Pattern::Lit(Lit::F64(bits)));
                        }
                        Ok(Pattern::Lit(Lit::Str(a.clone())))
                    }
                    _ => Err("bad lit pattern".into()),
                }
            } else {
                Err("unsupported pattern".into())
            }
        }
    }
}

fn build_lit(s: &Sexpr) -> Result<Expr, String> {
    let xs = s.list().ok_or("lit must be a list")?;
    let v = xs.get(1).ok_or("lit value")?;
    match v {
        Sexpr::Atom(a) => {
            if let Ok(n) = a.parse::<i64>() {
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::I64(n) })
            } else if let Some(bits) = parse_f64_literal(a) {
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::F64(bits) })
            } else if a == "true" {
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::Bool(true) })
            } else if a == "false" {
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::Bool(false) })
            } else if a == "unit" {
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::Unit })
            } else {
                // treat as string literal
                let id = sexpr_id(s);
                Ok(Expr::Lit { id, value: Lit::Str(a.clone()) })
            }
        }
        _ => Err("lit value must be an atom".into()),
    }
}