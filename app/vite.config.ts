import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  // `..` lets the UI import the shared /content and /assets folders at the repo root.
  server: { port: 1420, strictPort: true, fs: { allow: [".."] } },
  build: { target: "es2022" },
});
