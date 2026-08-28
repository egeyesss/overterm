# Porting OverTerm

macOS is the only platform with a real implementation. Windows and Linux
ports are not something I plan to write, and the code is arranged so that
somebody else can, without touching anything outside one directory.

This is what such a port has to fill in, and what the macOS one learned
the hard way so you do not have to learn it again.

## Where the work is

`crates/core` is portable already. PTY sessions go through `portable-pty`,
which covers macOS, Linux and Windows ConPTY, and the detection code reads
bytes and a screen model with no idea what it is running on. CI builds
that crate on Linux on every push to keep it that way.

Everything platform specific lives in `app/src/platform/`, which picks its
implementation at compile time:

```rust
#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "fallback.rs")]
mod imp;
```

So a port is a third file next to `macos.rs` and `fallback.rs`, plus one
line in that attribute. Nothing else in the app knows which one is in use.

`fallback.rs` is where to start reading. It compiles today, it is what
everything non-macOS currently gets, and every stub in it names the
Windows and Linux equivalent of what macOS does.

## What has to be written

| Function | What it has to do | Windows | Linux |
|---|---|---|---|
| `set_floating` | Sit above ordinary windows | `WS_EX_TOPMOST` | layer-shell on Wayland, `_NET_WM_STATE_ABOVE` on X11 |
| `stay_visible_when_inactive` | Stay on screen while another app is in front | `WS_EX_NOACTIVATE` | usually free |
| `join_all_spaces` | Be on every desktop | virtual desktop API | `_NET_WM_DESKTOP` set to all |
| `is_on_active_space` | Whether the window is where the user is looking | as above | as above |
| `show_without_focus` | Come forward without stealing keystrokes | `SW_SHOWNOACTIVATE` | depends on the compositor |
| `set_opacity` | Fade the window | `WS_EX_LAYERED` plus `SetLayeredWindowAttributes` | `_NET_WM_WINDOW_OPACITY` |
| `make_accessory` | No taskbar entry, no menu of its own | `WS_EX_TOOLWINDOW` | usually a window type hint |
| `process_name` | Name of a process by pid | see below | `/proc/<pid>/comm` |
| `process_path` | Full path of a process by pid | see below | the `/proc/<pid>/exe` symlink |
| `process_args` | Command line of a process by pid | see below | `/proc/<pid>/cmdline`, already NUL separated |

Returning `None` from the last three is safe. A session whose program
cannot be identified gets a plain label and the default detection
patterns, and nothing else changes.

`show_without_focus` is the one to get right. It runs when an agent
finishes work, which is by definition a moment when the user is typing in
some other application, so a window that takes focus there types into the
wrong place.

## What macOS needed

Useful as a reference, because a floating overlay hits the same set of
problems on every platform.

### Window level

Full screen content composites above the menu bar, so anything below that
is drawn underneath a full screen video no matter what else is set.

| Level | Value |
|---|---|
| Normal | 0 |
| Floating | 3 |
| Modal panel | 8 |
| Dock | 20 |
| Main menu | 24 |
| **Status, which OverTerm uses** | **25** |
| Pop up menu | 101 |
| Screen saver | 1000 |

25 is one step above the menu bar and deliberately no further. The levels
past it belong to system alerts and password prompts, which is not a
terminal's business. A port wants the equivalent of that, rather than the
highest level available.

### Being on every desktop is not the same as being over a full screen app

The single most repeated wrong answer to "how do I float over a full
screen app" on macOS is `canJoinAllSpaces`. It puts a window on every
ordinary desktop and stops at a full screen one, exactly as documented.

What actually does it is two more flags, `Auxiliary` and
`CanJoinAllApplications`, both added in macOS 13. Before that it was not
possible at all, which is why older answers do not mention them.

Expect the same shape of problem elsewhere. The obvious flag covers the
ordinary case and there is a second, less advertised one for the full
screen case.

### Read back what you actually got

Two rounds of changing flags and rebuilding produced nothing here, because
the window was not honouring what it was being told and nothing said so.
Printing the state back is what found the cause:

```sh
OVERTERM_WINDOW_DEBUG=1 npm run tauri dev
```

```
[platform] after setup: level=25 behavior=NSWindowCollectionBehavior(393489)
policy=NSApplicationActivationPolicy(1) visible=true onActiveSpace=true keyWindow=false
```

A port should print the same line for its own platform. It is the
cheapest debugging tool in this repo.

### A window can be visible and unreachable

`isVisible` returns true for a window sitting on a desktop the user is not
looking at. The summon shortcut was a toggle keyed off it, so pressing it
while watching something full screen hid the window being asked for, and
it read as the shortcut doing nothing. Worse, the window reported itself
as holding keyboard focus while being somewhere invisible.

Anything deciding whether the window is reachable has to ask both
questions, which is why `is_on_active_space` exists.

## Two problems that are not about windows

**The Claude Code hook entries are a `printf` command.** Each one is
literally `printf '%s' '<json>'`, run by Claude Code and inherited into
the terminal. PowerShell has no `printf`, so a Windows port needs a
different command that writes the same bytes, and the install and
uninstall code in `app/src/hooks.rs` has to produce it. The entries are
matched and removed by looking for the marker inside the command string,
so whatever you write has to keep that recognisable.

**Finding the program that owns a terminal.** `foreground_pid` in
`crates/core/src/session.rs` asks for the process group leader of the PTY,
which is a Unix idea. Windows consoles have no process group on the
handle, so it returns `None` there and a port has to answer the question
another way. Everything that identifies an agent hangs off that pid, so a
port without it gets plain labels and default detection, which works.

## Checking your work

Build the core crate on your platform first. If it does not build, that is
a bug in this repo rather than in your port, and worth an issue on its
own:

```sh
cargo build -p overterm-core
cargo test -p overterm-core
```

Then the app. Beyond that, the honest test of a window change is watching
it, since none of this is covered by the test suite and cannot easily be.
Put a long running agent in the window, put a full screen video behind it,
and see whether the window does what it says.
