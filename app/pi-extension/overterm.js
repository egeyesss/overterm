// Tells OverTerm what a Pi session is doing.
//
// Installed by the app into ~/.pi/agent/extensions/, where Pi picks up
// any .ts or .js file on its own. Delete it and Pi stops reporting; the
// app will not put it back.
//
// Pi has no way for an extension to hand the terminal a string, the way
// Claude Code's hooks return a `terminalSequence`, so this writes the
// marker itself. Same bytes either way: OSC 777 with the app's own
// module name, which draws nothing anywhere and means something only to
// the terminal this is running in. That is the whole point of putting it
// in the byte stream rather than on a socket. It needs no port, nothing
// to authenticate, and no way to say which session it belongs to, since
// the session it belongs to is the one it arrives on.

import { openSync, writeSync } from "node:fs";

// Has to match crates/core/src/detect/hook.rs. Written out here rather
// than built up, so the app can check the two agree.
const MARKER_PREFIX = "\u001b]777;overterm;";
const MARKER_END = "\u0007";

// Pi runs subagents as their own processes on the same terminal. Their
// turns are not the session's turns, so a subagent reporting would have
// the window coming back every time one of them finished.
const IS_SUBAGENT = Number(process.env.PI_SUBAGENT_DEPTH || "0") > 0;

// The terminal rather than stdout. Pi owns stdout and draws its interface
// through it, and in the modes without an interface it may not be a
// terminal at all.
let tty = null;
if (!IS_SUBAGENT) {
  try {
    tty = openSync("/dev/tty", "w");
  } catch {
    // No controlling terminal, so there is no OverTerm to tell. Every
    // other mode Pi runs in lands here and quietly does nothing.
  }
}

function report(event) {
  if (tty === null) return;
  try {
    // One call, so the marker cannot be cut in half and land inside one
    // of Pi's own escape sequences. It draws nothing, so arriving in the
    // middle of a frame is otherwise harmless.
    writeSync(tty, `${MARKER_PREFIX}${event}${MARKER_END}`);
  } catch {
    // A terminal that has gone away is not worth breaking a session for.
  }
}

export default function overterm(pi) {
  // Not the `input` event, which also fires for commands like /model
  // that open a selector and never start a turn. The window collapses on
  // a submit, and collapsing over a selector hides the thing the user
  // opened it to use. This fires once per agent run, which is what
  // "handed it a job" actually means.
  pi.on("before_agent_start", () => report("submit"));

  // Settled rather than ended: `agent_end` still has automatic retries,
  // compaction and queued continuations ahead of it, and each one would
  // read as another turn finishing.
  //
  // Pi does not say through this API whether a run ended on an answer or
  // an error, so this is always `stop` and never `stop-failure`. Nothing
  // is lost by that: the two are the same transition on the reading side,
  // and the difference only shows in a debug string.
  pi.on("agent_settled", () => report("stop"));

  // Anything blocking on the user. Pi has no permission event of its own
  // because permission gates are themselves extensions, and every one of
  // them asks through this.
  pi.on("ui_prompt_start", () => report("permission"));
}
