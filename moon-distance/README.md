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

Or call the entry directly:

```bash
target/release/aury run moon-distance/moon-distance.aury moon-report "$(date -u +%s)"
```

## Live D3 dashboard

The dashboard is vanilla HTML/CSS/JavaScript with a vendored D3 v7 build. A
small dependency-free Node server refreshes once per minute by compiling the
current timestamp through Aury's native LLVM backend and exposing the native
result as JSON.

```bash
./moon-distance/start-dashboard.sh
# open http://127.0.0.1:4173
```

Set `PORT` or `AURY_BIN` to override the defaults:

```bash
PORT=8080 AURY_BIN=/path/to/aury ./moon-distance/start-dashboard.sh
```

The returned `MoonDistance` record contains:

- `unix_seconds`: input UTC Unix timestamp
- `center_distance_km`: conventional geocentric Earth–Moon distance
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
