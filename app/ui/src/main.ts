import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { openUrl } from '@tauri-apps/plugin-opener';
import { SearchAddon } from '@xterm/addon-search';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon } from '@xterm/addon-web-links';
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

/// Mirrors the Rust struct, so the keys are snake_case the whole way
/// through: what the interface shows and what somebody reads in
/// config.toml are then the same names.
type Settings = {
  claude_hooks_installed: boolean;
  opacity: number;
  window: {
    collapse_on_submit: boolean;
    collapse_delay_ms: number;
    expand_when_wanted: boolean;
    reveal_when_stalled_ms: number;
  };
  cues: Cues;
  terminal: {
    font_family: string;
    font_size: number;
    scrollback: number;
  };
};

/// Font sizes the zoom shortcuts step between, smallest to largest.
const FONT_SIZES = [9, 10, 11, 12, 13, 14, 16, 18, 20, 24];
const DEFAULT_FONT_SIZE = 13;

const term = new Terminal({
  cursorBlink: true,
  fontSize: DEFAULT_FONT_SIZE,
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

const search = new SearchAddon();
term.loadAddon(search);

// Agents print localhost addresses, pull request links and doc URLs all
// day. The handler is ours because the default one calls window.open,
// which in a webview navigates the app away from itself.
term.loadAddon(
  new WebLinksAddon((_event, uri) => {
    // Reported rather than swallowed. A link that cannot be opened is
    // worth knowing about, and a silent catch here is what hid the URL
    // scope being missing from the app's permissions.
    openUrl(uri).catch((err) => console.error(`could not open ${uri}:`, err));
  }),
);

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
    // Nothing to search once the terminal is off screen, and leaving it
    // open means it comes back with a stale query on the next expand.
    if (!findBox.hidden) closeFind();
    if (!settingsSheet.hidden) closeSettings();
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

// Escape reaches the find bar even when the terminal has focus, which is
// where you are when you opened it and found what you wanted.
window.addEventListener('keydown', (event) => {
  if (event.key !== 'Escape') return;
  if (!settingsSheet.hidden) closeSettings();
  else if (!findBox.hidden) closeFind();
});

// --- terminal shortcuts ----------------------------------------------

/// Resize the font a step at a time, then tell the PTY its new size.
///
/// The terminal is measured in cells, so a bigger font means fewer
/// columns. Refitting is what tells the program on the other end to
/// redraw itself narrower instead of wrapping at the old width.
function zoom(steps: number) {
  const size = term.options.fontSize ?? DEFAULT_FONT_SIZE;
  // Nearest step rather than an exact match, so a size typed into the
  // settings sheet still zooms from where it is instead of jumping.
  let from = 0;
  for (let i = 1; i < FONT_SIZES.length; i++) {
    if (Math.abs(FONT_SIZES[i] - size) < Math.abs(FONT_SIZES[from] - size)) from = i;
  }
  const next = Math.min(Math.max(from + steps, 0), FONT_SIZES.length - 1);
  setFontSize(FONT_SIZES[next]);
}

function resetZoom() {
  setFontSize(DEFAULT_FONT_SIZE);
}

/// Resize the terminal and remember it, so a zoom survives a relaunch
/// rather than being undone by the stored size on the next start.
function setFontSize(size: number) {
  term.options.fontSize = size;
  fit.fit();
  if (settings && settings.terminal.font_size !== size) {
    settings = { ...settings, terminal: { ...settings.terminal, font_size: size } };
    persist(settings);
  }
}

/// Copy the selection, and report whether there was one to copy.
///
/// The terminal draws to a canvas, so there is no selected text for the
/// webview to copy on its own. Without this, Cmd+C over a selection does
/// nothing at all.
function copySelection(): boolean {
  const selection = term.getSelection();
  if (!selection) return false;
  navigator.clipboard.writeText(selection).catch(() => {});
  return true;
}

/// Read the clipboard and send it to the session.
///
/// Only used by the right-click below. Cmd+V is left alone, because the
/// terminal already receives a real paste event and handling it here as
/// well would send the clipboard twice.
function paste() {
  navigator.clipboard
    .readText()
    .then((text) => {
      if (text) write(text);
    })
    .catch(() => {});
}

// Right-click pastes, the way it does in a terminal rather than the way
// it does in a browser.
container.addEventListener('contextmenu', (event) => {
  event.preventDefault();
  paste();
});

// --- find ------------------------------------------------------------

const findBox = document.getElementById('find')!;
const findInput = document.getElementById('find-input') as HTMLInputElement;
const findCount = document.getElementById('find-count')!;

// Matches have to be painted by the addon: the terminal is a canvas, so
// there is no text node to highlight. Colours come from the same palette
// as the rest of the chrome.
const findDecorations = {
  matchBackground: '#3d59a1',
  matchBorder: '#3d59a1',
  matchOverviewRuler: '#3d59a1',
  activeMatchBackground: '#e0af68',
  activeMatchBorder: '#e0af68',
  activeMatchColorOverviewRuler: '#e0af68',
};

search.onDidChangeResults(({ resultIndex, resultCount }) => {
  findCount.textContent = resultCount ? `${resultIndex + 1}/${resultCount}` : 'none';
  findBox.classList.toggle('no-matches', findInput.value !== '' && resultCount === 0);
});

function runFind(direction: 'next' | 'previous') {
  const query = findInput.value;
  if (!query) {
    search.clearDecorations();
    findCount.textContent = '';
    findBox.classList.remove('no-matches');
    return;
  }
  const options = { decorations: findDecorations };
  if (direction === 'next') search.findNext(query, options);
  else search.findPrevious(query, options);
}

function openFind() {
  if (mode !== 'panel') return; // the terminal is not on screen in the bar
  findBox.hidden = false;
  findInput.select();
  findInput.focus();
}

function closeFind() {
  findBox.hidden = true;
  search.clearDecorations();
  findBox.classList.remove('no-matches');
  term.focus();
}

findInput.addEventListener('input', () => runFind('next'));

findInput.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') {
    runFind(event.shiftKey ? 'previous' : 'next');
  } else if (event.key === 'Escape') {
    closeFind();
  } else {
    return;
  }
  event.preventDefault();
});

