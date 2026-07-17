import { loadCalculator } from "./calculator.js";
import "./styles.css";

const fmt = new Intl.NumberFormat("en-US");
const group = (bigint) => fmt.format(bigint);

const el = {
  entry: document.querySelector("#entry"),
  expr: document.querySelector("#expr"),
  badge: document.querySelector("#badge"),
  status: document.querySelector("#status"),
  keys: document.querySelector("#keys"),
  functions: document.querySelector("#functions"),
};

// Pretty labels + the tiny expression symbol for each binary operator.
const BINARY_SYMBOL = {
  add: "+", subtract: "−", multiply: "×", divide: "÷", modulo: "mod",
  power: "^", gcd: "gcd", lcm: "lcm", maximum: "max", minimum: "min",
  average: "avg", percent: "% of",
};

const state = {
  entry: "0",        // string the user is typing
  acc: null,         // BigInt first operand, or null
  op: null,          // pending binary op name
  fresh: true,       // next digit starts a new entry
  ready: false,      // wasm loaded?
};

let calc = null; // { fns, meta }

function currentValue() {
  return BigInt(state.entry || "0");
}

function render() {
  el.entry.textContent = group(currentValue());
  if (state.op && state.acc !== null) {
    el.expr.textContent = `${group(state.acc)} ${BINARY_SYMBOL[state.op]}`;
  } else {
    el.expr.textContent = "";
  }
}

function setEntry(bigint) {
  state.entry = bigint.toString();
  state.fresh = true;
}

function showBadge(text, tone = "info") {
  el.badge.textContent = text;
  el.badge.dataset.tone = tone;
  el.badge.hidden = false;
}
function clearBadge() {
  el.badge.hidden = true;
}

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
  try {
    setEntry(calc.fns[name](currentValue()));
  } catch (err) {
    reportError(err);
    return;
  }
  render();
}

function applyPredicate(name) {
  try {
    const yes = calc.fns[name](currentValue());
    const label = name === "is_even" ? (yes ? "even" : "odd")
                                     : (yes ? "prime" : "not prime");
    showBadge(`${group(currentValue())} is ${label}`, yes ? "yes" : "no");
  } catch (err) {
    reportError(err);
  }
}

function toggleSign() {
  clearBadge();
  const v = currentValue();
  setEntry(-v);
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

function reportError(err) {
  showBadge(err instanceof Error ? err.message : String(err), "no");
}

// ---- keypad / function buttons -------------------------------------------

// Standard calculator keypad. `null` gaps keep the grid aligned.
const KEYPAD = [
  { label: "C", act: clearAll, cls: "util" },
  { label: "±", act: toggleSign, cls: "util" },
  { label: "⌫", act: backspace, cls: "util" },
  { label: "÷", act: () => inputBinary("divide"), cls: "op" },

  { label: "7", digit: "7" }, { label: "8", digit: "8" }, { label: "9", digit: "9" },
  { label: "×", act: () => inputBinary("multiply"), cls: "op" },

  { label: "4", digit: "4" }, { label: "5", digit: "5" }, { label: "6", digit: "6" },
  { label: "−", act: () => inputBinary("subtract"), cls: "op" },

  { label: "1", digit: "1" }, { label: "2", digit: "2" }, { label: "3", digit: "3" },
  { label: "+", act: () => inputBinary("add"), cls: "op" },

  { label: "0", digit: "0", wide: true }, { label: "mod", act: () => inputBinary("modulo"), cls: "op" },
  { label: "=", act: equals, cls: "equals" },
];

// Extra functions grouped by kind, rendered as labelled chips.
const FUNCTION_GROUPS = [
  {
    title: "Binary",
    items: [
      ["power", "xʸ"], ["gcd", "gcd"], ["lcm", "lcm"],
      ["maximum", "max"], ["minimum", "min"], ["average", "avg"], ["percent", "% of"],
    ],
    run: (name) => inputBinary(name),
  },
  {
    title: "Unary",
    items: [
      ["square", "x²"], ["isqrt", "√x"], ["factorial", "x!"], ["fibonacci", "fib"],
      ["negate", "±x"], ["absolute", "|x|"], ["double", "2x"],
      ["increment", "x+1"], ["decrement", "x−1"],
    ],
    run: (name) => applyUnary(name),
  },
  {
    title: "Predicates",
    items: [["is_even", "even?"], ["is_prime", "prime?"]],
    run: (name) => applyPredicate(name),
  },
];

function buildKeypad() {
  for (const key of KEYPAD) {
    const btn = document.createElement("button");
    btn.textContent = key.label;
    btn.className = `key ${key.cls || "num"}${key.wide ? " wide" : ""}`;
    btn.addEventListener("click", () => (key.digit ? inputDigit(key.digit) : key.act()));
    el.keys.appendChild(btn);
  }
}

function buildFunctions() {
  for (const grp of FUNCTION_GROUPS) {
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

// Physical keyboard support.
window.addEventListener("keydown", (e) => {
  if (!state.ready) return;
  if (e.key >= "0" && e.key <= "9") return inputDigit(e.key);
  const map = {
    "+": () => inputBinary("add"), "-": () => inputBinary("subtract"),
    "*": () => inputBinary("multiply"), "/": () => inputBinary("divide"),
    "%": () => inputBinary("modulo"), "^": () => inputBinary("power"),
    "Enter": equals, "=": equals, "Backspace": backspace, "Escape": clearAll,
  };
  if (map[e.key]) { e.preventDefault(); map[e.key](); }
});

// ---- boot ----------------------------------------------------------------

async function boot() {
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
  }
}

boot();
