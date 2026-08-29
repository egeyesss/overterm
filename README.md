# OverTerm

**The app is called oTerm.** OverTerm is the project; oTerm is what you
install, what sits in the Dock and what the window says.

Agent-aware floating terminal. Runs Claude Code (or any CLI) in an always-on-top
window that stays out of the way while the agent works, then gets your attention
when it finishes or needs input.

## Why

CLI agents like Claude Code run for minutes at a time. You alt-tab away, and then
you miss the moment they finish or ask you a question. oTerm keeps the session
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
- Runs several sessions at once, each in its own tab, labelled with
  whichever agent is in it. The window only gets out of your way once
  every session is busy, and comes back as soon as one of them wants you
- Terminal basics that a terminal should have: find, clickable links,
  GPU rendering, and multi-line prompts on Shift+Enter
- Settings for the window behaviour, the summon shortcut, how see-through
  the window is, the font and the scrollback, all applied without a
  restart

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh | sh
```

That puts the app in `/Applications`. Open it from Finder or Spotlight,
and `Cmd+Shift+O` summons and hides it from anywhere after that.

Piping a script into a shell is a lot to ask, so if you would rather read
it first:

```sh
curl -fsSLO https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh
less install.sh
sh install.sh
```

### What the script gets around

The app is not signed with an Apple Developer certificate. macOS refuses
to open an unsigned app that carries the `com.apple.quarantine`
attribute, and says it cannot check the app for malware.

Whatever downloads a file is what sets that attribute. Browsers set it and
Homebrew sets it. `curl` does not, so an app it downloads is never
quarantined and opens normally.

This gets around the check rather than passing it. The app is still
unsigned, so nothing proves the binary you end up running was built from
the source here. The build that produces it does run as a GitHub Actions
workflow in this repo, and its log is public, which is as close as an
unsigned app gets. Signing it properly needs a paid Apple Developer
account.

The script also checks the download against the checksum published with
the release. That catches a download that arrived damaged and nothing
beyond it, because the checksum comes from the same place the app does.

### Homebrew

The cask still works and still needs a second command, because Homebrew
quarantines what it downloads:

```sh
brew install --cask egeyesss/overterm/overterm
xattr -dr com.apple.quarantine /Applications/oTerm.app
```

On macOS 15 and later, opening the app from the right-click menu stopped
being a way around this.

### Uninstall

```sh
curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/uninstall.sh | sh
```

That removes the app and the Claude Code entries described below.
`brew uninstall --cask overterm` attempts the same. To keep the app and
drop only the entries, use the switch in its settings.

## Claude Code

Detection works on any CLI with nothing installed, by reading the terminal.
For Claude Code it can be exact, so on first launch oTerm adds four hook
entries to `~/.claude/settings.json`: one each for a prompt being submitted,
a turn finishing, a turn failing, and a permission prompt appearing.

Every entry is a single `printf` that prints a fixed JSON object and exits.
Claude Code turns that into an escape sequence in the terminal, and oTerm
reads it back out of the session it arrived on. Nothing listens on a port,
there is no secret to handle, and it still works over SSH.

The entries merge into whatever hooks you already have, and they go in once.
The app says so the first time it does it.

There is a switch for them in the settings, which is the easiest way to
put them back if you removed them or to take them out and keep the app.
Deleting the four entries whose command mentions `overterm` from that file
by hand works too, and oTerm will not put them back. Detection falls
back to reading the terminal, which is what every other CLI gets.

## Stack

- **Tauri v2**: app shell (small footprint; an always-on-top terminal has no
  business using 200 MB of RAM)
- **Rust core crate**: PTY sessions via `portable-pty` (macOS/Linux/Windows-ConPTY),
  no UI dependencies
- **xterm.js**: terminal rendering in the webview
- **objc2**: the AppKit calls a floating overlay needs on macOS, behind a
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
