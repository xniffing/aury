//! Lowering: Aury → LLVM IR → native executable.
//!
//! Lowers the scalar core, strings/results, immutable vectors/structs, and
//! deterministic RNG to LLVM IR text assembled by `clang`. The runtime in
//! `runtime/aury_rt.c` provides allocation, checked indexing, edge-case integer
//! operations, RNG state, and generic type-directed value display.
//!
//! Value model (type-aware): `lower_expr` returns (value, llvm_type, diverged).
//! i64/bool/unit → `i64` (bool is 0/1). str/vec/struct/result → `ptr` (boxed,
//! passed/returned by pointer). Codegen is alloca-based so `mem2reg` promotes
//! to SSA. `return`/`loop` diverge (tracked, like the validator's `diverges`).

use crate::ast::*;
use crate::interp::Value;
use crate::types::Type;
use std::collections::{HashMap, HashSet};

#[derive(Clone)]
struct Sig {
    params: Vec<Type>,
    ret: Type,
}

pub struct Lowerer {
    out: String,
    reg: usize,
    lbl: usize,
    str_n: usize,
    fns: HashMap<String, Sig>,
    structs: HashMap<String, StructDef>,
    scope: Vec<(String, String, Type)>, // name, slot, Aury type
    retslot: String,
    retty: String,
    errors: Vec<String>,
    str_literals: Vec<(String, String, String)>, // data name, boxed name, value
}

/// LLVM type string for an Aury type. Aggregates are boxed pointers.
fn llvm_type(t: &Type) -> String {
    match t {
        Type::I64 | Type::Bool | Type::Unit => "i64".into(),
        Type::Str | Type::Vec(_) | Type::Struct(_) | Type::Result(_, _) | Type::Ref { .. } | Type::Region => {
            "ptr".into()
        }
    }
}

/// Escape bytes for an LLVM `c"..."` constant and append its NUL terminator.
fn llvm_c_string(value: &str) -> String {
    let mut escaped = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b' '..=b'~' if *byte != b'"' && *byte != b'\\' => escaped.push(*byte as char),
            byte => escaped.push_str(&format!("\\{:02X}", byte)),
        }
    }
    escaped.push_str("\\00");
    escaped
}

fn emit_string_global(out: &mut String, data_name: &str, boxed_name: &str, value: &str) {
    let escaped = llvm_c_string(value);
    out.push_str(&format!(
        "{} = private constant [{} x i8] c\"{}\"\n",
        data_name,
        value.len() + 1,
        escaped
    ));
    out.push_str(&format!(
        "{} = private constant {{ i64, ptr }} {{ i64 {}, ptr {} }}\n",
        boxed_name,
        value.len(),
        data_name
    ));
}

pub fn lower_module(module: &Module) -> Result<String, String> {
    lower_set(module, &all_fns(module), true)
}

fn all_fns(module: &Module) -> HashSet<String> {
    module.items.iter().filter_map(|it| match it {
        ModuleItem::Fn(f) => Some(f.name.clone()),
        _ => None,
    }).collect()
}

pub fn reachable(module: &Module, entry: &str) -> HashSet<String> {
    let mut fns: HashMap<String, FnDef> = HashMap::new();
    for it in &module.items {
        if let ModuleItem::Fn(f) = it {
            fns.insert(f.name.clone(), f.clone());
        }
    }
    let mut set: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = vec![entry.to_string()];
    while let Some(name) = stack.pop() {
        if !set.insert(name.clone()) {
            continue;
        }
        if let Some(f) = fns.get(&name) {
            let mut calls = Vec::new();
            collect_calls(&f.body, &mut calls);
            for c in calls {
                if fns.contains_key(&c) && !set.contains(&c) {
                    stack.push(c);
                }
            }
        }
    }
    set
}

fn collect_calls(e: &Expr, out: &mut Vec<String>) {
    match e {
        Expr::Call { op, args, .. } => {
            out.push(op.clone());
            for a in args {
                collect_calls(a, out);
            }
        }
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
            for a in arms {
                collect_calls(&a.body, out);
            }
        }
        Expr::Loop { body, .. } => collect_calls(body, out),
        Expr::Return { value, .. } => collect_calls(value, out),
        Expr::Block { stmts, tail, .. } => {
            for s in stmts {
                collect_calls(s, out);
            }
            collect_calls(tail, out);
        }
        Expr::Region { body, .. } => collect_calls(body, out),
        Expr::Copy { value, .. } => collect_calls(value, out),
        Expr::Cast { value, .. } => collect_calls(value, out),
        Expr::VecNew { elems, .. } => {
            for e in elems {
                collect_calls(e, out);
            }
        }
        Expr::Index { target, index, .. } => {
            collect_calls(target, out);
            collect_calls(index, out);
        }
        Expr::Len { target, .. } => collect_calls(target, out),
        Expr::StructNew { fields, .. } => {
            for (_, v) in fields {
                collect_calls(v, out);
            }
        }
        Expr::Field { target, .. } => collect_calls(target, out),
        _ => {}
    }
}

