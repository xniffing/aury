//! JSON ingest: the AI authoring interface.
//!
//! The thesis (see `aury-proposal.md`): the model should emit a *structured
//! tree*, not free text it has to hand-balance. This module converts a JSON
//! tree into the canonical [`Sexpr`] representation, which then flows through
//! the *existing* [`crate::ast::build_module`] path — so a program authored
//! as JSON and a program authored as s-expressions produce byte-identical IR
//! (identical Merkle node ids). The s-expression form stays canonical
//! on-disk; JSON is an *authoring* surface.
//!
//! Two JSON shapes are accepted by [`json_to_sexpr`]:
//!
//! 1. **Typed-object form** — what a model emits via a tool-call: explicit
//!    `"kind"` tags, no delimiter counting, no remembering that a call is
//!    `(call op args...)`. e.g.
//!    `{"kind":"call","op":"i64.add","args":[{"kind":"ref","name":"a"},...]}`
//! 2. **Array form** — the s-expression with `()` spelled as `[]` and atoms
//!    quoted: `["call","i64.add",["ref","a"],["lit","0"]]`. This is what
//!    [`sexpr_to_json`] produces, so `emit-json` → `ingest` round-trips.
//!
//! [`sexpr_to_json`] emits the array form, so an existing `.aury` can be
//! converted to JSON and back losslessly.

use crate::sexpr::Sexpr;
use crate::types::{EffectRow, Type};
use serde_json::Value;

/// Convert a typed-object or array JSON tree into the canonical [`Sexpr`].
pub fn json_to_sexpr(v: &Value) -> Result<Sexpr, String> {
    match v {
        // ---- array form: raw s-expr with [] for () ----
        Value::Array(xs) => {
            let mut out = Vec::with_capacity(xs.len());
            for x in xs {
                out.push(json_to_sexpr(x)?);
            }
            Ok(Sexpr::List(out))
        }
        Value::String(s) => Ok(Sexpr::Atom(s.clone())),
        Value::Number(n) => Ok(Sexpr::Atom(n.to_string())),
        Value::Bool(b) => Ok(Sexpr::Atom(b.to_string())),
        Value::Null => Ok(Sexpr::Atom("unit".into())),

        // ---- typed-object form ----
        Value::Object(obj) => {
            let kind = obj
                .get("kind")
                .and_then(|k| k.as_str())
                .ok_or_else(|| "JSON object needs a \"kind\" field".to_string())?;
            typed_to_sexpr(kind, obj)
        }
    }
}

/// Convert a [`Sexpr`] into the array-form JSON (lossless for round-tripping).
pub fn sexpr_to_json(s: &Sexpr) -> Value {
    match s {
        Sexpr::Atom(a) => {
            if let Ok(n) = a.parse::<i64>() {
                Value::Number(serde_json::Number::from(n))
            } else if a == "true" {
                Value::Bool(true)
            } else if a == "false" {
                Value::Bool(false)
            } else {
                Value::String(a.clone())
            }
        }
        Sexpr::List(xs) => Value::Array(xs.iter().map(sexpr_to_json).collect()),
    }
}

