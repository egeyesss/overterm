import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import './style.css';

type AgentState = 'idle' | 'busy' | 'needsInput' | 'done';
type WindowMode = 'bar' | 'panel';

type PtyEvent =
  | { event: 'output'; data: { bytes: number[] } }
  | { event: 'agentStateChanged'; data: { state: AgentState; cause: string } }
  | { event: 'exited'; data: { code: number | null } };

type Cues = { glow: boolean; sound: boolean; notify: boolean };

const term = new Terminal({
  cursorBlink: true,
  fontSize: 13,
  fontFamily: 'Menlo, Monaco, "SF Mono", monospace',
  scrollback: 10_000,
  // Required by the Unicode 11 addon below, which registers a character
  // width provider through an API xterm still calls proposed. Without it
  // loading that addon throws.
  allowProposedApi: true,
  // Option sends ESC rather than composing accented characters, which is
  // what every agent CLI expects. It is also what gets Option+Enter below,
  // and Claude Code's other Option shortcuts, working at all.
  macOptionIsMeta: true,
  theme: {
    background: '#1a1b26',
    foreground: '#c0caf5',
    cursor: '#c0caf5',
  },
});
const fit = new FitAddon();
term.loadAddon(fit);

// Agents draw box-drawing characters and emoji, both of which are two
// cells wide under Unicode 11 and one cell wide under the default table.
// Getting this wrong shears a TUI sideways by a column per glyph.
term.loadAddon(new Unicode11Addon());
term.unicode.activeVersion = '11';

const container = document.getElementById('terminal')!;
term.open(container);

// Agent output arrives in bursts large enough to make the DOM renderer
// stutter. The GPU one keeps up, but its context can be taken away when
// the machine is under memory pressure, and an addon that has lost its
// context renders nothing at all. Dropping it falls back to the renderer
// that was there before.
try {
  const webgl = new WebglAddon();
  webgl.onContextLoss(() => webgl.dispose());
  term.loadAddon(webgl);
} catch {
  // No WebGL in this webview. The default renderer is correct, just slower.
}

fit.fit();
term.focus();

const body = document.body;
const barLine = document.getElementById('bar-line')!;
const barInput = document.getElementById('bar-input') as HTMLInputElement;
const pills = document.querySelectorAll<HTMLElement>('.pill');

let mode: WindowMode = 'panel';
let agentState: AgentState = 'idle';
let stateSince = Date.now();
let lastCause = '';
let sessionId: string | null = null;

const stateLabels: Record<AgentState, string> = {
  idle: 'idle',
  busy: 'busy',
  needsInput: 'needs input',
  done: 'done',
};

function setAgentState(state: AgentState, cause: string) {
  agentState = state;
  stateSince = Date.now();
  lastCause = cause;
  body.classList.remove('state-idle', 'state-busy', 'state-needsInput', 'state-done');
  body.classList.add(`state-${state}`);
  render();
}

function render() {
  const secs = Math.floor((Date.now() - stateSince) / 1000);
  for (const pill of pills) {
    pill.querySelector('.label')!.textContent = stateLabels[agentState];
    pill.querySelector('.elapsed')!.textContent = lastCause
      ? `${lastCause} · ${secs}s`
      : `${secs}s`;
  }
  if (mode === 'bar') {
    barLine.textContent = lastOutputLine();
  }
}
setInterval(render, 1000);

/// The bottom-most line with anything on it, read off what xterm actually
/// rendered so escape sequences are already resolved.
function lastOutputLine(): string {
  const buffer = term.buffer.active;
  const bottom = buffer.baseY + buffer.cursorY;
  for (let row = bottom; row >= 0 && row > bottom - term.rows; row--) {
    const text = buffer.getLine(row)?.translateToString(true).trim();
    if (text) return text;
  }
  return '';
}

// --- window mode -----------------------------------------------------

function applyMode(next: WindowMode) {
  mode = next;
  body.classList.toggle('mode-bar', next === 'bar');
  body.classList.toggle('mode-panel', next === 'panel');
  if (next === 'panel') {
    // The terminal was display:none, so it has to be measured again
    // before xterm can lay anything out.
    requestAnimationFrame(() => fit.fit());
  } else {
    showDraft();
  }
  // Keep typing flowing across an automatic switch, but only when the
  // window already had focus, so nothing is taken from the app the user
  // is actually working in.
  if (document.hasFocus()) {
    if (next === 'panel') term.focus();
    else barInput.focus();
  }
  render();
}

function requestMode(next: WindowMode) {
  invoke('set_window_mode', { mode: next }).catch(() => {});
  if (next === 'panel') term.focus();
  else barInput.focus();
}

for (const button of document.querySelectorAll('.icon.expand')) {
  button.addEventListener('click', () => requestMode('panel'));
}
for (const button of document.querySelectorAll('.icon.collapse')) {
  button.addEventListener('click', () => requestMode('bar'));
}
for (const button of document.querySelectorAll('.icon.hide')) {
  button.addEventListener('click', () => invoke('hide_window').catch(() => {}));
}

listen<{ mode: WindowMode }>('overterm://mode', (event) => applyMode(event.payload.mode));

// --- attention cues --------------------------------------------------

let audio: AudioContext | null = null;

function chime() {
  try {
    audio ??= new AudioContext();
    const now = audio.currentTime;
    const osc = audio.createOscillator();
    const gain = audio.createGain();
    osc.frequency.setValueAtTime(880, now);
    gain.gain.setValueAtTime(0.0001, now);
    gain.gain.exponentialRampToValueAtTime(0.2, now + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + 0.3);
    osc.connect(gain).connect(audio.destination);
    osc.start(now);
    osc.stop(now + 0.32);
  } catch {
    // No audio device, or the webview blocked playback. The glow still
    // did its job.
  }
}