fn lower_set(module: &Module, set: &HashSet<String>, skip_unsupported: bool) -> Result<String, String> {
    let mut l = Lowerer {
        out: String::new(),
        reg: 1,
        lbl: 1,
        str_n: 0,
        fns: HashMap::new(),
        structs: HashMap::new(),
        scope: Vec::new(),
        retslot: String::new(),
        retty: String::new(),
        errors: Vec::new(),
        str_literals: Vec::new(),
    };
    for item in &module.items {
        match item {
            ModuleItem::Fn(f) => {
                l.fns.insert(
                    f.name.clone(),
                    Sig {
                        params: f.params.iter().map(|p| p.ty.clone()).collect(),
                        ret: f.ret.clone(),
                    },
                );
            }
            ModuleItem::Struct(definition) => {
                l.structs
                    .entry(definition.name.clone())
                    .or_insert_with(|| definition.clone());
            }
            _ => {}
        }
    }
    l.out.push_str("; Aury native lowering (LLVM IR) - module ");
    l.out.push_str(&module.name);
    l.out.push('\n');
    l.out_str("declare i32 @printf(ptr, ...)\n");
    l.out_str("declare void @llvm.trap()\n");
    // Uniform 8-byte aggregate slot runtime ABI.
    l.out_str("declare ptr @aury_box_new(i64)\n");
    l.out_str("declare ptr @aury_box_slot(ptr, i64)\n");
    l.out_str("declare ptr @aury_vec_new(i64)\n");
    l.out_str("declare ptr @aury_vec_slot(ptr, i64)\n");
    l.out_str("declare void @aury_rng_init(i64)\n");
    l.out_str("declare i64 @aury_rng_next()\n");
    l.out_str("declare i64 @aury_i64_div(i64, i64)\n");
    l.out_str("declare i64 @aury_i64_mod(i64, i64)\n");
    l.out_str("declare void @aury_value_print(i64, ptr)\n");
    l.out_str("declare ptr @aury_str_concat(ptr, ptr)\n");
    l.out_str("declare i64 @aury_str_eq(ptr, ptr)\n");
    l.out_str("declare ptr @aury_i64_to_str(i64)\n");
    l.out_str("declare ptr @aury_i64_parse(ptr)\n");
    l.out_str("declare ptr @aury_i64_parse_strict(ptr)\n");
    l.out_str("declare void @aury_str_print(ptr)\n");
    l.out_str("@.fmt = private constant [6 x i8] c\"%lld\\0A\\00\"\n");
    l.out_str("@.t = private constant [6 x i8] c\"true\\0A\\00\"\n");
    l.out_str("@.f = private constant [7 x i8] c\"false\\0A\\00\"\n\n");
    for item in &module.items {
        if let ModuleItem::Fn(f) = item {
            if !set.contains(&f.name) {
                continue;
            }
            let mark = l.out.len();
            let literal_mark = l.str_literals.len();
            l.errors.clear();
            l.lower_fn(f);
            if !l.errors.is_empty() {
                if skip_unsupported {
                    l.out.truncate(mark);
                    l.str_literals.truncate(literal_mark);
                    l.out_str(&format!("; not lowered: {} ({})\n", f.name, l.errors.join("; ")));
                } else {
                    return Err(format!(
                        "native lowering failed for reachable function `{}`:\n  - {}",
                        f.name,
                        l.errors.join("\n  - ")
                    ));
                }
            }
        }
    }
    // Globals cannot appear in function bodies. Forward references are valid,
    // so flush all buffered literals after the lowered functions.
    for (data_name, boxed_name, value) in std::mem::take(&mut l.str_literals) {
        emit_string_global(&mut l.out, &data_name, &boxed_name, &value);
    }
    Ok(l.out)
}

fn type_descriptor(module: &Module, ty: &Type) -> Result<String, String> {
    fn build(module: &Module, ty: &Type, active: &mut HashSet<String>) -> Result<String, String> {
        Ok(match ty {
            Type::I64 => "i".into(),
            Type::Bool => "b".into(),
            Type::Str => "s".into(),
            Type::Unit => "u".into(),
            Type::Vec(inner) => format!("v{}", build(module, inner, active)?),
            Type::Result(ok, err) => {
                format!("r{}{}", build(module, ok, active)?, build(module, err, active)?)
            }
            Type::Struct(name) => {
                if !active.insert(name.clone()) {
                    return Err(format!(
                        "compile: recursive struct `{}` cannot be represented by a finite native entry type descriptor",
                        name
                    ));
                }
                let definition = module
                    .items
                    .iter()
                    .find_map(|item| match item {
                        ModuleItem::Struct(definition) if definition.name == *name => {
                            Some(definition)
                        }
                        _ => None,
                    })
                    .ok_or_else(|| format!("unknown struct `{}`", name))?;
                let mut result =
                    format!("t{}:{}{}:", name.len(), name, definition.fields.len());
                for (field, field_ty) in &definition.fields {
                    result.push_str(&format!(
                        "{}:{}{}",
                        field.len(),
                        field,
                        build(module, field_ty, active)?
                    ));
                }
                active.remove(name);
                result
            }
            Type::Ref { .. } | Type::Region => {
                return Err(format!("native CLI values of type {:?} are unsupported", ty));
            }
        })
    }

    build(module, ty, &mut HashSet::new())
}

fn main_fresh(counter: &mut usize) -> String {
    let result = format!("%m{}", *counter);
    *counter += 1;
    result
}

fn emit_main_value(module: &Module, value: &Value, ty: &Type, globals: &mut String, body: &mut String, counter: &mut usize) -> Result<String, String> {
    match (ty, value) {
        (Type::I64, Value::I64(number)) => Ok(number.to_string()),
        (Type::Bool, Value::Bool(boolean)) => Ok(if *boolean { "1" } else { "0" }.into()),
        (Type::Unit, Value::Unit) => Ok("0".into()),
        (Type::Str, Value::Str(string)) => {
            let id = *counter; *counter += 1;
            let data = format!("@.argd{}", id);
            let boxed = format!("@.arg{}", id);
            emit_string_global(globals, &data, &boxed, string);
            Ok(boxed)
        }
        (Type::Vec(inner), Value::Vec(values)) => {
            let vector = main_fresh(counter);
            body.push_str(&format!("  {} = call ptr @aury_vec_new(i64 {})\n", vector, values.len()));
            for (index, element) in values.iter().enumerate() {
                let operand = emit_main_value(module, element, inner, globals, body, counter)?;
                let bits = if llvm_type(inner) == "ptr" {
                    let bits = main_fresh(counter);
                    body.push_str(&format!("  {} = ptrtoint ptr {} to i64\n", bits, operand)); bits
                } else { operand };
                let slot = main_fresh(counter);
                body.push_str(&format!("  {} = call ptr @aury_vec_slot(ptr {}, i64 {})\n  store i64 {}, ptr {}\n", slot, vector, index, bits, slot));
            }
            Ok(vector)
        }
        (Type::Struct(name), Value::Struct(value_name, fields)) if name == value_name => {
            let definition = module.items.iter().find_map(|item| match item {
                ModuleItem::Struct(definition) if definition.name == *name => Some(definition), _ => None,
            }).ok_or_else(|| format!("unknown struct `{}`", name))?;
            let boxed = main_fresh(counter);
            body.push_str(&format!("  {} = call ptr @aury_box_new(i64 {})\n", boxed, definition.fields.len()));
            for (index, (field, field_ty)) in definition.fields.iter().enumerate() {
                let field_value = fields.iter().find(|(candidate, _)| candidate == field).map(|(_, value)| value)
                    .ok_or_else(|| format!("missing field `{}`", field))?;
                let operand = emit_main_value(module, field_value, field_ty, globals, body, counter)?;
                let bits = if llvm_type(field_ty) == "ptr" {
                    let bits = main_fresh(counter);
                    body.push_str(&format!("  {} = ptrtoint ptr {} to i64\n", bits, operand)); bits
                } else { operand };
                let slot = main_fresh(counter);
                body.push_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 {})\n  store i64 {}, ptr {}\n", slot, boxed, index, bits, slot));
            }
            Ok(boxed)
        }
        (Type::Result(ok_ty, err_ty), Value::ResultOk(payload))
        | (Type::Result(ok_ty, err_ty), Value::ResultErr(payload)) => {
            let is_ok = matches!(value, Value::ResultOk(_));
            let payload_ty = if is_ok { ok_ty.as_ref() } else { err_ty.as_ref() };
            let boxed = main_fresh(counter);
            body.push_str(&format!("  {} = call ptr @aury_box_new(i64 2)\n", boxed));
            let tag_slot = main_fresh(counter);
            body.push_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 0)\n  store i64 {}, ptr {}\n", tag_slot, boxed, if is_ok { 1 } else { 0 }, tag_slot));
            let operand = emit_main_value(module, payload, payload_ty, globals, body, counter)?;
            let bits = if llvm_type(payload_ty) == "ptr" {
                let bits = main_fresh(counter);
                body.push_str(&format!("  {} = ptrtoint ptr {} to i64\n", bits, operand)); bits
            } else { operand };
            let payload_slot = main_fresh(counter);
            body.push_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 1)\n  store i64 {}, ptr {}\n", payload_slot, boxed, bits, payload_slot));
            Ok(boxed)
        }
        _ => Err(format!("value {:?} does not match CLI type {:?}", value, ty)),
    }
}

