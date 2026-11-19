# 0017. Find other apps' hotkeys by reading, never by hooking

- **Status:** Accepted
- **Date:** 2026-11-18

## Context

Players run overlays, chat, clip and screenshot apps alongside Backsight, and many use the same keys (Alt+R opens NVIDIA's and AMD's overlays, Alt+C saves a Discord clip). An app that holds a key with `RegisterHotKey` or a swallowing keyboard hook stops Backsight's Raw Input from acting on it, so a clash can mean Backsight's key silently does nothing. Windows has no API that says which program owns a key, and tools that find out inject hooks into other processes, which games' anti-cheat treats as hostile.

## Decision

Backsight lists known apps in `content/overlays.json` (detection signals, default keys, how a clash behaves) and finds them from uninstall keys, Store package entries, program files and Toolhelp process names. Where an app's settings are readable (registry, INI, JSON), a small reader per app replaces the defaults with the saved keys. A `RegisterHotKey` probe of Backsight's own keys, held for a moment on a short-lived thread, catches keys another program holds that no listed app could be holding (apps that only watch keys never hold one). Discord's settings are not read: they sit beside its login token, and reading that folder looks like token-stealing malware to antivirus tools. Everything is read only, runs at start and when Home, Setup or Hotkeys opens, never in a loop or while a game is in front.

## Alternatives considered

| Option | Why not |
| --- | --- |
| Global keyboard hook or DLL injection to see who takes a key | Same technique as cheats and key loggers; anti-cheat risk; ADR 0004 rules out injection. |
| Simulate key presses and watch who reacts | Sends input on the player's behalf; triggers the other app's action. |
| Probe only | Can't see hooks or Raw Input listeners (NVIDIA, Discord), and can't name the app. |
| Change the other app's settings to free the key | Never touch another app's data; Backsight moves its own key instead. |

## Consequences

- Clash warnings name the app and what the key does there, and say when Backsight may not see the key.
- The app list and every reader are maintenance: vendors move settings without notice, so each reader fails quietly to the app's usual keys.
- Apps not listed, and portable apps that aren't running, are invisible unless the probe catches them.
