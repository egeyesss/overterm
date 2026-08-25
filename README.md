# OverTerm

Agent-aware floating terminal. Runs Claude Code (or any CLI) in an always-on-top
window that stays out of the way while the agent works, then gets your attention
when it finishes or needs input.

## Why

CLI agents like Claude Code run for minutes at a time. You alt-tab away, and then
you miss the moment they finish or ask you a question. OverTerm keeps the session
in a small always-on-top window and (soon) detects agent state to collapse out of
your way while it works and grab your attention when it needs you.

Free, open source, tool-agnostic, with a portable core.

## Stack

- **Tauri v2** — app shell (small footprint; an always-on-top terminal has no
  business using 200 MB of RAM)
- **Rust core crate** — PTY sessions via `portable-pty` (macOS/Linux/Windows-ConPTY),
  no UI dependencies
- **xterm.js** — terminal rendering in the webview

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
│   └── core/       # PTY sessions, (soon) detection state machine — no UI deps
├── app/
│   ├── src/        # Rust: Tauri setup, commands, PTY <-> IPC bridge
│   └── ui/         # TypeScript: xterm.js frontend
```

## License

MIT
