# 0007. Raw Input for global hotkeys

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

Hotkeys must work while a fullscreen game has focus, support mouse side buttons and push-to-talk (key-up events), and never look suspicious to anti-cheat.

## Decision

Use **Raw Input** (`RegisterRawInputDevices` with `RIDEV_INPUTSINK`) on a message-only window, and match bindings in our own code. Details are in [hotkeys.md](../architecture/hotkeys.md).

## Alternatives considered

| Option | Why not |
| --- | --- |
| `RegisterHotKey` | No mouse buttons, no key-up (so no push-to-talk), and fails silently when another app owns the combo |
| Low-level hooks (`WH_KEYBOARD_LL`, `WH_MOUSE_LL`) | Can swallow keys, but hooks add input latency system-wide, get unhooked on timeouts, and are a common anti-cheat red flag |

## Consequences

- Works in games, with mouse buttons and push-to-talk, and no hooks.
- Keys aren't swallowed: the game also sees the press. Defaults use Alt plus F-keys to avoid clashes.
- Elevated (admin) games block input to non-elevated apps; Backsight doesn't detect this; running Backsight as administrator too works around it.
