import { loadCalculator } from "./calculator.js";
import "./styles.css";

const group = new Intl.NumberFormat("en-US").format;

// A mode is either "int" (i64/bool arithmetic, BigInt entry) or "float" (f64
// arithmetic, decimal/Number entry). The mode decides which Aury exports the
// keypad and chips invoke, how the entry is parsed and formatted, and which
// binary-op symbol set is used.

const SYMBOL = {
  int: {
    add: "+", subtract: "−", multiply: "×", divide: "÷", modulo: "mod",
    power: "^", gcd: "gcd", lcm: "lcm", maximum: "max", minimum: "min",
    average: "avg", percent: "% of",
  },
  float: {
    fadd: "+", fsubtract: "−", fmultiply: "×", fdivide: "÷",
    fpower: "^", fmaximum: "max", fminimum: "min",
  },
};

const el = {
  entry: document.querySelector("#entry"),
  expr: document.querySelector("#expr"),
  badge: document.querySelector("#badge"),
  status: document.querySelector("#status"),
  mode: document.querySelector("#mode"),
  keys: document.querySelector("#keys"),
  functions: document.querySelector("#functions"),
};

const state = {
  mode: "int",       // "int" | "float"
  entry: "0",        // string the user is typing
  acc: null,         // first operand (BigInt in int mode, Number in float mode)
  op: null,          // pending binary op name (within the current mode)
  fresh: true,       // next digit starts a new entry
  ready: false,      // wasm loaded?
};

let calc = null; // { fns, meta }

// ---- value parsing / formatting ------------------------------------------

function currentValue() {
  if (state.mode === "int") return BigInt(state.entry || "0");
  const n = Number(state.entry);
  return Number.isNaN(n) ? 0 : n; // "." or "-" alone reads as 0
}

function formatFloat(n) {
  if (typeof n !== "number") return String(n);
  if (Number.isNaN(n)) return "NaN";
  if (!Number.isFinite(n)) return n > 0 ? "∞" : "-∞";
  if (Number.isInteger(n)) return n.toString();
  return String(Number(n.toPrecision(12))); // trim to 12 sig digits, drop trailing 0s
}

function fmtValue(v) {
  return state.mode === "int" ? group(v) : formatFloat(v);
}

function render() {
  el.entry.textContent = state.mode === "int"
    ? group(currentValue())
    : (state.entry || "0");
  if (state.op !== null && state.acc !== null) {
    el.expr.textContent = `${fmtValue(state.acc)} ${SYMBOL[state.mode][state.op]}`;
  } else {
    el.expr.textContent = "";
  }
}

function setEntry(value) {
  state.entry = state.mode === "int" ? value.toString() : formatFloat(value);
  state.fresh = true;
}

// ---- badges --------------------------------------------------------------

function showBadge(text, tone = "info") {
  el.badge.textContent = text;
  el.badge.dataset.tone = tone;
  el.badge.hidden = false;
}
function clearBadge() { el.badge.hidden = true; }
function reportError(err) { showBadge(err instanceof Error ? err.message : String(err), "no"); }

// ---- input actions -------------------------------------------------------

function inputDigit(d) {
  clearBadge();
  if (state.fresh || state.entry === "0") {
    state.entry = d;
    state.fresh = false;
  } else if (state.entry === "-0") {
    state.entry = "-" + d;
  } else {
    state.entry += d;
  }
  render();
}

function inputDot() {
  clearBadge();
  if (state.fresh) { state.entry = "0."; state.fresh = false; return; }
  if (!state.entry.includes(".")) state.entry += ".";
  render();
}

function inputBinary(name) {
  clearBadge();
  if (state.op !== null && !state.fresh) equals(); // chain: 2 + 3 + -> evaluates 2+3 first
  state.acc = currentValue();
  state.op = name;
  state.fresh = true;
  render();
}

function equals() {
  if (state.op === null || state.acc === null) return;
  try {
    const result = calc.fns[state.op](state.acc, currentValue());
    setEntry(result);
  } catch (err) {
    reportError(err);
    return;
  }
  state.acc = null;
  state.op = null;
  render();
}

