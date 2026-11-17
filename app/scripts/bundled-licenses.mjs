// Prints the license section for the npm packages that end up inside the built UI (app/dist).
// Builds the UI in memory and reads which modules each chunk holds, so dev-only packages
// (Vite, the Svelte compiler's build parts, Playwright) never show up.
// Used by scripts/third-party-notices.ps1. Run from app/: node scripts/bundled-licenses.mjs
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { build } from "vite";

const out = await build({ logLevel: "silent", build: { write: false } });
const ids = (Array.isArray(out) ? out : [out])
  .flatMap((o) => o.output)
  .flatMap((c) => (c.type === "chunk" ? c.moduleIds : []))
  .map((id) => id.replace(/\\/g, "/"));

const packages = new Map();
let preload = false;
for (const id of ids) {
  const m = id.match(/^(.*\/node_modules\/((?:@[^/]+\/)?[^/]+))\//);
  if (m) packages.set(m[2], m[1]);
  // Vite's own helper for dynamic imports is inlined into the bundle.
  else if (id.includes("vite/preload-helper")) preload = true;
}
if (preload) packages.set("vite", join(process.cwd(), "node_modules/vite"));

// Packages that ship no license text, and the package from the same project whose texts apply.
const sameAs = { "@tauri-apps/plugin-dialog": "@tauri-apps/api" };

const sections = [...packages].sort(([a], [b]) => a.localeCompare(b)).map(([name, dir]) => {
  const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  const files = readdirSync(dir).filter((f) => /^(licen[cs]e|copying)([._-][\w.-]*)?$/i.test(f) && !f.endsWith(".spdx"));
  // Vite's file goes on with the licenses of its own build tools, none of which ship.
  let texts = files.map((f) => readFileSync(join(dir, f), "utf8").replace(/\r\n/g, "\n").split("# Licenses of bundled dependencies")[0].trim());
  if (!texts.length && sameAs[name] && packages.has(sameAs[name])) {
    texts = [`The license texts are the ones printed for ${sameAs[name]} (same project, same licenses).`];
  }
  if (!texts.length) throw new Error(`${name} has no license file; add its text by hand.`);
  const repo = typeof pkg.repository === "string" ? pkg.repository : pkg.repository?.url;
  const what = name === "vite" ? " (only its small helper for loading parts of the page later)" : "";
  return [
    `${name} ${pkg.version}${what}`,
    `License: ${pkg.license}`,
    repo ? `Source: ${repo.replace(/^git\+/, "")}` : null,
    "",
    texts.join("\n\n"),
  ]
    .filter((l) => l !== null)
    .join("\n");
});
process.stdout.write(sections.join("\n\n" + "-".repeat(72) + "\n\n") + "\n");
