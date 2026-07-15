//! The MLIR dialect *sketch* — a structural preview of the proposal's
//! Aury → MLIR → LLVM pipeline. This is NOT executable MLIR; it shows the
//! shape of each dialect pass for auditing. The *real* native backend is in
//! [`crate::lower`] (Aury → LLVM IR text → native executable via clang).

use crate::ast::*;

pub fn lower_to_mlir_sketch(module: &Module) -> String {
    let mut out = String::new();
    out.push_str("// Aury → MLIR lowering sketch (structural preview, not executable)\n");
    out.push_str(&format!("// module: {}\n\n", module.name));
    out.push_str("module {\n");
    for item in &module.items {
        match item {
            ModuleItem::Fn(f) => {
                out.push_str(&format!(
                    "  aury.func @{}({}) -> {:?}\n",
                    f.name,
                    f.params
                        .iter()
                        .map(|p| format!("{}: {:?}", p.name, p.ty))
                        .collect::<Vec<_>>()
                        .join(", "),
                    f.ret
                ));
                out.push_str("    // → scf (if/match/loop) → mem (allocas/borrows) → arith+llvm → LLVM IR\n");
                sketch_expr(&f.body, &mut out, 2);
            }
            ModuleItem::Struct(s) => out.push_str(&format!(
                "  aury.struct @{} {{ {} }}\n",
                s.name,
                s.fields
                    .iter()
                    .map(|(n, t)| format!("{}: {:?}", n, t))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
            ModuleItem::Spec(_) => out.push_str("  // spec: verified at the aury dialect level\n"),
            ModuleItem::Extern(e) => out.push_str(&format!(
                "  aury.extern @{} -> {:?}\n",
                e.name, e.ret
            )),
        }
    }
    out.push_str("}\n");
    out
}

fn sketch_expr(e: &Expr, out: &mut String, indent: usize) {
    let pad = " ".repeat(indent);
    match e {
        Expr::Lit { value, .. } => out.push_str(&format!("{}// arith: lit {:?}\n", pad, value)),
        Expr::Ref { name, .. } => out.push_str(&format!("{}// llvm: load %{}\n", pad, name)),
        Expr::Call { op, args, .. } => {
            out.push_str(&format!("{}// arith/llvm: call @{} ({} args)\n", pad, op, args.len()));
            for a in args {
                sketch_expr(a, out, indent + 2);
            }
        }
        Expr::If { cond, then, els, .. } => {
            out.push_str(&format!("{}// scf.if\n", pad));
            sketch_expr(cond, out, indent + 2);
            sketch_expr(then, out, indent + 2);
            sketch_expr(els, out, indent + 2);
        }
        Expr::Loop { body, .. } => {
            out.push_str(&format!("{}// scf.while\n", pad));
            sketch_expr(body, out, indent + 2);
        }
        Expr::Return { value, .. } => {
            out.push_str(&format!("{}// scf.yield (return)\n", pad));
            sketch_expr(value, out, indent + 2);
        }
        Expr::Let { init, body, .. } => {
            sketch_expr(init, out, indent);
            sketch_expr(body, out, indent);
        }
        Expr::Match { scrut, arms, .. } => {
            out.push_str(&format!("{}// scf.match\n", pad));
            sketch_expr(scrut, out, indent + 2);
            for a in arms {
                sketch_expr(&a.body, out, indent + 2);
            }
        }
        _ => out.push_str(&format!("{}// (expr)\n", pad)),
    }
}