findBox.querySelector('.find-next')!.addEventListener('click', () => runFind('next'));
findBox.querySelector('.find-prev')!.addEventListener('click', () => runFind('previous'));
findBox.querySelector('.find-close')!.addEventListener('click', closeFind);

// --- settings --------------------------------------------------------

const settingsSheet = document.getElementById('settings')!;
const settingsNote = document.getElementById('settings-note')!;

/// Last known stored settings. The sheet edits a copy of this and sends
/// the whole thing back, so a field the interface does not show yet is
/// carried through untouched rather than dropped.
let settings: Settings | null = null;

const field = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const collapseOnSubmit = field<HTMLInputElement>('collapse-on-submit');
const collapseDelay = field<HTMLInputElement>('collapse-delay');
const expandWhenWanted = field<HTMLInputElement>('expand-when-wanted');
const revealStalled = field<HTMLInputElement>('reveal-stalled');
const cueGlow = field<HTMLInputElement>('cue-glow');
const cueSound = field<HTMLInputElement>('cue-sound');
const fontFamily = field<HTMLInputElement>('font-family');
const fontSize = field<HTMLInputElement>('font-size');
const scrollback = field<HTMLInputElement>('scrollback');
const opacity = field<HTMLInputElement>('opacity');
const opacityValue = field<HTMLElement>('opacity-value');

function showSettings(current: Settings) {
  settings = current;
  collapseOnSubmit.checked = current.window.collapse_on_submit;
  collapseDelay.value = String(current.window.collapse_delay_ms);
  expandWhenWanted.checked = current.window.expand_when_wanted;
  revealStalled.value = String(current.window.reveal_when_stalled_ms);
  cueGlow.checked = current.cues.glow;
  cueSound.checked = current.cues.sound;
  fontFamily.value = current.terminal.font_family;
  fontSize.value = String(current.terminal.font_size);
  scrollback.value = String(current.terminal.scrollback);
  opacity.value = String(current.opacity);
  opacityValue.textContent = `${current.opacity}%`;
  applyTerminalSettings(current);
  // The delay only means anything if the window collapses at all.
  collapseDelay.closest('.row')!.classList.toggle('inactive', !current.window.collapse_on_submit);
}

/// Put the stored terminal preferences on the terminal itself.
///
/// Changing the font changes how wide a cell is, so the terminal has to
/// be measured again and the new size passed to the program running in
/// it. Without that it keeps drawing to the old width and wraps.
function applyTerminalSettings(current: Settings) {
  term.options.fontFamily = current.terminal.font_family;
  term.options.fontSize = current.terminal.font_size;
  term.options.scrollback = current.terminal.scrollback;
  if (mode === 'panel') fit.fit();
}