function applyUnary(name) {
  clearBadge();
  try { setEntry(calc.fns[name](currentValue())); }
  catch (err) { reportError(err); return; }
  render();
}

function applyPredicate(name) {
  try {
    const yes = calc.fns[name](currentValue());
    const label = predicateLabel(name, yes);
    showBadge(`${fmtValue(currentValue())} ${label}`, yes ? "yes" : "no");
  } catch (err) { reportError(err); }
}

function predicateLabel(name, yes) {
  if (name === "is_even") return yes ? "is even" : "is odd";
  if (name === "is_prime") return yes ? "is prime" : "is not prime";
  if (name === "is_nan") return yes ? "is NaN" : "is finite";
  return yes ? "yes" : "no";
}

function toggleSign() {
  clearBadge();
  const v = currentValue();
  setEntry(state.mode === "int" ? -v : -v);
  state.fresh = false;
  render();
}

function backspace() {
  clearBadge();
  if (state.fresh) return;
  const s = state.entry;
  state.entry = s.length <= 1 || (s.length === 2 && s.startsWith("-")) ? "0" : s.slice(0, -1);
  render();
}

function clearAll() {
  clearBadge();
  state.entry = "0";
  state.acc = null;
  state.op = null;
  state.fresh = true;
  render();
}

// ---- mode switching ------------------------------------------------------

function setMode(m, { reset = true } = {}) {
  if (m === state.mode && reset) { clearAll(); return; }
  state.mode = m;
  state.acc = null;
  state.op = null;
  state.fresh = true;
  if (reset) state.entry = "0";
  el.mode.textContent = m === "int" ? "INT" : "FLT";
  el.mode.dataset.mode = m;
  buildKeypad();
  buildFunctions();
  render();
}

// Cross-mode conversions (the `cast` builtin at the boundary). They compute in
// the current mode then flip to the other, carrying the value along.
function convertToFloat() {
  try {
    const r = calc.fns.to_float(currentValue()); // i64 -> f64
    setMode("float", { reset: false });
    setEntry(r);
  } catch (err) { reportError(err); }
  render();
}
function convertToInt() {
  try {
    const r = calc.fns.to_int(currentValue()); // f64 -> i64 (truncates toward zero)
    setMode("int", { reset: false });
    setEntry(r);
  } catch (err) { reportError(err); }
  render();
}

// ---- keypad ---------------------------------------------------------------

// The keypad's four operator slots + the bottom row differ by mode: float mode
// drops `mod` (no f64 modulo) and gains a decimal-point key.
function opFor(slot) {
  return state.mode === "int"
    ? { divide: "divide", multiply: "multiply", subtract: "subtract", add: "add" }[slot]
    : { divide: "fdivide", multiply: "fmultiply", subtract: "fsubtract", add: "fadd" }[slot];
}

function buildKeypad() {
  el.keys.replaceChildren();
  const mk = (label, cls, act, wide) => {
    const b = document.createElement("button");
    b.textContent = label;
    b.className = `key ${cls || "num"}${wide ? " wide" : ""}`;
    b.addEventListener("click", act);
    return b;
  };
  const rows = [
    ["C", "util", clearAll, "±", "util", toggleSign, "⌫", "util", backspace, "÷", "op", () => inputBinary(opFor("divide"))],
    ["7", null, () => inputDigit("7"), "8", null, () => inputDigit("8"), "9", null, () => inputDigit("9"), "×", "op", () => inputBinary(opFor("multiply"))],
    ["4", null, () => inputDigit("4"), "5", null, () => inputDigit("5"), "6", null, () => inputDigit("6"), "−", "op", () => inputBinary(opFor("subtract"))],
    ["1", null, () => inputDigit("1"), "2", null, () => inputDigit("2"), "3", null, () => inputDigit("3"), "+", "op", () => inputBinary(opFor("add"))],
  ];
  for (const r of rows) {
    for (let i = 0; i < r.length; i += 3) el.keys.appendChild(mk(r[i], r[i + 1], r[i + 2], false));
  }
  // bottom row
  if (state.mode === "int") {
    el.keys.appendChild(mk("0", null, () => inputDigit("0"), true));
    el.keys.appendChild(mk("mod", "op", () => inputBinary("modulo"), false));
  } else {
    el.keys.appendChild(mk("0", null, () => inputDigit("0"), true));
    el.keys.appendChild(mk(".", null, inputDot, false));
  }
  el.keys.appendChild(mk("=", "equals", equals, false));
}

