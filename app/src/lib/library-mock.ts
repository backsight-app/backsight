// Mock library for previews outside Tauri (Vite dev, screenshots). Loaded lazily, never in the app.
// Pick a state with ?mock=empty | nodir | error | few (default: 1,200 items).
import type { Library, LibraryItem } from "./library";

const svg = (hue: number, label: string) =>
  "data:image/svg+xml;utf8," +
  encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' width='160' height='90'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'>` +
      `<stop offset='0' stop-color='hsl(${hue} 60% 38%)'/><stop offset='1' stop-color='hsl(${(hue + 60) % 360} 55% 18%)'/></linearGradient></defs>` +
      `<rect width='160' height='90' fill='url(#g)'/><circle cx='${40 + (hue % 80)}' cy='${30 + (hue % 30)}' r='${14 + (hue % 10)}' fill='hsl(${hue} 70% 70% / .35)'/>` +
      `<text x='8' y='82' fill='white' fill-opacity='.4' font-size='9' font-family='sans-serif'>${label}</text></svg>`,
  );

const games = ["Valorant", "Apex Legends", "Elden Ring", "Rocket League", "Hades II", "Minecraft", "Forza Horizon"];
const res: [number, number][] = [[1920, 1080], [1920, 1080], [2560, 1440], [3840, 2160], [1280, 720]];

function make(count: number): LibraryItem[] {
  let seed = 7;
  const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32);
  const now = Date.now();
  let t = now - 20 * 60_000;
  const items: LibraryItem[] = [];
  for (let i = 0; i < count; i++) {
    const game = games[Math.floor(rnd() * games.length)];
    const shot = rnd() < 0.4;
    const [width, height] = res[Math.floor(rnd() * res.length)];
    const d = new Date(t);
    const stamp = `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}-${String(d.getMinutes()).padStart(2, "0")}-${String(d.getSeconds()).padStart(2, "0")}`;
    const ext = shot ? (rnd() < 0.8 ? "png" : "jpg") : rnd() < 0.7 ? "mp4" : "mkv";
    const secs = 8 + Math.floor(rnd() * rnd() * 900);
    const hue = Math.floor(rnd() * 360);
    const missing = rnd() < 0.05; // some files have no thumbnail or metadata
    items.push({
      path: `C:\\Users\\Player\\${shot ? "Pictures" : "Videos"}\\Backsight\\${game}\\${game} ${stamp}.${ext}`,
      name: `${game} ${stamp}.${ext}`,
      kind: shot ? "screenshot" : "video",
      ext,
      bytes: shot ? Math.round((1.5 + rnd() * 5) * 1024 ** 2) : Math.round(secs * (1.2 + rnd() * 2.6) * 1024 ** 2),
      created_ms: t,
      duration_ms: shot || missing ? null : secs * 1000,
      width: missing ? null : width,
      height: missing ? null : height,
      fps: shot || missing ? null : rnd() < 0.7 ? 60 : 30,
      video_codec: shot || missing ? null : rnd() < 0.75 ? "h264" : "hevc",
      audio_tracks: shot || missing ? null : 2,
      thumbnail: missing ? null : svg(hue, game),
    });
    t -= Math.floor((rnd() < 0.15 ? 14 * 3600_000 : 40 * 60_000) * (0.3 + rnd() * 1.4));
  }
  return items;
}

export async function mockLibrary(): Promise<Library> {
  const mode = new URLSearchParams(location.search).get("mock");
  await new Promise((r) => setTimeout(r, 250));
  if (mode === "error") throw "The library service isn't running.";
  const dirs = { video_dir: "C:\\Users\\Player\\Videos\\Backsight", screenshot_dir: "C:\\Users\\Player\\Pictures\\Backsight" };
  if (mode === "nodir") return { items: [], video_dir: null, screenshot_dir: null };
  if (mode === "empty") return { items: [], ...dirs };
  return { items: make(mode === "few" ? 14 : 1200), ...dirs };
}

/** There is no real clip to play here, so the preview never arrives. */
export const mockPreview = () => new Promise<string>((_, no) => setTimeout(() => no("mock"), 4000));
