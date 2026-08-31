# xmux

A minimal terminal multiplexer built on [`libghostty-vt`](https://crates.io/crates/libghostty-vt),
the terminal emulation engine extracted from [Ghostty](https://ghostty.org).
Client/server like tmux: a long-lived server owns **sessions → tabs →
panes**, thin clients attach over a Unix socket — sessions survive SSH
disconnects, reattach and everything is as you left it.

## Supports agents

LLM agents get a first-class, non-interactive control surface — one-shot
commands over the socket, no pty, no keystroke faking:

```sh
xmux agent new    build              # create an agent session (or: new build <tab>)
xmux agent send   build 'cargo test' # type text + Enter into it ([-t tab])
xmux agent read   build              # the rendered screen, as plain text ([-t tab])
xmux agent rename build tests        # short, descriptive names
xmux agent kill   build              # kill a session (or: kill build <tab>)
```

Agent sessions are sandboxed by design: the agent commands **refuse to
touch your sessions**. They live in their own list, sorted by activity
and tagged with a last-activity age — press **`a`** in the session
manager to check on your agents, or attach with `xmux a <name>`; they
are normal sessions underneath. A ready-made Claude Code skill ships in
[`.claude/skills/xmux/`](.claude/skills/xmux/SKILL.md).

![xmux timelapse: splits, focus, fullscreen, tabs, managers, and an agent session](assets/demo.svg)

## Install

Grab the latest Linux build for your architecture (`x86_64` or
`aarch64`) from the
[releases page](https://github.com/zveinn/xmux/releases) and put `xmux`
on your PATH:

```sh
tar xzf xmux-v*-$(uname -m)-linux.tar.gz && cd xmux-v*-$(uname -m)-linux
sudo install -m755 xmux /usr/local/bin/
```

Then run the server as a systemd system service — it starts at boot and
survives SSH logouts, no linger tricks needed. The unit file ships in
the tarball (and in this repo); set `User=` to your username first:

```sh
sudo cp xmux.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now xmux
```

To build from source instead: `cargo install --path .` — needs Rust
1.90+, plus [Zig](https://ziglang.org) and `git` on PATH
(`libghostty-vt` compiles from Ghostty source).

```sh
xmux a work    # attach to session "work", creating it if new
xmux list      # sessions: tabs, panes, attach state, agent ages
```

Detach with **Ctrl+G** (or drop the SSH connection — the session keeps
running). The socket lives beside the config, at `~/.config/xmux/xmux.sock`
(`XMUX_SOCK` overrides); server logs land in `journalctl -u xmux`. Run the server
with `--config <dir>` to keep `config.yaml` and `layout.json` in a
custom directory instead of `~/.config/xmux/`.

## Config

`~/.config/xmux/config.yaml` — created from the built-in defaults on
first run, and **hot-reloaded** within about a second of saving (a
broken config is rejected and the old one stays active). Shown here
with sample `start_dir` and `commands` values:

```yaml
accent: "#7aa2f7"

shell: /usr/bin/bash

# where new shells start; unset or empty = your home directory
start_dir: ~/code

# lines of scrollback kept per pane
scrollback_lines: 5000

# mouse select-to-copy (clipboard via OSC 52, works over SSH)
select_copy: true

# tab bar position: bottom (default) or top
bar_position: bottom

terminal_envs:
  TERM: xterm-256color

# chords that type a program + Enter into the focused pane
commands:
  ctrl+h: htop
  ctrl+l: lazygit

keybindings:
  session-manager: ctrl+o
  tab-manager: ctrl+n
  split-horizontal: ctrl+w
  split-vertical: ctrl+q
  focus-next: ctrl+t
  focus-left: ctrl+h
  focus-right: ctrl+l
  focus-up: ctrl+k
  focus-down: ctrl+j
  detach: ctrl+g
  fullscreen: ctrl+f
  terminal-settings: ctrl+s

sessions:
  1: { name: project1, key: F1 }
  2: { name: project2, key: F2 }
  3: { name: random, key: F3 }
```

Keys are `[ctrl+][alt+]<char>` or `F1`–`F12`; every binding below is
from this default config and can be remapped. Bound chords are
swallowed by xmux and never reach the inner shell.

## Capabilities

| Capability | Keys / command | Notes |
|---|---|---|
| Sessions | `xmux a <name>` | Created on first attach; survive disconnects; one client per session (a new attach kicks the old) |
| Splits | `ctrl+w` stacked · `ctrl+q` side-by-side | Always 50/50; the new shell opens in the directory of the pane it was split from; a pane's sibling takes its space when the shell exits |
| Focus | `ctrl+h/j/k/l` directional · `ctrl+t` cycle | Left/right cross tab boundaries, wrapping — tabs form one strip. The focused pane's frame is accent-colored with centered `▸◂▴▾` arrows pointing into it |
| Fullscreen | `ctrl+f` | Focused pane takes the whole area; tab bar shows `[F]` |
| Scrollback | mouse wheel · `PageUp`/`PageDown` | `scrollback_lines:` per pane (default 5000); the wheel scrolls the pane under the pointer, typing snaps back to live. Apps that track the mouse or run full-screen get the events instead |
| Focus by mouse | click (any button) · scroll | Clicking or scrolling a pane focuses it, including panes running mouse-tracking apps — the click still reaches the app. Clicking a tab in the tab bar opens that tab |
| Select to copy | drag | `select_copy:` (default on). Releasing copies the selection to your clipboard via OSC 52 — in-band, so it works across SSH; your terminal must allow OSC 52 writes — and clears the highlight. Panes tracking the mouse (vim, htop, lazygit) get the mouse instead |
| Mouse passthrough | automatic | Apps that track the mouse (lazygit, vim, htop) get events in their own pane-local coordinates, re-encoded into the protocol they asked for (SGR, X10, urxvt) and filtered to their tracking mode |
| App clipboard | automatic | OSC 52 yanks from programs inside panes (helix `space+y`, vim) are forwarded to your local clipboard, clipboard/primary register preserved |
| Theme-native colors | automatic | Palette-indexed colors and default fg/bg pass through to your terminal, so panes follow its theme; truecolor is preserved exactly |
| Session manager | `ctrl+o` | `j/k` move · `enter` switch · `n` new · `r` rename · `x` kill · `/` search · `esc` close |
| Text prompts | search, name, and settings fields | Full line editing: `←`/`→` move the caret, `Home`/`End`, `Delete`, `Backspace`, `ctrl+a`/`ctrl+e`/`ctrl+u`/`ctrl+w`; long text scrolls. `esc` cancels, `enter` accepts |
| Agent list | `a` inside the session manager | Agent sessions only, most-recently-active first, with ages |
| Tab manager | `ctrl+n` | Same controls as the session manager |
| Pinned sessions | `sessions:` in the config | An F-key opens the session from anywhere, starting it if needed |
| Commands | `commands:` in the config | The chord types `<program><Enter>` into the focused pane |
| Shell | `shell:` in the config | Spawned in every pane; unset falls back to `$SHELL`, the passwd entry, then `/bin/sh` |
| Shell environment | `terminal_envs:` in the config | Env vars for every spawned shell; default is exactly `TERM=xterm-256color` |
| Start directory | `start_dir:` in the config | Where new shells start; unset = your home directory |
| Accent color | `accent:` in the config | Hex color for the focused-pane frame, tab chip, and selectors; unset follows your terminal palette's cyan |
| Rebindable keys | `keybindings:` in the config | Every control chord above can be remapped (`[ctrl+][alt+]<char>` or `F1`–`F12`); bound chords never reach the inner shell |
| Tab bar position | `bar_position:` in the config | `bottom` (default) or `top`; applies live on config reload |
| Hot reload | edit `config.yaml` | Applies within ~1s of saving: accent, keys, pins, `select_copy`, and `bar_position` live; `shell`, `start_dir`, `terminal_envs`, and `scrollback_lines` to new shells. A broken config is rejected and logged |
| Detach | `ctrl+g` | The session keeps running; reattach with `xmux a` |
| State restore | automatic | Sessions, tabs, splits, and each shell's directory are saved to `~/.config/xmux/layout.json` every 10s and recreated when the server starts (fresh shells in the saved dirs; agent sessions excluded) |
| Auto-run on restore | `ctrl+s` on a pane | Declare a command for the focused pane; it is typed into the restored shell after a server restart. Enter saves, empty clears, esc cancels |
| Agent mode | `xmux agent new/send/read/rename/kill` | Sandboxed to agent-created sessions; bumps activity ordering |
| Listing | `xmux list` | Colored on a tty, plain when piped (agents parse this) |

---

xmux does no terminal emulation of its own — that is all
[`libghostty-vt`](https://crates.io/crates/libghostty-vt), the VT
engine from [Ghostty](https://ghostty.org). Credit for every correctly
parsed escape sequence goes there.
