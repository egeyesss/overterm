# Windows port guide

What a Windows port needs, in the order it makes sense to do it.
[PORTING.md](PORTING.md) covers the parts that are the same on any
platform and is worth reading first; this only adds what is specific to
Windows.

`crates/core` already builds and passes its tests on Windows. PTY sessions
go through `portable-pty`, which uses ConPTY there, and the detection code
reads bytes and a screen model with no idea what it is running on. So the
work is the app crate.

## 1. The home directory, before anything else

Three places ask for `$HOME`, which Windows usually does not set:

| File | Line | Wants |
|---|---|---|
| `app/src/settings.rs` | 706 | `~/.config/overterm/config.toml` |
| `app/src/hooks.rs` | 39 | `~/.claude/settings.json` |
| `app/src/pi.rs` | 32 | `~/.pi/agent` |

All three return `None`, so settings never load and both integrations
quietly do nothing. Nothing crashes and nothing says why, which makes this
the worst thing to leave for later. `%USERPROFILE%` is the right base, and
both Claude Code and Pi use the same dotted directory names under it. The
app's own settings belong in `%APPDATA%` rather than `.config`.

## 2. The window

Eleven stubs in `app/src/platform/fallback.rs`. A port is a new
`windows.rs` beside `macos.rs` and one line in the `cfg_attr` at
`app/src/platform/mod.rs:9`. The table in PORTING.md names the Win32
equivalent for each.

`show_without_focus` is the one that matters. It runs when an agent
finishes, which is by definition a moment the user is typing somewhere
else, so `SW_SHOWNOACTIVATE` and `WS_EX_NOACTIVATE` are load bearing
rather than polish.

The macOS equivalent of "float over a full screen app" has no clean
Windows counterpart. A topmost window sits over a maximised window fine,
but a genuinely exclusive full screen application will cover it. Worth
deciding early whether that is acceptable, since it is the app's main
selling point.

## 3. Identifying the program in a session

PORTING.md says "see below" for these and then does not say it. Here:

| Function | Windows |
|---|---|
| `process_path` | `OpenProcess` then `QueryFullProcessImageNameW` |
| `process_name` | the file name from that path |
| `process_args` | no supported API. `NtQueryInformationProcess` and read the PEB, or query WMI `Win32_Process.CommandLine` |
| `process_cwd` | same problem, same two answers |

`foreground_pid` in `crates/core/src/session.rs:165` is `#[cfg(unix)]` and
returns `None` on Windows, because a ConPTY handle has no process group.
The workable answer is to walk the process tree from the shell that was
spawned with `CreateToolhelp32Snapshot` and take the deepest descendant.

All of this is optional. Every one of these may return `None`, and a
session that cannot be identified gets a plain label and the default
detection patterns. Everything else still works. Do the window first.

## 4. The two integrations

**Claude Code hooks.** Each entry is `printf '%s' '<json>'`. There is no
`printf` on Windows and `cmd`'s quoting will not survive that JSON. Find a
command that writes those exact bytes and produce it from
`app/src/hooks.rs`. Keep the `overterm` marker recognisable inside the
command string, because that is how entries are found again and removed.

**The Pi extension.** `app/pi-extension/overterm.js:34` opens `/dev/tty`,
which does not exist on Windows. `CONOUT$` is the equivalent. The write
has to stay a single call so a marker cannot be cut in half.

Both fail closed. Detection falls back to reading the screen, which is
what every other tool gets anyway.

## 5. Shipping it

`install.sh` is a macOS disk image script and does not carry over. Tauri
can bundle MSI and NSIS installers instead. Windows also needs the WebView2
runtime present, which the bundler can be told to install. The release
workflow builds on `macos-latest` and would need a Windows job beside it.

Signing is the same wall as on macOS: unsigned means SmartScreen warns.
Cheaper than an Apple certificate, still not free.

## Checking your work

```sh
cargo test -p overterm-core     # must pass before you write anything
cargo test                      # then the app
```

Beyond that, watch it. None of the window behaviour is covered by tests and
most of it cannot be. Put a long running agent in the window, put something
full screen behind it, and see whether the window does what it claims.
