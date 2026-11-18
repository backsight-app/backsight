// Prints section 4 of THIRD-PARTY-NOTICES.txt from cargo-about's JSON output.
// Usage: node crates.mjs <about.json>   (scripts/third-party-notices.ps1 runs it)
import { readFileSync } from "node:fs";
import { join } from "node:path";

const here = import.meta.dirname;
const about = JSON.parse(readFileSync(process.argv[2], "utf8"));

// Crates published without their license text: the text from their repository, kept in texts/.
const handTexts = {
  "alloc-stdlib": "alloc-stdlib.txt",
  "webview2-com": "webview2-rs.txt",
  "webview2-com-sys": "webview2-rs.txt",
  "webview2-com-macros": "webview2-rs.txt",
};
// Without a file, cargo-about prints the license's standard text. That is complete for these (the
// Unlicense names no one);
// the others name a copyright holder, so a missing file needs a hand-supplied text.
const standardText = new Set(["Apache-2.0", "MPL-2.0", "Unlicense"]);

const groups = []; // { title, text, crates }
const byFile = new Map();
const credited = new Set();
for (const license of about.licenses) {
  const crates = [];
  for (const { crate } of license.used_by) {
    if (!crate.source) continue; // Backsight's own crates
    credited.add(`${crate.name} ${crate.version}`);
    if (license.source_path || standardText.has(license.id)) {
      crates.push(crate);
      continue;
    }
    const file = handTexts[crate.name];
    if (!file) throw new Error(`${crate.name} ${crate.version} has no ${license.id} text; add one to scripts/third-party-notices/texts.`);
    if (!byFile.has(file)) {
      const group = { title: `${license.name} (${license.id})`, text: readFileSync(join(here, "texts", file), "utf8"), crates: [] };
      byFile.set(file, group);
      groups.push(group);
    }
    byFile.get(file).crates.push(crate);
  }
  if (crates.length) groups.push({ title: `${license.name} (${license.id})`, text: license.text, crates });
}

const line = "-".repeat(72);
const out = [
  "========================================================================",
  "4. Rust crates",
  "========================================================================",
  "",
  `Backsight is built with ${credited.size} Rust crates (libraries) from crates.io. Each license text`,
  "below comes after the list of crates it covers. Crates under the Mozilla Public License 2.0",
  "are used unchanged, and their source code is at the crates.io address given for each.",
  "",
];
for (const g of groups) {
  out.push(line, g.title, "Used by:");
  for (const c of g.crates) {
    const where = g.title.includes("MPL-2.0") ? `source: https://crates.io/crates/${c.name}/${c.version}` : c.repository;
    out.push(`  ${c.name} ${c.version}${where ? `  (${where})` : ""}`);
  }
  out.push("", g.text.replace(/\r\n/g, "\n").trim(), "");
}
process.stdout.write(out.join("\n") + "\n");