/// Build a runnable native program: lower the reachable set from `entry_fn`,
/// add a C `main` that calls it with `args` and prints the result.
pub fn lower_program_with_main(module: &Module, entry_fn: &str, args: &[String]) -> Result<String, String> {
    let mut ir = lower_set(module, &reachable(module, entry_fn), false)?;
    let function = module.items.iter().find_map(|item| match item {
        ModuleItem::Fn(function) if function.name == entry_fn => Some(function),
        _ => None,
    }).ok_or_else(|| format!("entry fn `{}` not found", entry_fn))?;
    if function.params.len() != args.len() {
        return Err(format!("compile: entry fn `{}` takes {} args, got {}", entry_fn, function.params.len(), args.len()));
    }
    let mut globals = String::new();
    let mut body = String::new();
    let mut counter = 0;
    let mut arguments = Vec::new();
    for (parameter, text) in function.params.iter().zip(args) {
        let value = crate::value_io::parse_cli_value(module, &parameter.ty, text)
            .map_err(|error| format!("compile: arg for `{}`: {}", parameter.name, error))?;
        let operand = emit_main_value(module, &value, &parameter.ty, &mut globals, &mut body, &mut counter)?;
        arguments.push(format!("{} {}", llvm_type(&parameter.ty), operand));
    }
    let descriptor = type_descriptor(module, &function.ret)?;
    globals.push_str(&format!("@.return_type = private constant [{} x i8] c\"{}\"\n", descriptor.len() + 1, llvm_c_string(&descriptor)));
    ir.push_str(&globals);
    ir.push_str("define i32 @main() {\nentry:\n  call void @aury_rng_init(i64 12648430)\n");
    ir.push_str(&body);
    ir.push_str(&format!("  %r = call {} @aury__{}({})\n", llvm_type(&function.ret), entry_fn, arguments.join(", ")));
    if llvm_type(&function.ret) == "ptr" {
        ir.push_str("  %rbits = ptrtoint ptr %r to i64\n  call void @aury_value_print(i64 %rbits, ptr @.return_type)\n");
    } else {
        ir.push_str("  call void @aury_value_print(i64 %r, ptr @.return_type)\n");
    }
    ir.push_str("  ret i32 0\n}\n");
    Ok(ir)
}

impl Lowerer {
    fn out_str(&mut self, s: &str) {
        self.out.push_str(s);
    }
    fn fresh(&mut self) -> String {
        let r = format!("%t{}", self.reg);
        self.reg += 1;
        r
    }
    fn fresh_lbl(&mut self, prefix: &str) -> String {
        let l = format!("{}{}", prefix, self.lbl);
        self.lbl += 1;
        l
    }
    fn slot(&mut self, name: &str, ty: &str) -> String {
        let r = format!("%.{}.{}", name, self.reg);
        self.reg += 1;
        self.out_str(&format!("  {} = alloca {}\n", r, ty));
        r
    }
    fn err(&mut self, msg: &str) {
        self.errors.push(msg.to_string());
    }

