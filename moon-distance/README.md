# Aury Moon Distance

A fixed-point Aury program that estimates the Moon's **geocentric distance**
(the distance between the centers of Earth and Moon) for a supplied Unix UTC
timestamp.

Aury v0 cannot read the system clock, so `run-now.sh` obtains the current Unix
timestamp and passes it to the pure Aury calculation.

## Run it now

```bash
./moon-distance/run-now.sh
```

Run through the native LLVM backend:

```bash
./moon-distance/run-now.sh --native
```

Run through the **WebAssembly (wasm32-wasi)** backend — builds a module and
executes it with `wasmtime`/`wasmer`:

```bash
./moon-distance/run-now.sh --wasm
```

Or call the entry directly:

```bash
target/release/aury run moon-distance/moon-distance.aury moon-report "$(date -u +%s)"
```

## wasm toolchain

The `--wasm` mode and the dashboard need a `wasm32-wasi` toolchain (clang with
the WebAssembly target, a wasi-libc sysroot, `wasm-ld`) plus a wasm runtime.
[`wasm-toolchain.sh`](wasm-toolchain.sh) auto-detects and exports the right
environment: a self-contained [wasi-sdk](https://github.com/WebAssembly/wasi-sdk)
install (`WASI_SDK_PATH` or `/opt/wasi-sdk`), or a Homebrew assembly:

```bash
brew install llvm lld wasi-libc wasmtime
```

Homebrew's `llvm` omits the wasm32 `compiler-rt` builtins; see the root
[README](../README.md#webassembly-wasm32-wasi) for the one-time step that adds
them. Anything you export yourself (`AURY_WASM_CLANG`, `WASI_SYSROOT`) is
respected.

## Live D3 dashboard — the Aury model runs in the browser

The dashboard is vanilla HTML/CSS/JavaScript with a vendored D3 v7 build. The
Aury computation runs **client-side in WebAssembly**, not on the server:

1. At startup the Node server compiles `moon-distance.aury` **once** into a
   `wasm32-wasi` reactor module (`aury wasm-lib … --export moon-distance-km`)
   and serves it as a static `/moon-distance.wasm`.
2. The server exposes `/api/timestamp` — the only parameter it supplies.
3. The browser instantiates the module once, then each minute fetches the
   timestamp and calls the exported `aury__moon-distance-km(unix_seconds)`. The
   heavy fixed-point work (46-term series + Q6 cosine) happens in the page; the
   derived fields below are computed in JS from the returned km, mirroring the
   Aury `moon-report` / `classify-distance` functions.

No clang runs per request, and the module has **zero imports**, so the browser
loads it with an empty import object — no WASI shim.

```bash
./moon-distance/start-dashboard.sh
# open http://127.0.0.1:4173
```

`start-dashboard.sh` sources `wasm-toolchain.sh` before launching the server, so
the one-time `aury wasm-lib` build finds its toolchain. Set `PORT` or `AURY_BIN`
to override the defaults:

```bash
PORT=8080 AURY_BIN=/path/to/aury ./moon-distance/start-dashboard.sh
```

The displayed record contains:

- `unix_seconds`: input UTC Unix timestamp (from the server)
- `center_distance_km`: conventional geocentric Earth–Moon distance (from wasm)
- `surface_distance_km`: approximate surface-to-surface distance, subtracting
  mean Earth and Moon radii
- `one_way_light_time_ms`: approximate one-way radio/light travel time
- `range`: `near perigee`, `mid-range`, or `near apogee`

## Model

The implementation uses only Aury `i64` arithmetic:

1. Convert seconds since J2000 into the lunar fundamental angles `D`, `M`,
   `M'`, and `F`, represented in millidegrees.
2. Evaluate cosine with Q6 fixed-point arithmetic and an eighth-order Taylor
   polynomial after quadrant reduction.
3. Evaluate 46 periodic distance terms from the lunar distance series in
   Meeus, *Astronomical Algorithms*, Table 47.A.
4. Return kilometers and derived values as an immutable Aury struct.

This is an astronomical approximation, not a navigation ephemeris. Near the
creation time, the Aury result was **362,738 km** at
`2026-07-15T08:32:26Z`; JPL Horizons reported **362,754 km** for 08:32 UTC, a
16 km difference.

## Verify and regenerate

The source is authored as typed-object JSON, then ingested into canonical Aury:

```bash
python3 moon-distance/build.py
target/release/aury ingest \
  moon-distance/moon-distance.json \
  moon-distance/moon-distance.aury
target/release/aury validate moon-distance/moon-distance.aury
target/release/aury test moon-distance/moon-distance.aury 12345
```

The property suite checks angle normalization, cosine symmetry, and physical
lunar-distance bounds.
