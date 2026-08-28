# Contributing

Thanks for looking. This is a small project, and the contributions that
help most are small too.

## What is most useful

**A detection profile for an agent you actually run.** Two regexes in a
config file, no new code, and it is the difference between the window
behaving and the window guessing. Described in full below.

**A Windows or Linux port.** All the platform code sits behind one trait
in one directory, and every stub in `app/src/platform/fallback.rs` names
what the other platforms want instead. See [PORTING.md](PORTING.md).

**Bugs in detection.** A session that reads as finished while the agent is
still working is the failure that matters most here, so those reports are
the most valuable ones. Bring a recording if you can, and the section
below says how to make one.

## Getting set up

You need Rust (stable), Node.js 20 or newer, and on macOS the Xcode
command line tools.

```sh
npm install                  # the Tauri CLI
npm install --prefix app/ui  # the frontend
npm run tauri dev            # build and run
cargo test                   # the tests
```

The tests need no frontend build. They spawn real shells in real PTYs and
replay recorded terminal transcripts, which is slower than mocking it and
a great deal more honest.

## Layout, and the one rule

```
crates/core   PTY sessions, agent state detection, window rules
app/src       Tauri setup, native window behaviour, the PTY bridge
app/ui        the xterm.js terminal, the tab rail and the collapsed bar
```

**`crates/core` has no UI and no platform dependencies.** It is the part a
port reuses untouched, so anything needing an OS window API goes behind
the `PlatformWindow` trait in `app/src/platform/` and nowhere else. CI
builds the crate on Linux, which is what keeps that from being a promise.

## Adding an agent

OverTerm works out what a session is doing in tiers. Claude Code writes
markers that say exactly what happened, because the app installs hooks
for it. Everything else is read off the screen, which needs no setup from
anyone and is a guess.

A profile is what makes that guess a good one for a particular tool. It
goes in `~/.config/overterm/config.toml`:

```toml
[[agents]]
match = "gemini"
label = "Gemini"
color = "#4285f4"
busy_pattern = "esc to cancel"
```

`match` is looked for in the path of the program running in the session
and in its arguments, so it finds a tool whether it is a binary of its own
or a script run through node.

`busy_pattern` is the field that earns its place. Between batches of
output an agent's cursor sits back in its input box and the screen looks
exactly like a finished prompt, so with nothing holding the state the
window decides the turn is over and comes back in the middle of an
answer. The pattern is whatever the tool keeps on screen for as long as it
is working. Claude Code shows `esc to interrupt` and Gemini CLI shows
`esc to cancel`.

Leaving `busy_pattern` out is fine and is what most built-in profiles do.
An empty one falls back to the default. A wrong one is worse than none,
because it reads as support while the window quietly ends every turn
early.

### Get the pattern by recording it

Do not take it from the tool's documentation, and do not ask a model for
it. Every detection pattern in this project that was guessed turned out to
be wrong, and the recordings are what caught each one.

Record a real session:

```sh
OVERTERM_CAPTURE=/tmp/session npm run tauri dev
```

Each session writes `/tmp/session-<id>.ndjson`, holding every byte in
either direction with the time it arrived. Run your agent, give it
something slow enough to watch, then quit.

Read it back through the detector:

```sh
cargo run --example replay /tmp/session-<id>.ndjson
```

It prints every state change and what caused it. An agent with no profile
will often show a turn finishing several times during one answer, which is
exactly what a busy pattern fixes.

The files in `crates/core/fixtures/` are recordings kept as tests. Putting
yours beside them with a test in `crates/core/tests/fixtures_replay.rs` is
what stops your profile breaking quietly a year from now.

## Sending a change

- Branch off `main` and name the branch after the work.
- One concern per commit, and each commit should build on its own.
- Imperative subject under 70 characters, then a body saying why. The
  existing log is the house style.
- `cargo fmt --all` and
  `cargo clippy --workspace --all-targets -- -D warnings` both have to be
  clean. CI runs them along with the tests.
- Say how you tested it. For anything touching detection, that means a
  recording rather than an opinion.

A green build proves the code compiles and nothing else. Five separate
features in this repo have shipped dead through a clean build and a clean
lint, so run the app and check the thing you changed actually happens.
