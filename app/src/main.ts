import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { mount } from "svelte";
// Bundled, not fetched: the app works offline.
import "@fontsource-variable/inter";
import "@fontsource-variable/sora";
import "./app.css";
import App from "./App.svelte";
import Hud from "./Hud.svelte";
import Picker from "./Picker.svelte";
import { flushSave } from "./lib/settings.svelte";
import { watchPage } from "./lib/uiwatch";

// Two short-lived windows share this page: the region picker (`index.html?picker`) and the pop-ups
// and recording dot (`index.html?hud`).
const params = new URLSearchParams(location.search);
const helper = params.has("picker") ? Picker : params.has("hud") ? Hud : null;
// First, so an error while the page starts reaches the log too.
watchPage();
mount(helper ?? App, { target: document.getElementById("app")! });
// The window is destroyed on close: save a change made in the last moment first.
if (!helper && isTauri()) getCurrentWindow().onCloseRequested(() => flushSave());
// A reload (F5) forgets the General page was moving the pop-ups: stop, so they don't stay stuck.
if (!helper && isTauri()) invoke("hud_arrange", { on: false }).catch(() => {});
// Same for a hotkey box that was listening: turn the live hotkeys back on.
if (!helper && isTauri()) invoke("pause_hotkeys", { on: false }).catch(() => {});
// The side mouse buttons would go "back" in the page (they can be bound as hotkeys).
if (!helper) addEventListener("mouseup", (e) => (e.button === 3 || e.button === 4) && e.preventDefault());