    fn lower_fn(&mut self, f: &FnDef) {
        let retty = llvm_type(&f.ret);
        let params: Vec<String> = f
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| format!("{} %a{}_{}", llvm_type(&p.ty), i, f.name))
            .collect();
        self.out_str(&format!("define {} @aury__{}({}) {{\n", retty, f.name, params.join(", ")));
        self.out_str("entry:\n");
        let retslot = format!("%.ret.{}", f.name);
        self.retslot = retslot.clone();
        self.retty = retty.clone();
        self.out_str(&format!("  {} = alloca {}\n", retslot, retty));
        let n_before = self.scope.len();
        for (i, p) in f.params.iter().enumerate() {
            let pty = llvm_type(&p.ty);
            let s = self.slot(&p.name, &pty);
            self.out_str(&format!("  store {} %a{}_{}, ptr {}\n", pty, i, f.name, s));
            self.scope.push((p.name.clone(), s, p.ty.clone()));
        }
        let body_lbl = self.fresh_lbl("body");
        self.out_str(&format!("  br label %{}\n", body_lbl));
        self.out_str(&format!("{}:\n", body_lbl));
        let (v, vty, div) = self.lower_expr(&f.body);
        if !div {
            if let Some(val) = v {
                // store through the ret slot; type must match retty (validator guarantees)
                self.out_str(&format!("  store {} {}, ptr {}\n", vty, val, retslot));
            }
            self.out_str("  br label %exit\n");
        }
        self.out_str("exit:\n");
        let r = self.fresh();
        self.out_str(&format!("  {} = load {}, ptr {}\n", r, retty, retslot));
        self.out_str(&format!("  ret {} {}\n", retty, r));
        self.out_str("}\n\n");
        self.scope.truncate(n_before);
        self.retslot.clear();
        self.retty.clear();
    }

    /// Lower an expression. Returns (value, llvm_type, diverged).
    fn lower_expr(&mut self, e: &Expr) -> (Option<String>, String, bool) {
        match e {
            Expr::Lit { value, .. } => match value {
                Lit::I64(n) => (Some(n.to_string()), "i64".into(), false),
                Lit::Bool(b) => (Some((if *b { 1 } else { 0 }).to_string()), "i64".into(), false),
                Lit::Unit => (Some("0".into()), "i64".into(), false),
                Lit::Str(s) => (Some(self.str_literal(s)), "ptr".into(), false),
            },
            Expr::Ref { name, .. } => {
                let (slot, ty) = self.lookup(name);
                let lt = llvm_type(&ty);
                let r = self.fresh();
                self.out_str(&format!("  {} = load {}, ptr {}\n", r, lt, slot));
                (Some(r), lt, false)
            }
            Expr::Let { name, ty, init, body, .. } => {
                let (iv, ity, idiv) = self.lower_expr(init);
                if idiv {
                    return (None, String::new(), true);
                }
                let lty = llvm_type(ty);
                let slot = self.slot(name, &lty);
                self.out_str(&format!("  store {} {}, ptr {}\n", ity, iv.unwrap(), slot));
                self.scope.push((name.clone(), slot, ty.clone()));
                let res = self.lower_expr(body);
                self.scope.pop();
                res
            }
            Expr::Call { op, args, .. } => self.lower_call(op, args),
            Expr::If { cond, then, els, .. } => self.lower_if(cond, then, els),
            Expr::Match { scrut, arms, .. } => self.lower_match(scrut, arms),
            Expr::Loop { body, .. } => self.lower_loop(body),
            Expr::Return { value, .. } => {
                let (v, vty, div) = self.lower_expr(value);
                if div {
                    return (None, String::new(), true);
                }
                self.out_str(&format!("  store {} {}, ptr {}\n", vty, v.unwrap(), self.retslot));
                self.out_str("  br label %exit\n");
                (None, String::new(), true)
            }
            Expr::Block { stmts, tail, .. } => {
                for s in stmts {
                    let (_, _, div) = self.lower_expr(s);
                    if div {
                        return (None, String::new(), true);
                    }
                }
                self.lower_expr(tail)
            }
            // v0 region/copy semantics are explicit immutable-value no-ops.
            Expr::Region { body, .. } => self.lower_expr(body),
            Expr::Copy { value, .. } => self.lower_expr(value),
            Expr::Cast { target, value, .. } => self.lower_cast(target, value),
            Expr::VecNew { ty, elems, .. } => self.lower_vec_new(ty, elems),
            Expr::Index { target, index, .. } => self.lower_index(target, index),
            Expr::Len { target, .. } => self.lower_len(target),
            Expr::StructNew { name, fields, .. } => self.lower_struct_new(name, fields),
            Expr::Field { target, field, .. } => self.lower_field(target, field),
        }
    }

    /// Buffer a boxed string literal for module-level emission and return its
    /// address. LLVM permits the function body to reference the later global.
    fn str_literal(&mut self, value: &str) -> String {
        let n = self.str_n;
        self.str_n += 1;
        let data_name = format!("@.sd{}", n);
        let boxed_name = format!("@.s{}", n);
        self.str_literals
            .push((data_name, boxed_name.clone(), value.to_string()));
        boxed_name
    }

    fn value_to_bits(&mut self, value: String, llvm_ty: &str) -> String {
        if llvm_ty == "ptr" {
            let bits = self.fresh();
            self.out_str(&format!("  {} = ptrtoint ptr {} to i64\n", bits, value));
            bits
        } else { value }
    }

    fn bits_to_value(&mut self, bits: String, ty: &Type) -> (String, String) {
        let llvm_ty = llvm_type(ty);
        if llvm_ty == "ptr" {
            let value = self.fresh();
            self.out_str(&format!("  {} = inttoptr i64 {} to ptr\n", value, bits));
            (value, llvm_ty)
        } else { (bits, llvm_ty) }
    }

    fn lower_vec_new(&mut self, ty: &Type, elems: &[Expr]) -> (Option<String>, String, bool) {
        if !matches!(ty, Type::Vec(_)) {
            self.err("vec-new annotation is not a vector type");
        }
        let vector = self.fresh();
        self.out_str(&format!("  {} = call ptr @aury_vec_new(i64 {})\n", vector, elems.len()));
        for (index, elem) in elems.iter().enumerate() {
            let (value, llvm_ty, diverged) = self.lower_expr(elem);
            if diverged { return (None, String::new(), true); }
            let bits = self.value_to_bits(value.unwrap(), &llvm_ty);
            let slot = self.fresh();
            self.out_str(&format!("  {} = call ptr @aury_vec_slot(ptr {}, i64 {})\n", slot, vector, index));
            self.out_str(&format!("  store i64 {}, ptr {}\n", bits, slot));
        }
        (Some(vector), "ptr".into(), false)
    }

    fn lower_index(&mut self, target: &Expr, index: &Expr) -> (Option<String>, String, bool) {
        let element_ty = match self.infer_type(target) {
            Type::Vec(inner) => *inner,
            _ => { self.err("idx target is not a vector"); Type::Unit }
        };
        let (vector, _, vector_diverged) = self.lower_expr(target);
        if vector_diverged { return (None, String::new(), true); }
        let (index_value, _, index_diverged) = self.lower_expr(index);
        if index_diverged { return (None, String::new(), true); }
        let slot = self.fresh();
        self.out_str(&format!("  {} = call ptr @aury_vec_slot(ptr {}, i64 {})\n", slot, vector.unwrap(), index_value.unwrap()));
        let bits = self.fresh();
        self.out_str(&format!("  {} = load i64, ptr {}\n", bits, slot));
        let (value, llvm_ty) = self.bits_to_value(bits, &element_ty);
        (Some(value), llvm_ty, false)
    }

    fn lower_len(&mut self, target: &Expr) -> (Option<String>, String, bool) {
        let (vector, _, diverged) = self.lower_expr(target);
        if diverged { return (None, String::new(), true); }
        let len = self.fresh();
        self.out_str(&format!("  {} = load i64, ptr {}\n", len, vector.unwrap()));
        (Some(len), "i64".into(), false)
    }

    fn lower_struct_new(&mut self, name: &str, fields: &[(String, Expr)]) -> (Option<String>, String, bool) {
        let Some(definition) = self.structs.get(name).cloned() else {
            self.err(&format!("unknown struct `{}`", name));
            return (Some("null".into()), "ptr".into(), false);
        };
        let boxed = self.fresh();
        self.out_str(&format!("  {} = call ptr @aury_box_new(i64 {})\n", boxed, definition.fields.len()));
        // Evaluate source fields left-to-right, store by declared-field index.
        for (field, expression) in fields {
            let (value, llvm_ty, diverged) = self.lower_expr(expression);
            if diverged { return (None, String::new(), true); }
            let Some(index) = definition.fields.iter().position(|(candidate, _)| candidate == field) else {
                self.err(&format!("unknown field `{}` on `{}`", field, name));
                continue;
            };
            let bits = self.value_to_bits(value.unwrap(), &llvm_ty);
            let slot = self.fresh();
            self.out_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 {})\n", slot, boxed, index));
            self.out_str(&format!("  store i64 {}, ptr {}\n", bits, slot));
        }
        (Some(boxed), "ptr".into(), false)
    }

    fn lower_field(&mut self, target: &Expr, field: &str) -> (Option<String>, String, bool) {
        let Type::Struct(name) = self.infer_type(target) else {
            self.err("get target is not a struct");
            return (Some("0".into()), "i64".into(), false);
        };
        let Some(definition) = self.structs.get(&name).cloned() else {
            self.err(&format!("unknown struct `{}`", name));
            return (Some("0".into()), "i64".into(), false);
        };
        let Some((index, (_, field_ty))) = definition.fields.iter().enumerate().find(|(_, (candidate, _))| candidate == field) else {
            self.err(&format!("unknown field `{}` on `{}`", field, name));
            return (Some("0".into()), "i64".into(), false);
        };
        let field_ty = field_ty.clone();
        let (boxed, _, diverged) = self.lower_expr(target);
        if diverged { return (None, String::new(), true); }
        let slot = self.fresh();
        self.out_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 {})\n", slot, boxed.unwrap(), index));
        let bits = self.fresh();
        self.out_str(&format!("  {} = load i64, ptr {}\n", bits, slot));
        let (value, llvm_ty) = self.bits_to_value(bits, &field_ty);
        (Some(value), llvm_ty, false)
    }

    fn lower_call(&mut self, op: &str, args: &[Expr]) -> (Option<String>, String, bool) {
        if let Some(r) = self.lower_builtin(op, args) {
            return r;
        }
        if let Some(sig) = self.fns.get(op).cloned() {
            let mut argvals: Vec<(String, String)> = Vec::new(); // (type, value)
            for a in args {
                let (av, aty, div) = self.lower_expr(a);
                if div {
                    return (None, String::new(), true);
                }
                argvals.push((aty, av.unwrap()));
            }
            if sig.params.len() != argvals.len() {
                self.err(&format!("arity mismatch calling `{}`", op));
            }
            let rty = llvm_type(&sig.ret);
            let r = self.fresh();
            let typed: Vec<String> = argvals.iter().map(|(ty, v)| format!("{} {}", ty, v)).collect();
            self.out_str(&format!("  {} = call {} @aury__{}({})\n", r, rty, op, typed.join(", ")));
            (Some(r), rty, false)
        } else {
            self.err(&format!("unknown call `{}` in native lowering", op));
            (Some("0".into()), "i64".into(), false)
        }
    }

    fn lower_builtin(&mut self, op: &str, args: &[Expr]) -> Option<(Option<String>, String, bool)> {
        let is_binary_scalar = matches!(
            op,
            "i64.add"
                | "i64.sub"
                | "i64.mul"
                | "i64.div"
                | "i64.mod"
                | "i64.gt"
                | "i64.lt"
                | "i64.ge"
                | "i64.le"
                | "i64.eq"
                | "i64.neq"
                | "bool.and"
                | "bool.or"
                | "bool.eq"
        );
        // Scalar builtins evaluate left-to-right. A `return` in either operand
        // is a successfully lowered divergent call, not an unknown builtin.
        let binary = if is_binary_scalar {
            if args.len() != 2 {
                return None;
            }
            let (a, aty, da) = self.lower_expr(&args[0]);
            if da {
                return Some((None, String::new(), true));
            }
            let (b, bty, db) = self.lower_expr(&args[1]);
            if db {
                return Some((None, String::new(), true));
            }
            if aty != "i64" || bty != "i64" {
                self.err(&format!("`{}` needs scalar args", op));
            }
            Some((a.unwrap(), b.unwrap()))
        } else {
            None
        };
        match op {
            "i64.add" | "i64.sub" | "i64.mul" => {
                let (a, b) = binary.clone().unwrap();
                let r = self.fresh();
                let k = if op == "i64.add" { "add" } else if op == "i64.sub" { "sub" } else { "mul" };
                self.out_str(&format!("  {} = {} i64 {}, {}\n", r, k, a, b));
                Some((Some(r), "i64".into(), false))
            }
            "i64.div" | "i64.mod" => {
                let (a, b) = binary.clone().unwrap();
                let r = self.fresh();
                let helper = if op == "i64.div" { "aury_i64_div" } else { "aury_i64_mod" };
                self.out_str(&format!("  {} = call i64 @{}(i64 {}, i64 {})\n", r, helper, a, b));
                Some((Some(r), "i64".into(), false))
            }
            "i64.gt" | "i64.lt" | "i64.ge" | "i64.le" | "i64.eq" | "i64.neq" => {
                let (a, b) = binary.clone().unwrap();
                let pred = match op {
                    "i64.gt" => "sgt", "i64.lt" => "slt", "i64.ge" => "sge",
                    "i64.le" => "sle", "i64.eq" => "eq", _ => "ne",
                };
                let c = self.fresh();
                self.out_str(&format!("  {} = icmp {} i64 {}, {}\n", c, pred, a, b));
                let r = self.fresh();
                self.out_str(&format!("  {} = zext i1 {} to i64\n", r, c));
                Some((Some(r), "i64".into(), false))
            }
            "i64.neg" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "i64" { self.err("i64.neg needs i64"); }
                let r = self.fresh();
                self.out_str(&format!("  {} = sub i64 0, {}\n", r, a.unwrap()));
                Some((Some(r), "i64".into(), false))
            }
            "i64.abs" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "i64" { self.err("i64.abs needs i64"); }
                let value = a.unwrap();
                let negative = self.fresh();
                self.out_str(&format!("  {} = icmp slt i64 {}, 0\n", negative, value));
                let negated = self.fresh();
                self.out_str(&format!("  {} = sub i64 0, {}\n", negated, value));
                let r = self.fresh();
                self.out_str(&format!("  {} = select i1 {}, i64 {}, i64 {}\n", r, negative, negated, value));
                Some((Some(r), "i64".into(), false))
            }
            "bool.and" | "bool.or" => {
                let (a, b) = binary.clone().unwrap();
                let r = self.fresh();
                let k = if op == "bool.and" { "and" } else { "or" };
                self.out_str(&format!("  {} = {} i64 {}, {}\n", r, k, a, b));
                Some((Some(r), "i64".into(), false))
            }
            "bool.not" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "i64" { self.err("bool.not needs bool"); }
                let r = self.fresh();
                self.out_str(&format!("  {} = xor i64 {}, 1\n", r, a.unwrap()));
                Some((Some(r), "i64".into(), false))
            }
            "bool.eq" => {
                let (a, b) = binary.unwrap();
                let c = self.fresh();
                self.out_str(&format!("  {} = icmp eq i64 {}, {}\n", c, a, b));
                let r = self.fresh();
                self.out_str(&format!("  {} = zext i1 {} to i64\n", r, c));
                Some((Some(r), "i64".into(), false))
            }
            // ---- str builtins ----
            "str.concat" => {
                if args.len() != 2 { return None; }
                let (a, aty, da) = self.lower_expr(&args[0]);
                if da { return Some((None, String::new(), true)); }
                let (b, bty, db) = self.lower_expr(&args[1]);
                if db { return Some((None, String::new(), true)); }
                if aty != "ptr" || bty != "ptr" { self.err("str.concat needs str"); }
                let r = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_str_concat(ptr {}, ptr {})\n", r, a.unwrap(), b.unwrap()));
                Some((Some(r), "ptr".into(), false))
            }
            "str.eq" => {
                if args.len() != 2 { return None; }
                let (a, _, da) = self.lower_expr(&args[0]);
                if da { return Some((None, String::new(), true)); }
                let (b, _, db) = self.lower_expr(&args[1]);
                if db { return Some((None, String::new(), true)); }
                let r = self.fresh();
                self.out_str(&format!("  {} = call i64 @aury_str_eq(ptr {}, ptr {})\n", r, a.unwrap(), b.unwrap()));
                Some((Some(r), "i64".into(), false))
            }
            "str.neq" => {
                if args.len() != 2 { return None; }
                let (a, _, da) = self.lower_expr(&args[0]);
                if da { return Some((None, String::new(), true)); }
                let (b, _, db) = self.lower_expr(&args[1]);
                if db { return Some((None, String::new(), true)); }
                let e = self.fresh();
                self.out_str(&format!("  {} = call i64 @aury_str_eq(ptr {}, ptr {})\n", e, a.unwrap(), b.unwrap()));
                let r = self.fresh();
                self.out_str(&format!("  {} = xor i64 {}, 1\n", r, e));
                Some((Some(r), "i64".into(), false))
            }
            "str.len" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "ptr" { self.err("str.len needs str"); }
                let r = self.fresh();
                // str layout: { i64 len, ptr data } — load field 0.
                self.out_str(&format!("  {} = load i64, ptr {}\n", r, a.unwrap()));
                Some((Some(r), "i64".into(), false))
            }
            "i64.to_str" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "i64" { self.err("i64.to_str needs i64"); }
                let r = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_i64_to_str(i64 {})\n", r, a.unwrap()));
                Some((Some(r), "ptr".into(), false))
            }
            "i64.parse" | "i64.from_str" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "ptr" { self.err("i64.parse needs str"); }
                let r = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_i64_parse(ptr {})\n", r, a.unwrap()));
                Some((Some(r), "ptr".into(), false))
            }
            "result.is_ok" => {
                if args.len() != 1 { return None; }
                let (a, aty, d) = self.lower_expr(&args[0]);
                if d { return Some((None, String::new(), true)); }
                if aty != "ptr" { self.err("result.is_ok needs a result"); }
                let tag = self.fresh();
                self.out_str(&format!("  {} = load i64, ptr {}\n", tag, a.unwrap()));
                let condition = self.fresh();
                self.out_str(&format!("  {} = icmp ne i64 {}, 0\n", condition, tag));
                let r = self.fresh();
                self.out_str(&format!("  {} = zext i1 {} to i64\n", r, condition));
                Some((Some(r), "i64".into(), false))
            }
            "rng.next" | "rng.i64" => {
                if !args.is_empty() { return None; }
                let r = self.fresh();
                self.out_str(&format!("  {} = call i64 @aury_rng_next()\n", r));
                Some((Some(r), "i64".into(), false))
            }
            _ => None,
        }
    }

    /// cast: i64<->i64 identity; i64->str; str->i64 (parse, trap on failure);
    /// str->str identity.
    fn lower_cast(&mut self, target: &Type, value: &Expr) -> (Option<String>, String, bool) {
        let (v, vty, div) = self.lower_expr(value);
        if div {
            return (None, String::new(), true);
        }
        let v = v.unwrap();
        let tty = llvm_type(target);
        match (vty.as_str(), tty.as_str()) {
            ("i64", "i64") => (Some(v), "i64".into(), false),
            ("ptr", "ptr") => (Some(v), "ptr".into(), false),
            ("i64", "ptr") => {
                // i64 -> str
                let r = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_i64_to_str(i64 {})\n", r, v));
                (Some(r), "ptr".into(), false)
            }
            ("ptr", "i64") => {
                // str -> i64: parse to generic {tag,payload} slots.
                let res = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_i64_parse_strict(ptr {})\n", res, v));
                let tag = self.fresh();
                self.out_str(&format!("  {} = load i64, ptr {}\n", tag, res));
                let ok = self.fresh();
                self.out_str(&format!("  {} = icmp ne i64 {}, 0\n", ok, tag));
                let good = self.fresh_lbl("castok");
                let bad = self.fresh_lbl("castbad");
                self.out_str(&format!("  br i1 {}, label %{}, label %{}\n", ok, good, bad));
                self.out_str(&format!("{}:\n  call void @llvm.trap()\n  unreachable\n", bad));
                self.out_str(&format!("{}:\n", good));
                let vp = self.fresh();
                self.out_str(&format!("  {} = call ptr @aury_box_slot(ptr {}, i64 1)\n", vp, res));
                let r = self.fresh();
                self.out_str(&format!("  {} = load i64, ptr {}\n", r, vp));
                (Some(r), "i64".into(), false)
            }
            _ => {
                self.err(&format!("cast {}->{} not supported natively", vty, tty));
                (Some("0".into()), "i64".into(), false)
            }
        }
    }

    fn lower_if(&mut self, cond: &Expr, then: &Expr, els: &Expr) -> (Option<String>, String, bool) {
        let (cv, cty, cdiv) = self.lower_expr(cond);
        if cdiv {
            return (None, String::new(), true);
        }
        if cty != "i64" {
            self.err("if condition must be bool");
        }
        let cz = self.fresh();
        self.out_str(&format!("  {} = icmp ne i64 {}, 0\n", cz, cv.unwrap()));
        let res = format!("%.if.{}", self.reg);
        self.reg += 1;
        // A diverging branch has no result type; size the slot from whichever
        // branch can reach the continuation.
        let result_ty = if Self::expr_diverges(then) {
            self.infer_type(els)
        } else {
            self.infer_type(then)
        };
        let rty = llvm_type(&result_ty);
        self.out_str(&format!("  {} = alloca {}\n", res, rty));
        let then_lbl = self.fresh_lbl("then");
        let else_lbl = self.fresh_lbl("else");
        let cont_lbl = self.fresh_lbl("cont");
        self.out_str(&format!("  br i1 {}, label %{}, label %{}\n", cz, then_lbl, else_lbl));
        self.out_str(&format!("{}:\n", then_lbl));
        let (tv, tty, tdiv) = self.lower_expr(then);
        if !tdiv {
            self.out_str(&format!("  store {} {}, ptr {}\n", tty, tv.unwrap(), res));
            self.out_str(&format!("  br label %{}\n", cont_lbl));
        }
        self.out_str(&format!("{}:\n", else_lbl));
        let (ev, ety, ediv) = self.lower_expr(els);
        if !ediv {
            self.out_str(&format!("  store {} {}, ptr {}\n", ety, ev.unwrap(), res));
            self.out_str(&format!("  br label %{}\n", cont_lbl));
        }
        if tdiv && ediv {
            return (None, String::new(), true);
        }
        self.out_str(&format!("{}:\n", cont_lbl));
        let r = self.fresh();
        self.out_str(&format!("  {} = load {}, ptr {}\n", r, rty, res));
        (Some(r), rty, false)
    }

    fn lower_match(&mut self, scrut: &Expr, arms: &[MatchArm]) -> (Option<String>, String, bool) {
        if arms.is_empty() {
            self.err("empty match is not supported in native lowering");
            return (Some("0".into()), "i64".into(), false);
        }
        let scrut_ty = self.infer_type(scrut);
        let (sv, sty, sdiv) = self.lower_expr(scrut);
        if sdiv {
            return (None, String::new(), true);
        }
        let sv = sv.unwrap();
        let result_ty = self.infer_match_type(arms, &scrut_ty);
        let rty = llvm_type(&result_ty);
        let res = format!("%.match.{}", self.reg);
        self.reg += 1;
        self.out_str(&format!("  {} = alloca {}\n", res, rty));
        let cont_lbl = self.fresh_lbl("cont");
        let mut any_nondiv = false;
        let mut has_fallthrough = true;
        let mut next_lbl = self.fresh_lbl("arm");
        self.out_str(&format!("  br label %{}\n", next_lbl));

        for arm in arms {
            if !has_fallthrough {
                break;
            }
            self.out_str(&format!("{}:\n", next_lbl));
            match &arm.pattern {
                Pattern::Wild | Pattern::Bind(_) => {
                    let bound = if let Pattern::Bind(name) = &arm.pattern {
                        let slot = self.slot(name, &sty);
                        self.out_str(&format!("  store {} {}, ptr {}\n", sty, sv, slot));
                        self.scope.push((name.clone(), slot, scrut_ty.clone()));
                        true
                    } else {
                        false
                    };
                    let (value, value_ty, diverged) = self.lower_expr(&arm.body);
                    if bound {
                        self.scope.pop();
                    }
                    if !diverged {
                        self.out_str(&format!(
                            "  store {} {}, ptr {}\n  br label %{}\n",
                            value_ty,
                            value.unwrap(),
                            res,
                            cont_lbl
                        ));
                        any_nondiv = true;
                    }
                    has_fallthrough = false;
                }
                Pattern::Lit(lit) => {
                    let cond = match lit {
                        Lit::I64(value) => {
                            if sty != "i64" {
                                self.err("i64 match literal used with non-i64 scrutinee");
                            }
                            let cond = self.fresh();
                            self.out_str(&format!("  {} = icmp eq i64 {}, {}\n", cond, sv, value));
                            cond
                        }
                        Lit::Bool(value) => {
                            if sty != "i64" {
                                self.err("bool match literal used with non-bool scrutinee");
                            }
                            let cond = self.fresh();
                            self.out_str(&format!(
                                "  {} = icmp eq i64 {}, {}\n",
                                cond,
                                sv,
                                if *value { 1 } else { 0 }
                            ));
                            cond
                        }
                        Lit::Str(value) => {
                            if sty != "ptr" {
                                self.err("str match literal used with non-str scrutinee");
                            }
                            let literal = self.str_literal(value);
                            let equal = self.fresh();
                            self.out_str(&format!(
                                "  {} = call i64 @aury_str_eq(ptr {}, ptr {})\n",
                                equal, sv, literal
                            ));
                            let cond = self.fresh();
                            self.out_str(&format!("  {} = icmp ne i64 {}, 0\n", cond, equal));
                            cond
                        }
                        Lit::Unit => {
                            if sty != "i64" {
                                self.err("unit match literal used with non-unit scrutinee");
                            }
                            let cond = self.fresh();
                            self.out_str(&format!("  {} = icmp eq i64 {}, 0\n", cond, sv));
                            cond
                        }
                    };
                    let this_arm = self.fresh_lbl("arm");
                    let following = self.fresh_lbl("arm");
                    self.out_str(&format!(
                        "  br i1 {}, label %{}, label %{}\n{}:\n",
                        cond, this_arm, following, this_arm
                    ));
                    let (value, value_ty, diverged) = self.lower_expr(&arm.body);
                    if !diverged {
                        self.out_str(&format!(
                            "  store {} {}, ptr {}\n  br label %{}\n",
                            value_ty,
                            value.unwrap(),
                            res,
                            cont_lbl
                        ));
                        any_nondiv = true;
                    }
                    next_lbl = following;
                }
            }
        }

        if has_fallthrough {
            self.out_str(&format!(
                "{}:\n  call void @llvm.trap()\n  unreachable\n",
                next_lbl
            ));
        }
        if !any_nondiv {
            return (None, String::new(), true);
        }
        self.out_str(&format!("{}:\n", cont_lbl));
        let value = self.fresh();
        self.out_str(&format!("  {} = load {}, ptr {}\n", value, rty, res));
        (Some(value), rty, false)
    }

    fn lower_loop(&mut self, body: &Expr) -> (Option<String>, String, bool) {
        let loop_lbl = self.fresh_lbl("loop");
        self.out_str(&format!("  br label %{}\n", loop_lbl));
        self.out_str(&format!("{}:\n", loop_lbl));
        let (_, _, div) = self.lower_expr(body);
        if div {
            return (None, String::new(), true);
        }
        self.out_str(&format!("  br label %{}\n", loop_lbl));
        (None, String::new(), true)
    }

    fn lookup(&mut self, name: &str) -> (String, Type) {
        for (n, s, ty) in self.scope.iter().rev() {
            if n == name {
                return (s.clone(), ty.clone());
            }
        }
        self.err(&format!("unbound ref `{}` in native lowering", name));
        ("%.bad".to_string(), Type::I64)
    }
}