/// Send the whole settings object and adopt whatever comes back.
///
/// The reply is the stored version rather than what was sent, because the
/// backend clamps some values. Taking its word keeps the interface honest
/// about what is actually in effect.
function saveSettings() {
  if (!settings) return;
  const next: Settings = {
    ...settings,
    opacity: Number(opacity.value),
    window: {
      collapse_on_submit: collapseOnSubmit.checked,
      collapse_delay_ms: Number(collapseDelay.value) || 0,
      expand_when_wanted: expandWhenWanted.checked,
      reveal_when_stalled_ms: Number(revealStalled.value) || 0,
    },
    cues: { ...settings.cues, glow: cueGlow.checked, sound: cueSound.checked },
    terminal: {
      font_family: fontFamily.value.trim() || settings.terminal.font_family,
      font_size: Number(fontSize.value) || settings.terminal.font_size,
      scrollback: Number(scrollback.value) || 0,
    },
  };
  persist(next);
}

function persist(next: Settings) {
  invoke<Settings>('save_settings', { settings: next })
    .then((stored) => {
      settingsNote.textContent = '';
      settingsNote.classList.remove('failed');
      showSettings(stored);
    })
    .catch((err) => {
      settingsNote.textContent = `Could not save: ${err}`;
      settingsNote.classList.add('failed');
    });
}

// The opacity slider fires continuously while dragging, so show the
// number every time and only write the file once the drag settles.
opacity.addEventListener('input', () => {
  opacityValue.textContent = `${opacity.value}%`;
});
opacity.addEventListener('change', saveSettings);

for (const input of [collapseOnSubmit, expandWhenWanted, cueGlow, cueSound]) {
  input.addEventListener('change', saveSettings);
}
for (const input of [collapseDelay, revealStalled, fontSize, scrollback, fontFamily]) {
  input.addEventListener('change', saveSettings);
}

function openSettings() {
  if (mode !== 'panel') return; // the sheet has no room in the bar
  // Read on every open rather than trusting the copy in memory, so a
  // file edited by hand between openings is not silently overwritten.
  invoke<Settings>('settings')
    .then(showSettings)
    .catch((err) => {
      settingsNote.textContent = `Could not read the settings: ${err}`;
      settingsNote.classList.add('failed');
    });
  settingsSheet.hidden = false;
}

function closeSettings() {
  settingsSheet.hidden = true;
  term.focus();
}

for (const button of document.querySelectorAll('.icon.settings-open')) {
  button.addEventListener('click', openSettings);
}
settingsSheet.querySelector('.settings-close')!.addEventListener('click', closeSettings);
// Clicking the dimmed area outside the sheet closes it.
settingsSheet.addEventListener('click', (event) => {
  if (event.target === settingsSheet) closeSettings();
});

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
  // Before the session spawns: the terminal reports its size to the PTY
  // on spawn, and that size depends on the font.
  try {
    showSettings(await invoke<Settings>('settings'));
  } catch {
    // The defaults the terminal was built with are already correct.
  }

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

  // Keys handled here rather than by the terminal.
  //
  // Returning false is not enough on its own. xterm leaves the browser's
  // default alone when this handler refuses a key, so the keypress event
  // still fires, and its own keypress path asks this handler again with
  // an event that is not a keydown. Waving that one through means the
  // character gets sent after all: a shifted enter sent the line break
  // and then a bare carriage return, and the carriage return submitted.
  // Cancelling the keydown stops the keypress happening at all.
  const handled = (event: KeyboardEvent) => {
    event.preventDefault();
    return false;
  };

  term.attachCustomKeyEventHandler((event) => {
    if (event.type !== 'keydown') return true;
    if (event.key === 'Enter' && event.shiftKey && !event.ctrlKey && !event.metaKey) {
      write(NEWLINE);
      return handled(event);
    }
    if (!event.metaKey) return true;
    switch (event.key) {
      case 'f':
        openFind();
        return handled(event);
      case ',':
        openSettings();
        return handled(event);
      case 'c':
        // Nothing selected means nothing to copy, so let the key through
        // rather than swallow it.
        return copySelection() ? handled(event) : true;
      case 'k':
        term.clear();
        return handled(event);
      case '=':
      case '+':
        zoom(1);
        return handled(event);
      case '-':
        zoom(-1);
        return handled(event);
      case '0':
        resetZoom();
        return handled(event);
      default:
        return true;
    }
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
