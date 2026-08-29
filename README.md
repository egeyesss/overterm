# oTerm

**A terminal that knows when your AI agent needs you.**

Run Claude Code, Codex, Gemini CLI or any other command line tool in a
small window that floats above everything else. While the agent works,
oTerm shrinks to a status bar and stays out of your way. The moment it
finishes or asks a question, the terminal comes back on its own, without
you needing to keep alt+tab-ing to check if the task is finished.

macOS 11 or newer. Free and open source, MIT licensed.

> The project is called OverTerm and the app is called oTerm. Same thing.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh | sh
```

That is the whole install. It puts `oTerm.app` in your Applications
folder, and you open it from the Dock, Spotlight or Finder like anything
else. `Cmd+Shift+O` brings it up from any app after that.

Piping a script into a shell is a lot to ask of anybody, so read it first
if you would rather:

```sh
curl -fsSLO https://raw.githubusercontent.com/egeyesss/overterm/main/install.sh
less install.sh
sh install.sh
```

To remove it again, along with the Claude Code entries described below:

```sh
curl -fsSL https://raw.githubusercontent.com/egeyesss/overterm/main/uninstall.sh | sh
```

### Why a script rather than a download

The app is not signed with an Apple Developer certificate, which costs
money every year. macOS refuses to open an unsigned app that carries the
`com.apple.quarantine` attribute, and tells you it cannot be checked for
malware.

That attribute is set by whatever downloaded the file. Browsers set it and
Homebrew sets it. `curl` does not, so an app it downloads is never
quarantined and opens normally.

Being straight about what that means: this gets around the check rather
than passing it. The app is still unsigned, so nothing proves the binary
you run was built from the source in this repo. What you do get is that
every release is built by a GitHub Actions workflow that lives here, and
its log is public. The script also checks the download against the
checksum published beside it, which catches a file that arrived damaged.

### Homebrew

If you would rather use a package manager, the cask needs a second command
because Homebrew quarantines what it downloads:

```sh
brew install --cask egeyesss/overterm/overterm
xattr -dr com.apple.quarantine /Applications/oTerm.app
```

On macOS 15 and later, right-clicking an app to open it stopped working as
a way around this, so the `xattr` line is what you need.

## What it does

**It works out what your agent is doing.** Idle, working, finished, or
waiting on an answer, with nothing to install and no cooperation from the
tool. It reads the terminal the way you would, so it works with a CLI it
has never heard of. With Claude Code it does better than guess, which is
the section below.

**It gets out of the way, then comes back.** Hand a session a job and the
window shrinks to a status bar showing what it is doing, where, and for
how long. When the agent finishes or hits a question, the terminal
returns. It never takes keyboard focus doing that, so it cannot eat a
keystroke meant for the window you were actually using.

**It stays where you can see it.** Always on top, on every Space,
including over another app's full-screen window.

**Several agents at once.** Each session gets a tab, labelled with
whichever tool is running in it and carrying its own status dot, so you
can see which one wants you without switching. The window only tucks
itself away once every session is busy.

**It is a real terminal.** A real PTY, so full colour interactive
interfaces work untouched. Find, clickable links, GPU rendering, correct
emoji widths, and Shift+Enter for a multi-line prompt.

**It is yours to arrange.** Light and dark or follow the system, drag any
edge to resize, three size presets, adjustable transparency, font,
scrollback and summon shortcut. Everything applies without a restart.

### Shortcuts

| Keys | What it does |
|---|---|
| `Cmd+Shift+O` | Summon or hide oTerm from anywhere |
| `Cmd+F` | Find in the scrollback |
| `Cmd+,` | Settings |
| `Shift+Enter` | New line in a prompt instead of submitting |
| `Cmd+K` | Clear the terminal |
| `Cmd+plus` and `Cmd+minus` | Font size |

## Claude Code

Reading the terminal works for every tool and is still a guess. Claude
Code can say exactly what happened, so on first launch oTerm adds four
hook entries to `~/.claude/settings.json`: a prompt being submitted, a
turn finishing, a turn failing, and a permission prompt appearing. It
tells you the first time it does this.

Each entry is a single `printf` that prints a fixed piece of JSON and
exits. Claude Code turns that into an escape sequence in the terminal, and
oTerm reads it back out of the session it arrived on. Nothing listens on a
port, there is no token to handle, and it works over SSH.

The entries merge into whatever hooks you already have and go in once.
There is a switch for them in the settings, which is also how to put them
back if you removed them. Deleting the four entries whose command mentions
`overterm` works too, and oTerm will not add them again. Detection falls
back to reading the terminal, which is what every other tool gets.

## Built with

- **Tauri v2** for the app shell, because an always-on-top terminal has no
  business using 200 MB of RAM
- **Rust** for PTY sessions and the detection state machine, in a core
  crate with no UI dependencies at all
- **xterm.js** for the terminal itself
- **objc2** for the AppKit calls a floating overlay needs, behind a single
  trait so a Windows or Linux port has one file to write

## Contributing

Two things are worth picking up first. Teaching oTerm to recognise a tool
it does not know yet is two lines of config, and a Windows or Linux port
has all of its platform code behind one trait. See
[CONTRIBUTING.md](CONTRIBUTING.md) and [PORTING.md](PORTING.md).

```sh
npm install                  # the Tauri CLI
npm install --prefix app/ui  # the frontend
npm run tauri dev            # build and run
cargo test                   # the tests
```

Tests spawn real shells in real PTYs and replay recorded terminal
transcripts, so they are slower than mocks and a great deal more honest.

```
crates/core   PTY sessions, agent state detection, window rules
app/src       Tauri setup, native window behaviour, the PTY bridge
app/ui        the xterm.js terminal, the tab rail and the collapsed bar
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE.md](NOTICE.md).
