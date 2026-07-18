//! Aury v0 — a small, strongly-typed, s-expression IR co-designed around an
//! LLM repair loop, with an intent-verification gate.
//!
//! See [`aury-proposal.md`] for the design and [`crate::repair_loop`] for
//! the closed generate→validate→repair→accept loop.
//!
//! Some fields and methods are reserved for v1 (capabilities, shared
//! regions) and intentionally unused in v0.
#![allow(dead_code)]
//!
//! # Layout
//! - [`sexpr`]: canonical s-expression reader (one-screen, unambiguous).
//! - [`id`]: content-addressed Merkle node ids (SHA-256).
//! - [`ast`]: typed AST + conversion from s-exprs, assigning ids.
//! - [`types`]: explicit types + effect rows (no inference).
//! - [`repair`]: the repair protocol — rejections with ranked admissible patches.
//! - [`validate`]: the type/effect/region checker, emitting rejections.
//! - [`spec`]: contracts, property tests, vacuity check, shrinking.
//! - [`interp`]: tree-walking interpreter (v0 execution backend).
//! - [`lower`]: type-aware LLVM IR lowering for the native-supported subset.
//! - [`lower_sketch`]: structural MLIR preview retained for `aury lower`.
//! - [`loop_driver`]: the closed repair loop.
//! - [`eval`]: the evaluation harness — repair convergence over a task corpus.

pub mod ast;
pub mod diagram;
pub mod eval;
pub mod id;
pub mod interp;
pub mod json;
pub mod lower;
pub mod lower_sketch;
pub mod loop_driver;
pub mod repair;
pub mod sexpr;
pub mod spec;
pub mod types;
pub mod validate;
pub mod value_io;

pub use loop_driver::{repair_loop, LoopResult};