listen<{ active: boolean; cues: Cues }>('overterm://attention', (event) => {
  const { active, cues } = event.payload;
  body.classList.toggle('attention', active && cues.glow);
  body.classList.toggle('attention-needs', active && agentState === 'needsInput');
  if (active && cues.sound) chime();
});

// Any interaction means the user has seen it.
for (const clear of ['click', 'keydown'] as const) {
  window.addEventListener(clear, () => body.classList.remove('attention'));
}

// --- session ---------------------------------------------------------

/// What a terminal has to send so an agent CLI inserts a line break
/// instead of submitting.
///
/// Enter and Shift+Enter are the same byte in a plain terminal, so a CLI
/// cannot tell them apart unless the terminal says so. ESC followed by a
/// carriage return is the sequence Claude Code accepts for this, and the
/// one its own editor integrations are configured to send. Option+Enter
/// arrives here too, because `macOptionIsMeta` prefixes ESC.
const NEWLINE = '\x1b\r';

/// Best-effort mirror of what the user has typed since the last submit.
///
/// The real buffer belongs to whatever is running in the PTY and cannot be
/// read back, so this follows the keystrokes we sent it. It is what lets a
/// prompt started in the full terminal show up in the bar, and the other
/// way round: both views send the same keystrokes to the same place, so
/// the bar shows a copy rather than a second draft.
let pending = '';

function write(data: string) {
  if (sessionId) invoke('write_pty', { sessionId, data }).catch(() => {});
  track(data);
}

function track(data: string) {
  // A line break grows the draft. Checked before the escape guard below,
  // which would otherwise drop it for starting with ESC.
  if (data === NEWLINE) {
    pending += '\n';
    showDraft();
    return;
  }
  // Arrow keys, history recall and the like move a cursor inside the
  // program that this side cannot see. Leave the mirror alone rather than
  // let it drift.
  if (data.startsWith('\x1b')) return;
  for (const ch of data) {
    if (ch === '\r' || ch === '\x03' || ch === '\x15') {
      pending = '';
    } else if (ch === '\n') {
      pending += '\n'; // Ctrl+J, the line break that needs no setup
    } else if (ch === '\x7f' || ch === '\b') {
      pending = pending.slice(0, -1);
    } else if (ch >= ' ') {
      pending += ch;
    }
  }
  showDraft();
}

/// The bar is one line tall, so a draft with breaks in it is shown with
/// them marked rather than flattened away.
function showDraft() {
  barInput.value = pending.replace(/\n/g, ' \u21b5 ');
}

// The bar input forwards keystrokes rather than submitting its contents.
// What is showing is already in the program's own buffer, so sending the
// whole box on Enter would send it twice.
barInput.addEventListener('keydown', (event) => {
  if (event.metaKey || event.altKey) return;
  if (event.ctrlKey) {
    if (event.key === 'c') write('\x03');
    else if (event.key === 'u') write('\x15');
    // The line break that works in every terminal with no setup, and the
    // only one that reached the session from here before.
    else if (event.key === 'j') write('\n');
    else return;
  } else if (event.key === 'Enter') {
    // Same split the full terminal makes: shift means a line break.
    write(event.shiftKey ? NEWLINE : '\r');
  } else if (event.key === 'Escape') {
    // How you interrupt Claude Code, and the bar is exactly where you
    // are when you want to. It changes no text, so the draft mirror
    // above is left alone.
    write('\x1b');
  } else if (event.key === 'Backspace') {
    write('\x7f');
  } else if (event.key.length === 1) {
    write(event.key);
  } else {
    return; // arrows, tab, function keys: not modelled
  }
  event.preventDefault();
});

barInput.addEventListener('paste', (event) => {
  event.preventDefault();
  const text = event.clipboardData?.getData('text');
  if (text) write(text);
});

async function start() {
  const onEvent = new Channel<PtyEvent>();
  onEvent.onmessage = (msg) => {
    if (msg.event === 'output') {
      term.write(new Uint8Array(msg.data.bytes));
    } else if (msg.event === 'agentStateChanged') {
      setAgentState(msg.data.state, msg.data.cause);
    } else {
      const code = msg.data.code ?? 'signal';
      term.write(`\r\n\x1b[90m[process exited: ${code}]\x1b[0m\r\n`);
    }
  };

  sessionId = await invoke<string>('spawn_session', {
    cols: term.cols,
    rows: term.rows,
    onEvent,
  });

  // xterm.js ignores the modifier on Enter and sends a bare carriage
  // return, so a shifted one submits. Returning false stops it handling
  // the key at all, leaving the sequence above as the only thing sent.
  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== 'keydown') return true;
    if (event.key === 'Enter' && event.shiftKey && !event.ctrlKey && !event.metaKey) {
      write(NEWLINE);
      return false;
    }
    return true;
  });

  term.onData(write);
  term.onResize(({ cols, rows }) => {
    invoke('resize_pty', { sessionId, cols, rows }).catch(() => {});
  });

  applyMode(await invoke<WindowMode>('window_mode'));
}

// Refit on any container size change; onResize above forwards it to the
// PTY. Skipped in bar mode, where the hidden terminal measures as zero and
// would reflow the agent's TUI down to nothing.
let refitQueued = false;
new ResizeObserver(() => {
  if (refitQueued || mode !== 'panel') return;
  refitQueued = true;
  requestAnimationFrame(() => {
    refitQueued = false;
    if (container.clientHeight > 0 && container.clientWidth > 0) fit.fit();
  });
}).observe(container);

start().catch((err) => {
  term.write(`\r\n\x1b[31mfailed to start session: ${err}\x1b[0m\r\n`);
});