impl Lowerer {
    /// Infer the Aury type of an expression (mirrors the validator's rules for
    /// the supported subset) so if/match result slots get the right LLVM type.
    /// Uses a local env threaded through `let` bindings.
    fn infer_type(&self, e: &Expr) -> Type {
        let env: Vec<(String, Type)> = self.scope.iter().map(|(n, _, t)| (n.clone(), t.clone())).collect();
        self.infer_env(e, &env)
    }
    fn infer_env(&self, e: &Expr, env: &[(String, Type)]) -> Type {
        match e {
            Expr::Lit { value, .. } => match value {
                Lit::I64(_) => Type::I64,
                Lit::Bool(_) => Type::Bool,
                Lit::Str(_) => Type::Str,
                Lit::Unit => Type::Unit,
            },
            Expr::Ref { name, .. } => env
                .iter().rev()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .unwrap_or(Type::I64),
            Expr::Let { name, ty, body, .. } => {
                let mut e2: Vec<(String, Type)> = env.to_vec();
                e2.push((name.clone(), ty.clone()));
                self.infer_env(body, &e2)
            }
            Expr::Call { op, .. } => self.builtin_ret(op),
            Expr::If { then, els, .. } => {
                if Self::expr_diverges(then) {
                    self.infer_env(els, env)
                } else {
                    self.infer_env(then, env)
                }
            }
            Expr::Match { scrut, arms, .. } => {
                let scrut_ty = self.infer_env(scrut, env);
                arms.iter()
                    .find(|arm| !Self::expr_diverges(&arm.body))
                    .map(|arm| {
                        let mut arm_env = env.to_vec();
                        if let Pattern::Bind(name) = &arm.pattern {
                            arm_env.push((name.clone(), scrut_ty.clone()));
                        }
                        self.infer_env(&arm.body, &arm_env)
                    })
                    .unwrap_or(Type::Unit)
            }
            Expr::Loop { .. } | Expr::Return { .. } => Type::Unit,
            Expr::Block { tail, .. } => self.infer_env(tail, env),
            Expr::Region { body, .. } => self.infer_env(body, env),
            Expr::Copy { value, .. } => self.infer_env(value, env),
            Expr::Cast { target, .. } => target.clone(),
            Expr::VecNew { ty, .. } => ty.clone(),
            Expr::Index { target, .. } => match self.infer_env(target, env) {
                Type::Vec(t) => *t,
                _ => Type::Unit,
            },
            Expr::Len { .. } => Type::I64,
            Expr::StructNew { name, .. } => Type::Struct(name.clone()),
            Expr::Field { target, field, .. } => match self.infer_env(target, env) {
                Type::Struct(name) => self.structs.get(&name)
                    .and_then(|definition| definition.fields.iter().find(|(candidate, _)| candidate == field))
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or(Type::Unit),
                _ => Type::Unit,
            },
        }
    }
    fn infer_match_type(&self, arms: &[MatchArm], scrut_ty: &Type) -> Type {
        let base_env: Vec<(String, Type)> = self
            .scope
            .iter()
            .map(|(name, _, ty)| (name.clone(), ty.clone()))
            .collect();
        arms.iter()
            .find(|arm| !Self::expr_diverges(&arm.body))
            .map(|arm| {
                let mut env = base_env;
                if let Pattern::Bind(name) = &arm.pattern {
                    env.push((name.clone(), scrut_ty.clone()));
                }
                self.infer_env(&arm.body, &env)
            })
            .unwrap_or(Type::Unit)
    }

