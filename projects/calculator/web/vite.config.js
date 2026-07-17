import { defineConfig } from "vite";

// The Aury-compiled calculator.wasm lives in public/ and is served at /calculator.wasm.
// No plugins needed: the reactor module links with zero imports and is instantiated
// directly from an ArrayBuffer at runtime.
export default defineConfig({
  server: { open: true },
});
