// Renders brand PNGs from the SVG sources and (with --screens) README screenshots.
// Uses the Microsoft Edge that ships with Windows, so no browser download is needed.
//
//   npm run assets                      brand PNGs into assets/brand/
//   npm run dev  +  npm run screenshots UI screenshots into docs/assets/screenshots/
import { chromium } from "playwright-core";
import { mkdir, readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

const root = new URL("../../", import.meta.url);
const out = (rel) => fileURLToPath(new URL(rel, root));
const browser = await chromium.launch({ channel: "msedge" });

async function svgToPng(svgRel, pngRel, width, height, scale = 1) {
  const page = await browser.newPage({ viewport: { width, height }, deviceScaleFactor: scale });
  const svg = (await readFile(out(svgRel), "utf8")).replace("<svg ", `<svg width="${width}" height="${height}" `);
  await page.setContent(`<body style="margin:0;background:transparent">${svg}</body>`);
  await page.screenshot({ path: out(pngRel), omitBackground: true });
  await page.close();
}

await svgToPng("assets/brand/backsight-mark.svg", "assets/brand/backsight-mark-1024.png", 1024, 1024);
await svgToPng("assets/brand/backsight-desktop.svg", "assets/brand/backsight-desktop-1024.png", 1024, 1024);
await svgToPng("assets/brand/banner.svg", "assets/brand/banner.png", 1600, 520, 1.5);
await svgToPng("assets/brand/social-preview.svg", "assets/brand/social-preview.png", 1280, 640);

if (process.argv.includes("--screens")) {
  const url = process.env.BACKSIGHT_URL ?? "http://localhost:1420";
  const dir = "docs/assets/screenshots/";
  await mkdir(out(dir), { recursive: true });
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });
  const shot = async (hash, file, prepare) => {
    await page.goto(`${url}/#${hash}`);
    await page.waitForLoadState("networkidle");
    await page.mouse.move(1, 1);
    if (prepare) await prepare();
    await page.waitForTimeout(400);
    await page.screenshot({ path: out(dir + file) });
  };

  await shot("home", "home.png");
  await shot("video", "encoder-picker.png", async () => {
    await page.click('[data-select="encoder"]');
    await page.hover('[role="option"][data-value="nvenc-hevc"]');
  });
  await shot("video", "tooltip.png", async () => {
    await page.click('button[aria-pressed="false"]'); // Advanced mode
    await page.hover('[data-term="multipass"]');
  });
  await shot("hotkeys", "hotkeys.png");
  // A row already in view: scrolling one into view closes the card. The Library uses mock data outside the app. Its preview clip never arrives here, so the card shows the loading state.
  await shot("library", "library.png", async () => {
    await page.hover(".row:has(.dur) >> nth=1");
    await page.waitForSelector(".hover.placed");
  });
  await shot("library", "library-filters.png", async () => {
    await page.click("text=Videos");
    for (const [name, option] of [["size", "Over 1 GB"], ["resolution", "1080p"]]) {
      await page.click(`[data-select="${name}"]`);
      await page.click(`[role="option"]:has-text("${option}")`);
    }
    await page.mouse.move(1, 1);
  });
}

await browser.close();
