#!/usr/bin/env node

import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile, rm } from "node:fs/promises";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const webDirectory = dirname(fileURLToPath(import.meta.url));
const projectDirectory = resolve(webDirectory, "../..");
const auryBinary = process.env.AURY_BIN || join(projectDirectory, "target/release/aury");
const moonProgram = join(projectDirectory, "moon-distance/moon-distance.aury");
const port = Number(process.env.PORT || 4173);

const staticFiles = new Map([
  ["/", ["index.html", "text/html; charset=utf-8"]],
  ["/index.html", ["index.html", "text/html; charset=utf-8"]],
  ["/app.js", ["app.js", "text/javascript; charset=utf-8"]],
  ["/styles.css", ["styles.css", "text/css; charset=utf-8"]],
  ["/vendor/d3.v7.min.js", ["vendor/d3.v7.min.js", "text/javascript; charset=utf-8"]],
  ["/vendor/d3.LICENSE", ["vendor/d3.LICENSE", "text/plain; charset=utf-8"]],
]);

let cachedMinute = null;
let cachedData = null;
let inFlight = null;

function parseAuryReport(output) {
  const pattern = /MoonDistance\{unix_seconds: (-?\d+), center_distance_km: (-?\d+), surface_distance_km: (-?\d+), one_way_light_time_ms: (-?\d+), range: "([^"]+)"\}/;
  const match = output.match(pattern);
  if (!match) {
    throw new Error(`Unexpected native Aury output: ${output.trim()}`);
  }
  const [, unixSeconds, centerKm, surfaceKm, lightTimeMs, range] = match;
  return {
    unix_seconds: Number(unixSeconds),
    utc: new Date(Number(unixSeconds) * 1000).toISOString(),
    center_distance_km: Number(centerKm),
    surface_distance_km: Number(surfaceKm),
    one_way_light_time_ms: Number(lightTimeMs),
    range,
    source: "Aury native LLVM backend",
    model: "46-term fixed-point lunar distance series",
    refreshed_every_seconds: 60,
  };
}

async function ensureAuryBinary() {
  if (existsSync(auryBinary)) return;
  await execFileAsync("cargo", ["build", "--release"], {
    cwd: projectDirectory,
    maxBuffer: 8 * 1024 * 1024,
  });
}

async function calculateMinute(minute) {
  await ensureAuryBinary();
  const timestamp = minute * 60;
  const executable = join(tmpdir(), `aury-moon-web-${process.pid}-${minute}`);
  try {
    // Aury's generated main embeds its typed CLI argument, so each minute gets
    // a fresh tiny native executable. `aury compile` lowers to LLVM, invokes
    // clang, runs the executable, and returns the native struct output.
    const { stdout, stderr } = await execFileAsync(
      auryBinary,
      ["compile", moonProgram, "moon-report", String(timestamp), "-o", executable],
      { cwd: projectDirectory, maxBuffer: 8 * 1024 * 1024 },
    );
    if (stderr.trim()) console.error(stderr.trim());
    return parseAuryReport(stdout);
  } finally {
    await rm(executable, { force: true });
  }
}

async function currentMoonData() {
  const minute = Math.floor(Date.now() / 60_000);
  if (cachedData && cachedMinute === minute) return cachedData;
  if (!inFlight) {
    inFlight = calculateMinute(minute)
      .then((data) => {
        cachedMinute = minute;
        cachedData = data;
        return data;
      })
      .finally(() => {
        inFlight = null;
      });
  }
  return inFlight;
}

function sendJson(response, status, value) {
  response.writeHead(status, {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "no-store",
  });
  response.end(JSON.stringify(value));
}

const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url || "/", `http://${request.headers.host || "localhost"}`);
    if (url.pathname === "/api/moon-distance") {
      sendJson(response, 200, await currentMoonData());
      return;
    }

    const staticFile = staticFiles.get(url.pathname);
    if (!staticFile) {
      sendJson(response, 404, { error: "Not found" });
      return;
    }
    const [name, contentType] = staticFile;
    const body = await readFile(join(webDirectory, name));
    response.writeHead(200, {
      "Content-Type": contentType,
      "Cache-Control": extname(name) === ".html" ? "no-cache" : "public, max-age=300",
    });
    response.end(body);
  } catch (error) {
    console.error(error);
    sendJson(response, 500, {
      error: "Native Aury calculation failed",
      detail: error instanceof Error ? error.message : String(error),
    });
  }
});

server.listen(port, "127.0.0.1", () => {
  console.log(`Moon Distance dashboard: http://127.0.0.1:${port}`);
  console.log(`Native Aury binary: ${auryBinary}`);
});
