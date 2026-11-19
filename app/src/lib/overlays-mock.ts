// Sample apps for previews outside Tauri (Vite dev). Loaded lazily, never in the app.
// ?mock=none shows the empty state.
import type { Scan } from "./overlays";

export function mockScan(): Scan {
  if (new URLSearchParams(location.search).get("mock") === "none") return { apps: [], taken: [] };
  return {
    apps: [
      {
        id: "discord",
        name: "Discord",
        running: true,
        saved: true,
        passes: "both",
        scope: "always",
        keys: [
          { key: "Alt+C", does: "saves a Discord clip" },
          { key: "Ctrl+Shift+M", does: "mutes your mic in Discord" },
        ],
      },
      {
        id: "nvidia",
        name: "NVIDIA app",
        running: true,
        saved: false,
        passes: "hidden",
        scope: "always",
        keys: [
          { key: "Alt+Z", does: "opens the NVIDIA overlay" },
          { key: "Alt+R", does: "shows NVIDIA's performance overlay" },
          { key: "Alt+F10", does: "saves NVIDIA's Instant Replay" },
          { key: "Alt+V", does: "is NVIDIA's push-to-talk key" },
        ],
      },
      {
        id: "steam",
        name: "Steam",
        running: false,
        saved: false,
        passes: "unknown",
        scope: "in-game",
        keys: [
          { key: "Shift+Tab", does: "opens the Steam overlay" },
          { key: "F12", does: "takes a Steam screenshot" },
        ],
      },
    ],
    taken: [{ action: "screenshot-region", key: "Ctrl+Alt+S" }],
  };
}
