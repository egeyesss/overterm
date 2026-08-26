# OverTerm

Agent-aware floating terminal. Runs Claude Code (or any CLI) in an always-on-top
window that stays out of the way while the agent works, then gets your attention
when it finishes or needs input.

## Why

CLI agents like Claude Code run for minutes at a time. You alt-tab away, and then
you miss the moment they finish or ask you a question. OverTerm keeps the session
in a small always-on-top window, works out what the agent is doing, and collapses
to a status bar while it runs. When it finishes or needs an answer, the terminal
comes back on its own without taking keyboard focus from whatever you are doing.

Free, open source, tool-agnostic, with a portable core.

## What it does

- Runs any CLI in a real PTY, so full-colour interactive TUIs work untouched
- Works out whether the session is idle, busy, finished, or waiting on you,
  with no setup or integration required. Running Claude Code, it reads the
  real events and does not have to guess
- Collapses to a one-line bar once you hand it a job, and expands again when
  it wants you. A draft you were part-way through typing follows you between
  the two views
- Floats above other windows on every Space, including over full-screen apps
- `Cmd+Shift+O` from anywhere summons or hides it

## Claude Code

Detection works on any CLI with nothing installed, by reading the terminal.
For Claude Code it can be exact, so on first launch OverTerm adds four hook
entries to `~/.claude/settings.json`: one each for a prompt being submitted,
a turn finishing, a turn failing, and a permission prompt appearing.

Every entry is a single `printf` that prints a fixed JSON object and exits.
Claude Code turns that into an escape sequence in the terminal, and OverTerm
reads it back out of the session it arrived on. Nothing listens on a port,
there is no secret to handle, and it still works over SSH.

The entries merge into whatever hooks you already have, and running the app
again leaves one copy of each. To remove them, delete the four entries whose
command mentions `overterm` from that file. Detection falls back to reading
the terminal, which is what every other CLI gets.

## Stack

- **Tauri v2** — app shell (small footprint; an always-on-top terminal has no
  business using 200 MB of RAM)
- **Rust core crate** — PTY sessions via `portable-pty` (macOS/Linux/Windows-ConPTY),
  no UI dependencies
- **xterm.js** — terminal rendering in the webview
- **objc2** — the AppKit calls a floating overlay needs on macOS, behind a
  single trait so a Windows or Linux port only has to implement that one piece

## Development

Prerequisites: Rust (stable), Node.js 20+, Xcode Command Line Tools (macOS).

```sh
npm install                       # root: Tauri CLI
npm install --prefix app/ui       # frontend deps
npm run tauri dev                 # build + launch
```

Run tests:

```sh
cargo test
```

## Layout

```
overterm/
├── crates/
│   └── core/       # PTY sessions, agent-state detection, window rules (no UI deps)
├── app/
│   ├── src/        # Rust: Tauri setup, native window behaviour, PTY <-> IPC bridge
│   └── ui/         # TypeScript: xterm.js terminal and the collapsed bar
```

## License

MIT
