/* global d3 */

const state = {
  data: null,
  history: loadHistory(),
  refreshTimer: null,
};

const number = new Intl.NumberFormat("en-US");
const time = new Intl.DateTimeFormat("en-GB", {
  timeZone: "UTC",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

const elements = {
  distance: document.querySelector("#distance-value"),
  surface: document.querySelector("#surface-value"),
  light: document.querySelector("#light-value"),
  band: document.querySelector("#band-value"),
  range: document.querySelector("#range-label"),
  updated: document.querySelector("#updated-at"),
  next: document.querySelector("#next-update"),
  countdown: document.querySelector("#countdown-value"),
  trend: document.querySelector("#trend-label"),
  status: document.querySelector("#status-text"),
  statusDot: document.querySelector("#status-dot"),
  error: document.querySelector("#error-toast"),
};

// moon-distance.aury, compiled to a wasm32-wasi reactor and served by the host.
// The heavy fixed-point work (46-term lunar series + Q6 cosine) runs here in the
// browser; the server only hands us a timestamp. Exported as `(i64) -> i64`, so
// it crosses the JS boundary as a BigInt with no linear-memory marshaling.
let moonDistanceKm = null;

async function loadMoonModule() {
  const response = await fetch("/moon-distance.wasm", { cache: "no-store" });
  if (!response.ok) throw new Error(`wasm module HTTP ${response.status}`);
  const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
  instance.exports._initialize?.();
  moonDistanceKm = instance.exports["aury__moon-distance-km"];
  if (typeof moonDistanceKm !== "function") {
    throw new Error("wasm module is missing export aury__moon-distance-km");
  }
}

// Derive the display record from the km the wasm returns. These derivations
// mirror the Aury `moon-report` and `classify-distance` functions; keeping them
// here lets the module export a single scalar entry point.
function reportFromTimestamp(unixSeconds) {
  const km = Number(moonDistanceKm(BigInt(unixSeconds)));
  let range = "mid-range";
  if (km < 370_000) range = "near perigee";
  else if (km > 400_000) range = "near apogee";
  return {
    unix_seconds: unixSeconds,
    utc: new Date(unixSeconds * 1000).toISOString(),
    center_distance_km: km,
    surface_distance_km: km - 8_108,
    one_way_light_time_ms: Math.trunc((km * 1_000) / 299_792),
    range,
    source: "Aury wasm32-wasi (in-browser)",
    model: "46-term fixed-point lunar distance series",
    refreshed_every_seconds: 60,
  };
}

function loadHistory() {
  try {
    const values = JSON.parse(localStorage.getItem("aury-moon-history") || "[]");
    return values
      .filter((entry) => Number.isFinite(entry.time) && Number.isFinite(entry.distance))
      .slice(-60);
  } catch {
    return [];
  }
}

function recordHistory(data) {
  const sample = { time: data.unix_seconds * 1000, distance: data.center_distance_km };
  const last = state.history.at(-1);
  if (!last || last.time !== sample.time) state.history.push(sample);
  state.history = state.history.slice(-60);
  localStorage.setItem("aury-moon-history", JSON.stringify(state.history));
}

function tweenNumber(element, nextValue) {
  const previous = Number(String(element.textContent).replaceAll(",", "")) || nextValue;
  d3.select(element)
    .interrupt()
    .transition()
    .duration(900)
    .tween("text", () => {
      const interpolate = d3.interpolateNumber(previous, nextValue);
      return (t) => { element.textContent = number.format(Math.round(interpolate(t))); };
    });
}

const space = d3.select("#space-visual").attr("viewBox", "0 0 1200 400");
const defs = space.append("defs");

const earthGradient = defs.append("radialGradient").attr("id", "earth-gradient").attr("cx", "36%").attr("cy", "30%");
earthGradient.append("stop").attr("offset", "0%").attr("stop-color", "#86d9ff");
earthGradient.append("stop").attr("offset", "44%").attr("stop-color", "#2876c7");
earthGradient.append("stop").attr("offset", "100%").attr("stop-color", "#09234b");

const moonGradient = defs.append("radialGradient").attr("id", "moon-gradient").attr("cx", "34%").attr("cy", "28%");
moonGradient.append("stop").attr("offset", "0%").attr("stop-color", "#fffdf3");
moonGradient.append("stop").attr("offset", "56%").attr("stop-color", "#cbc7bd");
moonGradient.append("stop").attr("offset", "100%").attr("stop-color", "#686b73");

const signalGradient = defs.append("linearGradient").attr("id", "signal-gradient");
signalGradient.append("stop").attr("offset", "0%").attr("stop-color", "#63a5ff").attr("stop-opacity", .25);
signalGradient.append("stop").attr("offset", "100%").attr("stop-color", "#75e5e8").attr("stop-opacity", .8);

const earthX = 120;
const centerY = 175;
const moonScale = d3.scaleLinear().domain([356_500, 406_700]).range([790, 1080]).clamp(true);

space.append("line")
  .attr("class", "distance-track")
  .attr("x1", earthX + 52)
  .attr("x2", 1080)
  .attr("y1", centerY)
  .attr("y2", centerY)
  .attr("stroke", "rgba(139,177,236,.18)")
  .attr("stroke-width", 1)
  .attr("stroke-dasharray", "4 9");

const activeLine = space.append("line")
  .attr("x1", earthX + 52)
  .attr("y1", centerY)
  .attr("y2", centerY)
  .attr("stroke", "url(#signal-gradient)")
  .attr("stroke-width", 1.5);

const earth = space.append("g").attr("transform", `translate(${earthX},${centerY})`);
earth.append("circle").attr("r", 65).attr("fill", "none").attr("stroke", "rgba(99,165,255,.12)");
earth.append("circle").attr("r", 52).attr("fill", "url(#earth-gradient)").attr("filter", "drop-shadow(0 0 22px rgba(54,132,240,.34))");
earth.append("path").attr("d", "M-36,-17 C-17,-31 3,-28 12,-16 C21,-4 9,4 -4,2 C-17,0 -16,14 -31,10 Z").attr("fill", "rgba(82,188,154,.48)");
earth.append("path").attr("d", "M8,13 C25,6 42,17 38,29 C29,37 17,42 5,36 C-1,28 1,19 8,13 Z").attr("fill", "rgba(74,170,137,.4)");
earth.append("text").attr("y", 92).attr("text-anchor", "middle").attr("fill", "#8e9ab3").attr("font-family", "DM Mono").attr("font-size", 10).attr("letter-spacing", ".14em").text("EARTH");
earth.append("text").attr("y", 107).attr("text-anchor", "middle").attr("fill", "#566079").attr("font-family", "DM Mono").attr("font-size", 8).text("R 6,371 km");

const moon = space.append("g");
moon.append("circle").attr("r", 14).attr("fill", "url(#moon-gradient)").attr("filter", "drop-shadow(0 0 16px rgba(232,227,216,.27))");
moon.append("circle").attr("cx", -4).attr("cy", -3).attr("r", 2.7).attr("fill", "rgba(75,78,84,.22)");
moon.append("circle").attr("cx", 5).attr("cy", 4).attr("r", 3.4).attr("fill", "rgba(75,78,84,.18)");
moon.append("circle").attr("cx", 4).attr("cy", -6).attr("r", 1.6).attr("fill", "rgba(75,78,84,.2)");
moon.append("text").attr("y", 49).attr("text-anchor", "middle").attr("fill", "#8e9ab3").attr("font-family", "DM Mono").attr("font-size", 10).attr("letter-spacing", ".14em").text("MOON");
moon.append("text").attr("y", 64).attr("text-anchor", "middle").attr("fill", "#566079").attr("font-family", "DM Mono").attr("font-size", 8).text("R 1,737 km");

const signal = space.append("circle").attr("r", 3.5).attr("cy", centerY).attr("fill", "#75e5e8").attr("filter", "drop-shadow(0 0 7px #75e5e8)");

const bracket = space.append("g");
bracket.append("line").attr("class", "bracket-line").attr("x1", earthX).attr("y1", 290).attr("y2", 290).attr("stroke", "rgba(169,190,231,.34)");
bracket.append("line").attr("x1", earthX).attr("x2", earthX).attr("y1", 282).attr("y2", 298).attr("stroke", "rgba(169,190,231,.34)");
bracket.append("line").attr("class", "bracket-end").attr("y1", 282).attr("y2", 298).attr("stroke", "rgba(169,190,231,.34)");
bracket.append("text").attr("class", "bracket-label").attr("y", 317).attr("text-anchor", "middle").attr("fill", "#75e5e8").attr("font-family", "DM Mono").attr("font-size", 11);

const gaugeX = d3.scaleLinear().domain([356_500, 406_700]).range([255, 1065]);
space.append("line").attr("x1", gaugeX.range()[0]).attr("x2", gaugeX.range()[1]).attr("y1", 366).attr("y2", 366).attr("stroke", "rgba(169,190,231,.17)").attr("stroke-width", 4).attr("stroke-linecap", "round");
space.append("text").attr("x", gaugeX.range()[0]).attr("y", 389).attr("text-anchor", "start").attr("fill", "#566079").attr("font-family", "DM Mono").attr("font-size", 8).text("PERIGEE · 356,500 km");
space.append("text").attr("x", gaugeX.range()[1]).attr("y", 389).attr("text-anchor", "end").attr("fill", "#566079").attr("font-family", "DM Mono").attr("font-size", 8).text("APOGEE · 406,700 km");
const gaugeMarker = space.append("g");
gaugeMarker.append("line").attr("y1", 354).attr("y2", 376).attr("stroke", "#f5bd67").attr("stroke-width", 2);
gaugeMarker.append("circle").attr("cy", 366).attr("r", 4).attr("fill", "#f5bd67");

function animateSignal(moonX) {
  signal.interrupt().attr("cx", earthX + 56).attr("opacity", 0);
  signal
    .transition().duration(250).attr("opacity", 1)
    .transition().duration(2600).ease(d3.easeLinear).attr("cx", moonX - 17)
    .transition().duration(250).attr("opacity", 0)
    .on("end", () => animateSignal(moonX));
}

function updateSpace(data) {
  const moonX = moonScale(data.center_distance_km);
  moon.transition().duration(1100).ease(d3.easeCubicOut).attr("transform", `translate(${moonX},${centerY})`);
  activeLine.transition().duration(1100).attr("x2", moonX - 15);
  bracket.select(".bracket-line").transition().duration(1100).attr("x2", moonX);
  bracket.select(".bracket-end").transition().duration(1100).attr("x1", moonX).attr("x2", moonX);
  bracket.select(".bracket-label").attr("x", (earthX + moonX) / 2).text(`${number.format(data.center_distance_km)} KM · CENTER TO CENTER`);
  gaugeMarker.transition().duration(1100).attr("transform", `translate(${gaugeX(data.center_distance_km)},0)`);
  animateSignal(moonX);
}

function updateHistory() {
  const svg = d3.select("#history-visual").attr("viewBox", "0 0 1160 165");
  svg.selectAll("*").remove();
  const values = state.history;
  if (!values.length) return;

  const margin = { top: 14, right: 12, bottom: 28, left: 64 };
  const width = 1160 - margin.left - margin.right;
  const height = 165 - margin.top - margin.bottom;
  const extent = d3.extent(values, (d) => d.distance);
  const spread = Math.max(4, (extent[1] - extent[0]) * .3);
  const x = d3.scaleTime().domain(d3.extent(values, (d) => new Date(d.time))).range([0, width]);
  if (values.length === 1) x.domain([new Date(values[0].time - 60_000), new Date(values[0].time + 60_000)]);
  const y = d3.scaleLinear().domain([extent[0] - spread, extent[1] + spread]).nice().range([height, 0]);
  const group = svg.append("g").attr("transform", `translate(${margin.left},${margin.top})`);

  group.append("g").attr("class", "axis").attr("transform", `translate(0,${height})`).call(d3.axisBottom(x).ticks(Math.min(6, values.length)).tickFormat(d3.utcFormat("%H:%M")));
  group.append("g").attr("class", "axis").call(d3.axisLeft(y).ticks(4).tickFormat(d3.format(",d")));
  group.append("path")
    .datum(values)
    .attr("fill", "none")
    .attr("stroke", "#63a5ff")
    .attr("stroke-width", 2)
    .attr("d", d3.line().x((d) => x(new Date(d.time))).y((d) => y(d.distance)).curve(d3.curveMonotoneX));
  group.selectAll("circle.sample")
    .data(values)
    .join("circle")
    .attr("class", "sample")
    .attr("cx", (d) => x(new Date(d.time)))
    .attr("cy", (d) => y(d.distance))
    .attr("r", 3)
    .attr("fill", "#75e5e8");

  if (values.length > 1) {
    const delta = values.at(-1).distance - values.at(-2).distance;
    elements.trend.textContent = `${delta >= 0 ? "+" : ""}${number.format(delta)} km since last sample · ${values.length} samples`;
  } else {
    elements.trend.textContent = "First sample recorded · awaiting next minute";
  }
}

function applyData(data) {
  state.data = data;
  recordHistory(data);
  tweenNumber(elements.distance, data.center_distance_km);
  tweenNumber(elements.surface, data.surface_distance_km);
  tweenNumber(elements.light, data.one_way_light_time_ms);
  elements.band.textContent = data.range;
  elements.range.textContent = data.range.toUpperCase();
  elements.updated.textContent = time.format(new Date(data.unix_seconds * 1000));
  elements.status.textContent = "Aury wasm telemetry online";
  elements.statusDot.classList.remove("error");
  elements.error.hidden = true;
  updateSpace(data);
  updateHistory();
}

async function refresh() {
  clearTimeout(state.refreshTimer);
  try {
    elements.status.textContent = "Fetching timestamp · computing in wasm…";
    const response = await fetch("/api/timestamp", { cache: "no-store" });
    const params = await response.json();
    if (!response.ok) throw new Error(params.detail || params.error || `HTTP ${response.status}`);
    applyData(reportFromTimestamp(params.unix_seconds));
  } catch (error) {
    elements.status.textContent = "Aury wasm telemetry unavailable";
    elements.statusDot.classList.add("error");
    elements.error.textContent = error instanceof Error ? error.message : String(error);
    elements.error.hidden = false;
  } finally {
    const delay = 60_000 - (Date.now() % 60_000) + 350;
    state.refreshTimer = setTimeout(refresh, delay);
  }
}

setInterval(() => {
  const remaining = Math.max(0, Math.ceil((60_000 - (Date.now() % 60_000)) / 1000));
  elements.countdown.textContent = String(remaining);
  elements.next.textContent = `Next wasm sample in ${remaining}s`;
}, 250);

elements.status.textContent = "Loading Aury wasm module…";
loadMoonModule()
  .then(refresh)
  .catch((error) => {
    elements.status.textContent = "Aury wasm module failed to load";
    elements.statusDot.classList.add("error");
    elements.error.textContent = error instanceof Error ? error.message : String(error);
    elements.error.hidden = false;
  });