fn typed_to_sexpr(kind: &str, obj: &serde_json::Map<String, Value>) -> Result<Sexpr, String> {
    let atom = |s: &str| Sexpr::Atom(s.to_string());
    let list = |xs: Vec<Sexpr>| Sexpr::List(xs);
    let jarr = |key: &str| obj.get(key).and_then(|v| v.as_array());
    let jstr = |key: &str| obj.get(key).and_then(|v| v.as_str());
    let jeach = |key: &str| -> Vec<&Value> {
        jarr(key).map(|a| a.iter().collect()).unwrap_or_default()
    };

    match kind {
        "module" => {
            let name = jstr("name").ok_or("module.name")?;
            let mut items = vec![atom("module"), atom(name)];
            for it in jeach("items") {
                items.push(json_to_sexpr(it)?);
            }
            Ok(list(items))
        }
        "struct" => {
            let name = jstr("name").ok_or("struct.name")?;
            let mut items = vec![atom("struct"), atom(name)];
            for f in jeach("fields") {
                let fname = f.get("name").and_then(|v| v.as_str()).ok_or("field.name")?;
                let fty = f.get("type").and_then(|v| v.as_str()).ok_or("field.type")?;
                items.push(list(vec![atom(fname), ty_to_sexpr(fty)?]));
            }
            Ok(list(items))
        }
        "fn" => {
            let name = jstr("name").ok_or("fn.name")?;
            let mut params = vec![atom("params")];
            for p in jeach("params") {
                let pname = p.get("name").and_then(|v| v.as_str()).ok_or("param.name")?;
                let pty = p.get("type").and_then(|v| v.as_str()).ok_or("param.type")?;
                params.push(list(vec![atom(pname), ty_to_sexpr(pty)?]));
            }
            let mut items = vec![atom("fn"), atom(name), list(params)];
            items.push(list(vec![atom("ret"), ty_to_sexpr(jstr("ret").ok_or("fn.ret")?)?]));
            if let Some(effs) = jarr("effects") {
                if !effs.is_empty() {
                    let mut e = vec![atom("effects")];
                    for c in effs {
                        e.push(json_to_sexpr(c)?);
                    }
                    items.push(list(e));
                }
            }
            // Contract clauses, emitted after effects and before the body so
            // they line up with the s-expression `fn` grammar.
            for r in jeach("requires") {
                items.push(list(vec![atom("requires"), json_to_sexpr(r)?]));
            }
            for e in jeach("ensures") {
                items.push(list(vec![atom("ensures"), json_to_sexpr(e)?]));
            }
            let body = obj.get("body").ok_or("fn.body")?;
            items.push(list(vec![atom("body"), json_to_sexpr(body)?]));
            Ok(list(items))
        }
        "extern" => {
            let name = jstr("name").ok_or("extern.name")?;
            let mut params = vec![atom("params")];
            for p in jeach("params") {
                let pname = p.get("name").and_then(|v| v.as_str()).ok_or("param.name")?;
                let pty = p.get("type").and_then(|v| v.as_str()).ok_or("param.type")?;
                params.push(list(vec![atom(pname), ty_to_sexpr(pty)?]));
            }
            let mut items = vec![atom("extern"), atom(name)];
            items.push(list(vec![atom("abi"), atom(jstr("abi").unwrap_or("c"))]));
            items.push(list(params));
            items.push(list(vec![atom("ret"), ty_to_sexpr(jstr("ret").ok_or("extern.ret")?)?]));
            if let Some(effs) = jarr("effects") {
                if !effs.is_empty() {
                    let mut e = vec![atom("effects")];
                    for c in effs {
                        e.push(json_to_sexpr(c)?);
                    }
                    items.push(list(e));
                }
            }
            Ok(list(items))
        }
        "spec" => {
            let mut items = vec![atom("spec")];
            if let Some(contracts) = jarr("contracts") {
                for c in contracts {
                    if let Some(pre) = c.get("pre") {
                        items.push(list(vec![atom("pre"), json_to_sexpr(pre)?]));
                    }
                    if let Some(post) = c.get("post") {
                        items.push(list(vec![atom("post"), json_to_sexpr(post)?]));
                    }
                }
            }
            for p in jeach("properties") {
                let pname = p.get("name").and_then(|v| v.as_str()).ok_or("property.name")?;
                let mut bindings = vec![];
                for b in p.get("forall").and_then(|v| v.as_array()).unwrap_or(&Vec::new()) {
                    let bn = b.get("name").and_then(|v| v.as_str()).ok_or("forall.name")?;
                    let bt = b.get("type").and_then(|v| v.as_str()).ok_or("forall.type")?;
                    bindings.push(list(vec![atom(bn), ty_to_sexpr(bt)?]));
                }
                let body = p.get("body").ok_or("property.body")?;
                items.push(list(vec![
                    atom("property"),
                    atom(pname),
                    list(vec![atom("forall"), list(bindings), json_to_sexpr(body)?]),
                ]));
            }
            Ok(list(items))
        }
        // ---- expressions ----
        "lit" => {
            let v = obj.get("value").ok_or("lit.value")?;
            match v {
                Value::Null => Ok(list(vec![atom("lit"), atom("unit")])),
                Value::Bool(b) => Ok(atom(b.to_string().as_str())),
                Value::Number(n) => Ok(list(vec![atom("lit"), atom(n.to_string().as_str())])),
                Value::String(s) => Ok(list(vec![atom("lit"), atom(s)])),
                _ => Err("lit.value must be null/bool/number/string".into()),
            }
        }
        "ref" => {
            let name = jstr("name").ok_or("ref.name")?;
            Ok(list(vec![atom("ref"), atom(name)]))
        }
        "let" => {
            let name = jstr("name").ok_or("let.name")?;
            let ty = ty_to_sexpr(jstr("type").ok_or("let.type")?)?;
            let init = json_to_sexpr(obj.get("init").ok_or("let.init")?)?;
            let body = json_to_sexpr(obj.get("body").ok_or("let.body")?)?;
            Ok(list(vec![atom("let"), atom(name), ty, init, body]))
        }
        "call" => {
            let op = jstr("op").ok_or("call.op")?;
            let mut items = vec![atom("call"), atom(op)];
            for a in jeach("args") {
                items.push(json_to_sexpr(a)?);
            }
            Ok(list(items))
        }
        "if" => {
            let cond = json_to_sexpr(obj.get("cond").ok_or("if.cond")?)?;
            let then = json_to_sexpr(obj.get("then").ok_or("if.then")?)?;
            let els = json_to_sexpr(obj.get("else").ok_or("if.else")?)?;
            Ok(list(vec![
                atom("if"),
                cond,
                list(vec![atom("then"), then]),
                list(vec![atom("else"), els]),
            ]))
        }
        "match" => {
            let scrut = json_to_sexpr(obj.get("scrut").ok_or("match.scrut")?)?;
            let mut items = vec![atom("match"), scrut];
            for arm in jeach("arms") {
                let pat = arm.get("pattern").ok_or("match.arm.pattern")?;
                let body = arm.get("body").ok_or("match.arm.body")?;
                items.push(list(vec![pattern_to_sexpr(pat)?, json_to_sexpr(body)?]));
            }
            Ok(list(items))
        }
        "loop" => {
            let body = json_to_sexpr(obj.get("body").ok_or("loop.body")?)?;
            Ok(list(vec![atom("loop"), body]))
        }
        "break" => {
            // value is optional; (break) yields unit.
            match obj.get("value") {
                Some(v) => Ok(list(vec![atom("break"), json_to_sexpr(v)?])),
                None => Ok(list(vec![atom("break")])),
            }
        }
        "set" => {
            let name = jstr("name").ok_or("set.name")?;
            let value = json_to_sexpr(obj.get("value").ok_or("set.value")?)?;
            Ok(list(vec![atom("set"), atom(name), value]))
        }
        "return" => {
            let value = json_to_sexpr(obj.get("value").ok_or("return.value")?)?;
            Ok(list(vec![atom("return"), value]))
        }
        "block" => {
            let mut items = vec![atom("block")];
            for s in jeach("stmts") {
                items.push(json_to_sexpr(s)?);
            }
            items.push(json_to_sexpr(obj.get("tail").ok_or("block.tail")?)?);
            Ok(list(items))
        }
        "region" => {
            let name = jstr("name").ok_or("region.name")?;
            let body = json_to_sexpr(obj.get("body").ok_or("region.body")?)?;
            Ok(list(vec![atom("region"), atom(name), body]))
        }
        "copy" => {
            let value = json_to_sexpr(obj.get("value").ok_or("copy.value")?)?;
            Ok(list(vec![atom("copy"), value]))
        }
        "vec-new" => {
            let ty = ty_to_sexpr(jstr("type").ok_or("vec-new.type")?)?;
            let mut items = vec![atom("vec-new"), ty];
            for e in jeach("elems") {
                items.push(json_to_sexpr(e)?);
            }
            Ok(list(items))
        }
        "idx" => {
            let target = json_to_sexpr(obj.get("target").ok_or("idx.target")?)?;
            let index = json_to_sexpr(obj.get("index").ok_or("idx.index")?)?;
            Ok(list(vec![atom("idx"), target, index]))
        }
        "len" => {
            let target = json_to_sexpr(obj.get("target").ok_or("len.target")?)?;
            Ok(list(vec![atom("len"), target]))
        }
        "vec-push" => {
            let target = json_to_sexpr(obj.get("target").ok_or("vec-push.target")?)?;
            let value = json_to_sexpr(obj.get("value").ok_or("vec-push.value")?)?;
            Ok(list(vec![atom("vec-push"), target, value]))
        }
        "new-struct" => {
            let name = jstr("name").ok_or("new-struct.name")?;
            let mut items = vec![atom("new-struct"), atom(name)];
            for f in jeach("fields") {
                let fname = f.get("name").and_then(|v| v.as_str()).ok_or("new-struct.field.name")?;
                let fval = json_to_sexpr(f.get("value").ok_or("new-struct.field.value")?)?;
                items.push(list(vec![atom(fname), fval]));
            }
            Ok(list(items))
        }
        "get" => {
            let target = json_to_sexpr(obj.get("target").ok_or("get.target")?)?;
            let field = jstr("field").ok_or("get.field")?;
            Ok(list(vec![atom("get"), target, atom(field)]))
        }
        "cast" => {
            let ty = ty_to_sexpr(jstr("type").ok_or("cast.type")?)?;
            let value = json_to_sexpr(obj.get("value").ok_or("cast.value")?)?;
            Ok(list(vec![atom("cast"), ty, value]))
        }
        other => Err(format!("unknown JSON node kind: {}", other)),
    }
}

