//! The repair protocol — the centerpiece of Aury.
//!
//! Every rejection from any gate (type, effect, region, contract, property
//! test) is emitted as a [`Rejection`] carrying a ranked list of admissible
//! [`Repair`] patches. A repair is *admissible by construction*: the
//! validator only proposes replacements it has already checked are locally
//! valid. The model picks from a menu of known-valid fixes — it cannot pick
//! an invalid one. It can only refuse all of them, in which case it should
//! regenerate, not loop.

use crate::id::NodeId;
use crate::sexpr::Sexpr;
use std::collections::HashMap;

/// Which gate produced the rejection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gate {
    Type,
    Effect,
    Region,
    Contract,
    PropertyTest,
}

impl Gate {
    pub fn as_str(&self) -> &'static str {
        match self {
            Gate::Type => "type",
            Gate::Effect => "effect",
            Gate::Region => "region",
            Gate::Contract => "contract",
            Gate::PropertyTest => "property-test",
        }
    }
}

/// One admissible repair.
#[derive(Clone, Debug)]
pub struct Repair {
    pub id: String,
    /// A short action tag, e.g. "wrap", "replace_node", "change_param_type".
    pub action: String,
    /// Serialized s-expression replacement (if the repair replaces a node).
    pub with: Option<Sexpr>,
    /// Cost / ranking weight — lower is preferred.
    pub cost: u32,
    /// Whether applying this repair preserves the function's declared effect
    /// row.
    pub preserves_effects: bool,
    /// Whether applying this repair preserves existing contracts.
    pub preserves_contracts: bool,
    /// Other nodes that may need updating if this repair is applied
    /// (propagation), named by their short id + a path.
    pub propagates: Vec<String>,
    /// Human-readable note for the model.
    pub note: String,
}

impl Repair {
    pub fn rank_by_cost(repairs: &mut [Repair]) {
        repairs.sort_by_key(|r| r.cost);
    }
}

/// A structured rejection.
#[derive(Clone, Debug)]
pub struct Rejection {
    pub gate: Gate,
    pub kind: String,
    pub node: NodeId,
    pub path: String,
    pub expected: String,
    pub received: String,
    pub context: HashMap<String, String>,
    pub repairs: Vec<Repair>,
}

impl Rejection {
    /// Render as JSON (the on-the-wire shape the model consumes).
    pub fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\n");
        s.push_str(&format!("  \"gate\": \"{}\",\n", self.gate.as_str()));
        s.push_str(&format!("  \"kind\": \"{}\",\n", json_escape(&self.kind)));
        s.push_str(&format!("  \"node\": \"{}\",\n", self.node));
        s.push_str(&format!("  \"path\": \"{}\",\n", json_escape(&self.path)));
        s.push_str(&format!("  \"expected\": \"{}\",\n", json_escape(&self.expected)));
        s.push_str(&format!("  \"received\": \"{}\",\n", json_escape(&self.received)));
        s.push_str("  \"context\": {");
        if self.context.is_empty() {
            s.push_str("},\n");
        } else {
            s.push('\n');
            for (k, v) in &self.context {
                s.push_str(&format!(
                    "    \"{}\": \"{}\",\n",
                    json_escape(k),
                    json_escape(v)
                ));
            }
            s.truncate(s.len() - 2);
            s.push_str("\n  },\n");
        }
        s.push_str("  \"repairs\": [");
        if self.repairs.is_empty() {
            s.push_str("]\n");
        } else {
            s.push('\n');
            for r in &self.repairs {
                s.push_str("    {\n");
                s.push_str(&format!("      \"id\": \"{}\",\n", json_escape(&r.id)));
                s.push_str(&format!("      \"action\": \"{}\",\n", json_escape(&r.action)));
                if let Some(w) = &r.with {
                    s.push_str(&format!(
                        "      \"with\": {:?},\n",
                        format!("{:?}", w)
                    ));
                } else {
                    s.push_str("      \"with\": null,\n");
                }
                s.push_str(&format!("      \"cost\": {},\n", r.cost));
                s.push_str(&format!(
                    "      \"preserves_effects\": {},\n",
                    r.preserves_effects
                ));
                s.push_str(&format!(
                    "      \"preserves_contracts\": {},\n",
                    r.preserves_contracts
                ));
                s.push_str("      \"propagates\": [");
                if r.propagates.is_empty() {
                    s.push_str("],\n");
                } else {
                    s.push_str("\n");
                    for p in &r.propagates {
                        s.push_str(&format!("        \"{}\",\n", json_escape(p)));
                    }
                    s.truncate(s.len() - 2);
                    s.push_str("\n      ],\n");
                }
                s.push_str(&format!(
                    "      \"note\": \"{}\"\n",
                    json_escape(&r.note)
                ));
                s.push_str("    },\n");
            }
            s.truncate(s.len() - 2);
            s.push_str("\n  ]\n");
        }
        s.push_str("}");
        s
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Result of validating a module. Either it's accepted (no rejections), or it
/// carries the list of structured rejections to feed back to the model.
#[derive(Clone, Debug)]
pub enum ValidationOutcome {
    Accepted,
    Rejected(Vec<Rejection>),
}

impl ValidationOutcome {
    pub fn is_accepted(&self) -> bool {
        matches!(self, ValidationOutcome::Accepted)
    }
    pub fn rejections(&self) -> &[Rejection] {
        match self {
            ValidationOutcome::Accepted => &[],
            ValidationOutcome::Rejected(r) => r,
        }
    }
}