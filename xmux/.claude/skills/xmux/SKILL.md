---
name: xmux
description: Drive the xmux terminal multiplexer as an agent using the "xmux agent" commands — create/kill agent sessions and tabs, run commands in them, and read their screens. Use whenever asked to manage xmux sessions or to run/observe work inside xmux.
---

# Driving xmux as an agent

xmux is a client/server terminal multiplexer: a long-lived server owns
**sessions → tabs → panes** (each pane is one shell). The `xmux agent`
subcommands are a non-interactive control surface made for you: each is
a one-shot command over the server's socket — no pty, no attach, no
keystroke faking.

**Agent commands only touch *agent sessions*** (ones created with
`xmux agent new`). They refuse to kill, type into, or read the user's
own sessions — so you can use them freely without risk. Agent sessions
appear under a separate list in the user's session manager (they press
`a` to see them) and are tagged `agent` in `xmux list`.

## Discover

```sh
xmux list
```

```
○ meow   not running                 <- pinned in config, not started
● work   2 tabs · 3 panes  attached  <- the user's session, hands off
○ build  1 tab · 1 pane · agent      <- an agent session, yours (most
                                        recently active agent lists first)
```

If it errors with "cannot connect to server": `sudo systemctl start xmux`
(or background `xmux server`).

## The commands

```sh
xmux agent new  <session>            # create an agent session (one shell)
xmux agent new  <session> <tab>      # add a tab to it (becomes active)
xmux agent kill <session>            # kill the whole session
xmux agent kill <session> <tab>      # kill one tab (last tab = session dies)
xmux agent send <session> [-t tab] <text...>   # type text + Enter into a pane
xmux agent read <session> [-t tab]             # print the pane grid as text
xmux agent rename <session> <new-name>         # rename an agent session
```

`send`/`read` target the focused pane of the session's active tab;
`-t <tab>` targets a named tab instead. All commands print a
confirmation or a clear error and exit nonzero on failure.

`new`/`send`/`read` bump the session's activity timestamp, and agent
sessions are always listed most-recently-active first — so the top
agent session in `xmux list` is the one most recently worked in.

## Run a command and read its output

```sh
xmux agent new build
xmux agent send build 'cargo test 2>&1 | tail -20'
sleep 5                       # the shell runs it; wait for it to finish
xmux agent read build
```

`read` returns the rendered screen — shell prompt, the command you
typed, and its output — ending with a `== session ... tabs: ... ==`
status line. The screen is a fixed-size grid: long output scrolls off
the top, so for big output redirect to a file and read that, or pipe
through `tail`. Poll with repeated `read` calls to watch progress; a
fresh prompt line at the bottom means the command finished.

Each pane is a real persistent shell: `cd`, env vars, and background
jobs survive between `send` calls, and the session outlives you — the
user can attach to it later with `xmux a <session>` (agent sessions
behave like normal ones).

## Organize work with tabs

```sh
xmux agent new build server     # second tab "server" in build
xmux agent send build -t server './run-dev-server.sh'
xmux agent read build -t server
xmux agent kill build server    # done with that tab
```

## Etiquette

- Name sessions after the work — **short and descriptive** (`build`,
  `tests`, `repro-1234`) so the user knows what they'll find in them.
  Rename when the purpose shifts (`xmux agent rename scratch bisect`);
  names must be unique and can't shadow a config-pinned session.
- Kill your sessions when the work is done — unless the point was to
  leave something running for the user (say so in your summary, and
  tell them: attach with `xmux a <session>`, list with `a` in the
  Ctrl+O session manager).
- A session dies on its own when all its shells exit (e.g. the shell
  crashes or you `send` an `exit`) — check `xmux list` if a session
  seems to be missing.
- Don't try to interactively attach (`xmux a`) yourself: it's a
  full-screen TUI that needs a real terminal; `send`/`read` are your
  hands and eyes.