/// A match pattern to its s-expr.
fn pattern_to_sexpr(v: &Value) -> Result<Sexpr, String> {
    let atom = |s: &str| Sexpr::Atom(s.to_string());
    let obj = v
        .as_object()
        .ok_or_else(|| "pattern must be an object".to_string())?;
    let kind = obj
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or_else(|| "pattern needs a kind".to_string())?;
    match kind {
        "wild" => Ok(atom("_")),
        "bind" => Ok(atom(obj.get("name").and_then(|v| v.as_str()).ok_or("bind.name")?)),
        "lit" => {
            let val = obj.get("value").ok_or("pattern lit.value")?;
            match val {
                Value::Null => Ok(Sexpr::List(vec![atom("lit"), atom("unit")])),
                Value::Bool(b) => Ok(atom(b.to_string().as_str())),
                Value::Number(n) => Ok(Sexpr::List(vec![atom("lit"), atom(n.to_string().as_str())])),
                Value::String(s) => Ok(Sexpr::List(vec![atom("lit"), atom(s)])),
                _ => Err("pattern lit.value bad type".into()),
            }
        }
        other => Err(format!("unknown pattern kind: {}", other)),
    }
}

/// Parse a type *string* (e.g. "i64", "(vec i64)", "(struct Vec2)") into the
/// s-expr `Type::parse` expects. Reuses the s-expr reader so the forms match
/// exactly what hand-written `.aury` uses.
pub fn ty_to_sexpr(ty: &str) -> Result<Sexpr, String> {
    let xs = crate::sexpr::parse(ty).map_err(|e| format!("bad type {:?}: {}", ty, e))?;
    if xs.len() != 1 {
        return Err(format!("type {:?} must be one form", ty));
    }
    Ok(xs.into_iter().next().unwrap())
}

/// Convenience: JSON text → Sexpr.
pub fn parse_json_sexpr(text: &str) -> Result<Sexpr, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("JSON parse: {}", e))?;
    json_to_sexpr(&v)
}

/// Convenience: parse a typed-object JSON module straight to a typed AST.
pub fn build_module_from_json(text: &str) -> Result<crate::ast::Module, String> {
    let s = parse_json_sexpr(text)?;
    crate::ast::build_module(&s)
}

// Keep the `Type`/`EffectRow` parse helpers referenced so the module compiles
// even if unused externally.
#[allow(dead_code)]
fn _unused() {
    let _ = Type::I64;
    let _ = EffectRow::default();
}