// ---- function chips ------------------------------------------------------

const INT_GROUPS = [
  { title: "Binary", items: [["power","xʸ"],["gcd","gcd"],["lcm","lcm"],["maximum","max"],["minimum","min"],["average","avg"],["percent","% of"]], run: (n) => inputBinary(n) },
  { title: "Unary", items: [["square","x²"],["isqrt","√x"],["factorial","x!"],["fibonacci","fib"],["negate","±x"],["absolute","|x|"],["double","2x"],["increment","x+1"],["decrement","x−1"]], run: (n) => applyUnary(n) },
  { title: "Predicates", items: [["is_even","even?"],["is_prime","prime?"]], run: (n) => applyPredicate(n) },
  { title: "Convert", items: [["to_float","→ float"]], run: () => convertToFloat() },
];

const FLOAT_GROUPS = [
  { title: "Binary", items: [["fpower","xʸ"],["fmaximum","max"],["fminimum","min"]], run: (n) => inputBinary(n) },
  { title: "Unary", items: [["fsquare","x²"],["fsqrt","√x"],["fnegate","±x"],["fabs","|x|"],["freciprocal","1/x"]], run: (n) => applyUnary(n) },
  { title: "Predicates", items: [["is_nan","nan?"]], run: (n) => applyPredicate(n) },
  { title: "Convert", items: [["to_int","→ int"]], run: () => convertToInt() },
];

function buildFunctions() {
  el.functions.replaceChildren();
  const groups = state.mode === "int" ? INT_GROUPS : FLOAT_GROUPS;
  for (const grp of groups) {
    const section = document.createElement("div");
    section.className = "fn-group";
    const h = document.createElement("h3");
    h.textContent = grp.title;
    section.appendChild(h);
    const row = document.createElement("div");
    row.className = "fn-row";
    for (const [name, label] of grp.items) {
      const chip = document.createElement("button");
      chip.className = "chip";
      chip.textContent = label;
      chip.title = name;
      chip.addEventListener("click", () => grp.run(name));
      row.appendChild(chip);
    }
    section.appendChild(row);
    el.functions.appendChild(section);
  }
}

// ---- keyboard ------------------------------------------------------------

window.addEventListener("keydown", (e) => {
  if (!state.ready) return;
  if (e.key >= "0" && e.key <= "9") return inputDigit(e.key);
  if (state.mode === "float" && (e.key === "." || e.key === ",")) return inputDot();
  const map = {
    "+": () => inputBinary(opFor("add")), "-": () => inputBinary(opFor("subtract")),
    "*": () => inputBinary(opFor("multiply")), "/": () => inputBinary(opFor("divide")),
    "Enter": equals, "=": equals, "Backspace": backspace, "Escape": clearAll,
  };
  if (state.mode === "int") map["%"] = () => inputBinary("modulo");
  if (map[e.key]) { e.preventDefault(); map[e.key](); }
});

// ---- boot ----------------------------------------------------------------

async function boot() {
  el.mode.addEventListener("click", () => setMode(state.mode === "int" ? "float" : "int"));
  buildKeypad();
  buildFunctions();
  render();
  try {
    calc = await loadCalculator();
    state.ready = true;
    el.status.textContent = "Aury wasm calculator online — computing in wasm32-wasi";
    el.status.classList.remove("error");
  } catch (err) {
    el.status.textContent = `Failed to load wasm module: ${err.message}`;
    el.status.classList.add("error");
    el.keys.querySelectorAll("button").forEach((b) => (b.disabled = true));
    el.functions.querySelectorAll("button").forEach((b) => (b.disabled = true));
    el.mode.disabled = true;
  }
}

boot();