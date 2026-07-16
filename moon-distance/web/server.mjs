#!/usr/bin/env node

import { execFile } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
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
// moon-distance.aury is compiled to wasm ONCE at startup; the browser then runs
// the module every minute. The server only supplies the timestamp parameter.
const wasmModulePath = join(tmpdir(), `aury-moon-distance-${process.pid}.wasm`);
const port = Number(process.env.PORT || 4173);

const staticFiles = new Map([
  ["/", ["index.html", "text/html; charset=utf-8"]],
  ["/index.html", ["index.html", "text/html; charset=utf-8"]],
  ["/app.js", ["app.js", "text/javascript; charset=utf-8"]],
  ["/styles.css", ["styles.css", "text/css; charset=utf-8"]],
  ["/vendor/d3.v7.min.js", ["vendor/d3.v7.min.js", "text/javascript; charset=utf-8"]],
  ["/vendor/d3.LICENSE", ["vendor/d3.LICENSE", "text/plain; charset=utf-8"]],
]);

async function ensureAuryBinary() {
  if (existsSync(auryBinary)) return;
  await execFileAsync("cargo", ["build", "--release"], {
    cwd: projectDirectory,
    maxBuffer: 8 * 1024 * 1024,
  });
}

// Compile moon-distance.aury into a wasm32-wasi reactor module that exports
// `aury__moon-distance-km`. Done once; the module is served as a static asset
// and executed in the browser, so no clang runs per request.
async function buildWasmModule() {
  await ensureAuryBinary();
  const { stderr } = await execFileAsync(
    auryBinary,
    ["wasm-lib", moonProgram, "--export", "moon-distance-km", "-o", wasmModulePath],
    { cwd: projectDirectory, maxBuffer: 8 * 1024 * 1024 },
  );
  if (stderr.trim()) console.error(stderr.trim());
  if (!existsSync(wasmModulePath)) {
    throw new Error("wasm-lib did not produce a module");
  }
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

    // The "parameter from the server": the current UTC timestamp. The browser
    // feeds this into the wasm module to compute the distance client-side.
    if (url.pathname === "/api/timestamp") {
      const unixSeconds = Math.floor(Date.now() / 1000);
      sendJson(response, 200, {
        unix_seconds: unixSeconds,
        utc: new Date(unixSeconds * 1000).toISOString(),
        refreshed_every_seconds: 60,
      });
      return;
    }

    if (url.pathname === "/moon-distance.wasm") {
      const body = await readFile(wasmModulePath);
      response.writeHead(200, {
        "Content-Type": "application/wasm",
        "Cache-Control": "no-store",
      });
      response.end(body);
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
      error: "Request failed",
      detail: error instanceof Error ? error.message : String(error),
    });
  }
});

buildWasmModule()
  .then(() => {
    server.listen(port, "127.0.0.1", () => {
      console.log(`Moon Distance dashboard: http://127.0.0.1:${port}`);
      console.log(`Aury wasm module: ${wasmModulePath}`);
      console.log("moon-distance.aury → wasm32-wasi reactor; the browser runs it each minute.");
    });
  })
  .catch((error) => {
    console.error("Failed to build the Aury wasm module:", error);
    process.exit(1);
  });