    /// Match the validator's divergence rule when choosing a control-flow
    /// expression's normal result type.
    fn expr_diverges(expr: &Expr) -> bool {
        match expr {
            Expr::Return { .. } | Expr::Loop { .. } => true,
            Expr::Let { init, body, .. } => {
                Self::expr_diverges(init) || Self::expr_diverges(body)
            }
            Expr::Call { args, .. } => args.iter().any(Self::expr_diverges),
            Expr::If { cond, then, els, .. } => {
                Self::expr_diverges(cond)
                    || (Self::expr_diverges(then) && Self::expr_diverges(els))
            }
            Expr::Match { scrut, arms, .. } => {
                Self::expr_diverges(scrut)
                    || (!arms.is_empty()
                        && arms.iter().all(|arm| Self::expr_diverges(&arm.body)))
            }
            Expr::Block { stmts, tail, .. } => {
                stmts.iter().any(Self::expr_diverges) || Self::expr_diverges(tail)
            }
            Expr::Region { body, .. } => Self::expr_diverges(body),
            Expr::Copy { value, .. } | Expr::Cast { value, .. } => {
                Self::expr_diverges(value)
            }
            Expr::VecNew { elems, .. } => elems.iter().any(Self::expr_diverges),
            Expr::Index { target, index, .. } => {
                Self::expr_diverges(target) || Self::expr_diverges(index)
            }
            Expr::Len { target, .. } | Expr::Field { target, .. } => {
                Self::expr_diverges(target)
            }
            Expr::StructNew { fields, .. } => {
                fields.iter().any(|(_, value)| Self::expr_diverges(value))
            }
            Expr::Lit { .. } | Expr::Ref { .. } => false,
        }
    }

    /// Return type of a builtin op (mirrors the validator's builtin table).
    fn builtin_ret(&self, op: &str) -> Type {
        match op {
            "i64.add" | "i64.sub" | "i64.mul" | "i64.div" | "i64.mod" | "i64.neg" | "i64.abs" => Type::I64,
            "i64.gt" | "i64.lt" | "i64.ge" | "i64.le" | "i64.eq" | "i64.neq" => Type::Bool,
            "i64.to_str" | "str.concat" => Type::Str,
            "i64.parse" | "i64.from_str" => Type::Result(Box::new(Type::I64), Box::new(Type::Str)),
            "bool.and" | "bool.or" | "bool.not" | "bool.eq" => Type::Bool,
            "str.eq" | "str.neq" => Type::Bool,
            "str.len" => Type::I64,
            "result.is_ok" => Type::Bool,
            _ => self.fns.get(op).map(|s| s.ret.clone()).unwrap_or(Type::I64),
        }
    }
}
