import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';

/// The eight directions a window edge can be dragged in, as the window
/// API names them. Mirrored here because the type is not exported.
type ResizeDirection =
  | 'North'
  | 'South'
  | 'East'
  | 'West'
  | 'NorthEast'
  | 'NorthWest'
  | 'SouthEast'
  | 'SouthWest';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { openUrl } from '@tauri-apps/plugin-opener';
// Raw so the markup can go straight into a tab. These are each a single
// <svg> with a viewBox and no fixed size, so CSS decides how big it is.
import claudeMark from './brand/claude.svg?raw';
import codexMark from './brand/codex.svg?raw';
import geminiMark from './brand/gemini.svg?raw';
import kimiMark from './brand/kimi.svg?raw';
import ollamaMark from './brand/ollama.svg?raw';
import piMark from './brand/pi.svg?raw';
// A bitmap, so this one is a URL rather than markup.
import antigravityMark from './brand/antigravity.png';
import { SearchAddon } from '@xterm/addon-search';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { WebglAddon } from '@xterm/addon-webgl';
import '@xterm/xterm/css/xterm.css';
import './style.css';
import { CodexChat, shortPath, type ThreadView } from './codex';

type AgentState = 'idle' | 'busy' | 'needsInput' | 'done';
type WindowMode = 'bar' | 'panel';

type PtyEvent =
  | { event: 'output'; data: { bytes: number[] } }
  | { event: 'agentStateChanged'; data: { state: AgentState; cause: string } }
  | { event: 'exited'; data: { code: number | null } }
  | {
      event: 'agentChanged';
      data: { agent: { id: string; label: string; icon: string | null; color: string | null; cwd: string | null } };
    };

type Cues = { glow: boolean; sound: boolean };
type Theme = 'light' | 'dark' | 'system';

/// What the backend knows about releases. Worked out there, because the
/// comparison has to treat 1.0.10 as newer than 1.0.9 and that is a rule
/// worth having tests for.
type UpdateStatus = {
  current: string;
  latest: string;
  available: boolean;
  dismissed: boolean;
  homebrew: boolean;
  installable: boolean;
  releaseUrl: string;
};

/// Mirrors the Rust struct, so the keys are snake_case the whole way
/// through: what the interface shows and what somebody reads in
/// config.toml are then the same names.
type Settings = {
  claude_hooks_installed: boolean;
  claude_hooks_notice_seen: boolean;
  pi_extension_installed: boolean;
  opacity: number;
  hotkey: string;
  theme: Theme;
  check_for_updates: boolean;
  latest_version: string;
  last_update_check: number;
  update_notice_seen: string;
  window: {
    collapse_on_submit: boolean;
    collapse_delay_ms: number;
    expand_when_wanted: boolean;
    reveal_when_stalled_ms: number;
    panel_width: number;
    panel_height: number;
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

/// One tab in the window, with everything that belongs to it.
///
/// Every field here used to be a module-level variable, which is exactly
/// why the window could only hold one session.
type SessionBase = {
  id: string;
  pane: HTMLDivElement;
  tab: HTMLButtonElement;
  state: AgentState;
  stateSince: number;
  lastCause: string;
  /// How long the previous turn took, so the bar can say what the last
  /// one cost as well as what this one is costing.
  lastTurnMs: number | null;
  /// Where this session currently is, or null when it could not be read.
  cwd: string | null;
  /// Best-effort mirror of what the user has typed since the last submit.
  ///
  /// The real buffer belongs to whatever is running in the PTY and cannot
  /// be read back, so this follows the keystrokes we sent it. It is what
  /// lets a prompt started in the full terminal show up in the bar, and
  /// the other way round. Per session, because two agents have two
  /// half-written prompts. A Codex tab keeps its composer's text here.
  pending: string;
};

type TerminalSession = SessionBase & {
  kind: 'terminal';
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
};

/// A thread owned by the Codex desktop app, shown as a chat rather than a
/// terminal. Everything about the window treats it like any other tab.
type CodexSession = SessionBase & {
  kind: 'codex';
  threadId: string;
  chat: CodexChat;
};

type Session = TerminalSession | CodexSession;

const sessions: Session[] = [];
/// The session on screen. Null only before the first one has started.
let active: Session | null = null;

const terminalsEl = document.getElementById('terminals') as HTMLDivElement;
const tabsEl = document.getElementById('tabs') as HTMLElement;
const tabAdd = document.getElementById('tab-add') as HTMLButtonElement;

function terminals(): TerminalSession[] {
  return sessions.filter((s): s is TerminalSession => s.kind === 'terminal');
}

/// The active tab if it is a terminal. Find, zoom, copy and clear only mean
/// something there.
function activeTerminal(): TerminalSession | null {
  return active?.kind === 'terminal' ? active : null;
}

function fitActive() {
  activeTerminal()?.fit.fit();
}

function focusActive() {
  if (active?.kind === 'codex') active.chat.focus();
  else active?.term.focus();
}

/// Read one palette value out of the stylesheet.
///
/// The terminal is drawn by xterm and the chrome around it by CSS, and
/// they have to agree. Keeping the colours in both places means the first
/// person to change one of them silently splits the window in half, so the
/// stylesheet is the only copy and this reads it back.
///
/// An empty answer means the token is missing or the stylesheet has not
/// arrived. Say so rather than substituting a colour, because a second
/// colour written here is the thing being removed.
function token(name: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  if (!value) console.error(`[theme] ${name} is not defined in the stylesheet`);
  return value;
}

function newTerminal(): { term: Terminal; fit: FitAddon; search: SearchAddon } {
  const term = new Terminal({
    cursorBlink: true,
    fontSize: settings?.terminal.font_size ?? DEFAULT_FONT_SIZE,
    fontFamily: settings?.terminal.font_family ?? "'IBM Plex Mono', Menlo, Monaco, monospace",
    scrollback: settings?.terminal.scrollback ?? 10_000,
    // Required by the Unicode 11 addon below, which registers a character
    // width provider through an API xterm still calls proposed. Without it
    // loading that addon throws.
    allowProposedApi: true,
    // Option sends ESC rather than composing accented characters, which is
    // what every agent CLI expects. It is also what gets Option+Enter
    // working at all, and Claude Code's other Option shortcuts.
    macOptionIsMeta: true,
    theme: {
      background: token('--body'),
      foreground: token('--ink'),
      cursor: token('--ink'),
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
    // No WebGL in this webview. The default renderer is correct, just
    // slower.
  }

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

  return { term, fit, search };
}

const body = document.body;
const barInput = document.getElementById('bar-input') as HTMLInputElement;
const pills = document.querySelectorAll<HTMLElement>('.pill');
const barCwd = document.getElementById('bar-cwd')!;
const elapsedFields = document.querySelectorAll<HTMLElement>('.elapsed');

let mode: WindowMode = 'panel';

const stateLabels: Record<AgentState, string> = {
  idle: 'idle',
  busy: 'busy',
  needsInput: 'needs input',
  done: 'done',
};

function setAgentState(session: Session, state: AgentState, cause: string) {
  // A turn that just ended is worth remembering the length of.
  if (session.state === 'busy' && state !== 'busy') {
    session.lastTurnMs = Date.now() - session.stateSince;
  }
  session.state = state;
  session.stateSince = Date.now();
  session.lastCause = cause;
  session.tab.classList.remove('state-idle', 'state-busy', 'state-needsInput', 'state-done');
  session.tab.classList.add(`state-${state}`);
  render();
}

function elapsed(ms: number): string {
  const secs = Math.floor(ms / 1000);
  return secs < 60 ? `${secs}s` : `${Math.floor(secs / 60)}m ${secs % 60}s`;
}


function render() {
  // The pill speaks for the window, not for one session: with several
  // running, the useful thing to know is how many are still going.
  const busy = sessions.filter((s) => s.state === 'busy').length;
  const label =
    sessions.length > 1 && busy > 0 ? `${busy} busy` : stateLabels[active?.state ?? 'idle'];

  for (const pill of pills) {
    pill.querySelector('.label')!.textContent = label;
  }
  for (const field of elapsedFields) {
    field.textContent = active ? timings(active) : '';
  }
  body.classList.remove('state-idle', 'state-busy', 'state-needsInput', 'state-done');
  body.classList.add(`state-${active?.state ?? 'idle'}`);

  barCwd.textContent = active?.cwd ?? '';
}
setInterval(render, 1000);

function timings(session: Session): string {
  const now = elapsed(Date.now() - session.stateSince);
  return session.lastTurnMs === null ? now : `${now} / last ${elapsed(session.lastTurnMs)}`;
}


// --- window mode -----------------------------------------------------

function applyMode(next: WindowMode) {
  mode = next;
  body.classList.toggle('mode-bar', next === 'bar');
  body.classList.toggle('mode-panel', next === 'panel');
  if (next === 'panel') {
    // The pane was display:none, so it has to be measured again before
    // xterm can lay anything out.
    requestAnimationFrame(fitActive);
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
    if (next === 'panel') focusActive();
    else barInput.focus();
  }
  render();
}

function requestMode(next: WindowMode) {
  invoke('set_window_mode', { mode: next }).catch(() => {});
  if (next === 'panel') focusActive();
  else barInput.focus();
}

for (const button of document.querySelectorAll('.icon.expand')) {
  button.addEventListener('click', () => requestMode('panel'));
}
for (const button of document.querySelectorAll('.icon.collapse')) {
  button.addEventListener('click', () => requestMode('bar'));
}
// Ends the process rather than hiding the window. With no Dock icon
// there would be nothing to bring a hidden one back from, so a running
// process behind an invisible window would look like a failed quit while
// still holding the summon shortcut.
for (const button of document.querySelectorAll('.icon.close')) {
  button.addEventListener('click', () => {
    invoke('quit_app').catch((err) => console.error('[quit] refused:', err));
  });
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
  else if (!codexPicker.hidden) closeCodexPicker();
  else if (!findBox.hidden) closeFind();
});

// --- terminal shortcuts ----------------------------------------------

/// Resize the font a step at a time, then tell the PTY its new size.
///
/// The terminal is measured in cells, so a bigger font means fewer
/// columns. Refitting is what tells the program on the other end to
/// redraw itself narrower instead of wrapping at the old width.
function zoom(steps: number) {
  const size = settings?.terminal.font_size ?? DEFAULT_FONT_SIZE;
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
  for (const session of terminals()) {
    session.term.options.fontSize = size;
  }
  document.documentElement.style.setProperty('--chat-font-size', `${size}px`);
  fitActive();
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
  const selection = activeTerminal()?.term.getSelection();
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
      if (text) writeActive(text);
    })
    .catch(() => {});
}

// Right-click pastes, the way it does in a terminal rather than the way
// it does in a browser.
terminalsEl.addEventListener('contextmenu', (event: MouseEvent) => {
  // A chat is ordinary text, so it keeps the webview's own menu.
  if (!activeTerminal()) return;
  event.preventDefault();
  paste();
});

// --- find ------------------------------------------------------------

const findBox = document.getElementById('find')!;
const findInput = document.getElementById('find-input') as HTMLInputElement;
const findCount = document.getElementById('find-count')!;

// Matches have to be painted by the addon: the terminal is a canvas, so
// there is no text node to highlight. Colours come from the same palette
// as the rest of the chrome, which means reading them rather than
// repeating them.
//
// Built on demand rather than once at module scope, because reading a
// custom property before the stylesheet is applied returns nothing.
function findDecorations() {
  const match = token('--match');
  const active = token('--needs-hex');
  return {
    matchBackground: match,
    matchBorder: match,
    matchOverviewRuler: match,
    activeMatchBackground: active,
    activeMatchBorder: active,
    activeMatchColorOverviewRuler: active,
  };
}

function runFind(direction: 'next' | 'previous') {
  const search = activeTerminal()?.search;
  if (!search) return;
  const query = findInput.value;
  if (!query) {
    search.clearDecorations();
    findCount.textContent = '';
    findBox.classList.remove('no-matches');
    return;
  }
  const options = { decorations: findDecorations() };
  if (direction === 'next') search.findNext(query, options);
  else search.findPrevious(query, options);
}

function openFind() {
  if (mode !== 'panel') return; // the terminal is not on screen in the bar
  if (!activeTerminal()) return; // a chat has no canvas to search
  findBox.hidden = false;
  findInput.select();
  findInput.focus();
}

function closeFind() {
  findBox.hidden = true;
  activeTerminal()?.search.clearDecorations();
  findBox.classList.remove('no-matches');
  focusActive();
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

// A window with no system frame gets no resize border from the window
// manager, so each edge hands the drag over itself. The direction is on
// the element rather than worked out from the pointer, which keeps the
// corners honest.
for (const grip of document.querySelectorAll<HTMLElement>('.grip')) {
  grip.addEventListener('mousedown', (event) => {
    if (event.button !== 0) return;
    event.preventDefault();
    const direction = grip.dataset.dir;
    if (!direction) return;
    // Refused rather than crashing if the permission is ever missing, and
    // said out loud: a resize that silently does nothing is the kind of
    // thing that survives a whole release.
    getCurrentWindow()
      .startResizeDragging(direction as ResizeDirection)
      .catch((err: unknown) => console.error('[resize] could not start:', err));
  });
}

// --- settings --------------------------------------------------------

const settingsSheet = document.getElementById('settings')!;
const settingsNote = document.getElementById('settings-note')!;
const versionLine = document.getElementById('version')!;

/// Asked for once. It cannot change while the app is running, and a bug
/// report is a great deal more useful with it than without.
invoke<string>('app_version')
  .then((version) => {
    versionLine.textContent = `oTerm ${version}`;
  })
  .catch((err) => {
    // Never swallowed: a version that quietly fails to appear is how
    // nobody notices this stopped working.
    console.error('[version] could not read it:', err);
  });

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
const hotkey = field<HTMLInputElement>('hotkey');
const hotkeyNote = field<HTMLElement>('hotkey-note');
const claudeHooks = field<HTMLInputElement>('claude-hooks');
const hooksNote = field<HTMLElement>('hooks-note');
const piExtension = field<HTMLInputElement>('pi-extension');
const piNote = field<HTMLElement>('pi-note');
const fontFamily = field<HTMLInputElement>('font-family');
const fontSize = field<HTMLInputElement>('font-size');
const scrollback = field<HTMLInputElement>('scrollback');
const opacity = field<HTMLInputElement>('opacity');
const opacityValue = field<HTMLElement>('opacity-value');

const themeButtons = document.querySelectorAll<HTMLButtonElement>('.theme-toggle');
const systemIsDark = window.matchMedia('(prefers-color-scheme: dark)');

/// Which theme is actually on screen, which is not the same as the setting:
/// "system" resolves to whichever the machine is set to.
function effectiveTheme(choice: Theme): 'light' | 'dark' {
  if (choice === 'system') return systemIsDark.matches ? 'dark' : 'light';
  return choice;
}

/// Put the theme on the page and back into every terminal.
///
/// The terminals need doing by hand: xterm reads its colours once when it
/// is built, so a live session keeps the old palette until it is told
/// otherwise, and the window ends up half in each theme.
function applyTheme(choice: Theme) {
  if (choice === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', choice);

  const shown = effectiveTheme(choice);
  // The stylesheet picks the glyph off this: the button offers the theme
  // you are not in, so it shows where you are going rather than where you
  // are.
  document.documentElement.dataset.shown = shown;
  for (const button of themeButtons) {
    button.title =
      choice === 'system'
        ? 'Following the system (click to pick one)'
        : `Switch to ${shown === 'light' ? 'dark' : 'light'} (right-click to follow the system)`;
  }

  for (const session of terminals()) {
    session.term.options.theme = {
      background: token('--body'),
      foreground: token('--ink'),
      cursor: token('--ink'),
    };
  }
}

/// Following the system means tracking it while it changes.
systemIsDark.addEventListener('change', () => {
  if (settings?.theme === 'system') applyTheme('system');
});

for (const button of themeButtons) {
  button.addEventListener('click', () => {
    if (!settings) return;
    const next: Theme = effectiveTheme(settings.theme) === 'light' ? 'dark' : 'light';
    settings = { ...settings, theme: next };
    applyTheme(next);
    persist(settings);
  });
  // The third option is deliberately off the cycle: two clicks should not
  // be able to land you somewhere you cannot get back from.
  button.addEventListener('contextmenu', (event) => {
    event.preventDefault();
    if (!settings) return;
    settings = { ...settings, theme: 'system' };
    applyTheme('system');
    persist(settings);
  });
}

const panelWidth = field<HTMLInputElement>('panel-width');
const panelHeight = field<HTMLInputElement>('panel-height');

/// Put a size the backend settled on back into the two fields.
///
/// It answers with what it applied rather than what was asked for, since
/// a size below the usable minimum is raised and a preset is worked out
/// from the screen.
function showPanelSize(size: { width: number; height: number }) {
  panelWidth.value = String(size.width);
  panelHeight.value = String(size.height);
  if (settings) {
    settings = {
      ...settings,
      window: { ...settings.window, panel_width: size.width, panel_height: size.height },
    };
  }
}

function applyTypedSize() {
  const width = Number(panelWidth.value);
  const height = Number(panelHeight.value);
  if (!width || !height) return;
  invoke<{ width: number; height: number }>('set_panel_size', { width, height })
    .then(showPanelSize)
    .catch((err) => {
      settingsNote.textContent = `Could not resize: ${err}`;
      settingsNote.classList.add('failed');
    });
}

for (const input of [panelWidth, panelHeight]) {
  input.addEventListener('change', applyTypedSize);
}

for (const button of document.querySelectorAll<HTMLButtonElement>('.preset')) {
  button.addEventListener('click', () => {
    invoke<{ width: number; height: number }>('size_preset', { name: button.dataset.size })
      .then(showPanelSize)
      .catch((err) => {
        settingsNote.textContent = `Could not resize: ${err}`;
        settingsNote.classList.add('failed');
      });
  });
}

function showSettings(current: Settings) {
  settings = current;
  applyTheme(current.theme);
  collapseOnSubmit.checked = current.window.collapse_on_submit;
  collapseDelay.value = String(current.window.collapse_delay_ms);
  expandWhenWanted.checked = current.window.expand_when_wanted;
  revealStalled.value = String(current.window.reveal_when_stalled_ms);
  cueGlow.checked = current.cues.glow;
  cueSound.checked = current.cues.sound;
  hotkey.value = current.hotkey;
  fontFamily.value = current.terminal.font_family;
  fontSize.value = String(current.terminal.font_size);
  scrollback.value = String(current.terminal.scrollback);
  panelWidth.value = String(current.window.panel_width);
  panelHeight.value = String(current.window.panel_height);
  opacity.value = String(current.opacity);
  opacityValue.textContent = `${current.opacity}%`;
  showUpdateSettings(current);
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
  for (const session of terminals()) {
    session.term.options.fontFamily = current.terminal.font_family;
    session.term.options.fontSize = current.terminal.font_size;
    session.term.options.scrollback = current.terminal.scrollback;
  }
  document.documentElement.style.setProperty('--chat-font-size', `${current.terminal.font_size}px`);
  if (mode === 'panel') fitActive();
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
    check_for_updates: checkUpdates.checked,
    window: {
      // Spread first: the size lives in here too and is applied through
      // its own command, so rebuilding this object from the checkboxes
      // alone would quietly throw it away.
      ...settings.window,
      collapse_on_submit: collapseOnSubmit.checked,
      collapse_delay_ms: Number(collapseDelay.value) || 0,
      expand_when_wanted: expandWhenWanted.checked,
      reveal_when_stalled_ms: Number(revealStalled.value) || 0,
    },
    cues: { glow: cueGlow.checked, sound: cueSound.checked },
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

/// Modifier names in the order the shortcut parser writes them.
const MODIFIERS = [
  ['metaKey', 'CmdOrCtrl'],
  ['ctrlKey', 'Control'],
  ['altKey', 'Alt'],
  ['shiftKey', 'Shift'],
] as const;

/// Build a chord from a keypress, or null while only modifiers are held.
///
/// Held modifiers arrive as their own keydown events, so the box shows
/// them building up and only commits once a real key lands.
function chordFrom(event: KeyboardEvent): { chord: string; complete: boolean } {
  const parts: string[] = [];
  for (const [flag, name] of MODIFIERS) {
    if (event[flag]) parts.push(name);
  }
  const key = event.key;
  const isModifier = ['Meta', 'Control', 'Alt', 'Shift'].includes(key);
  if (isModifier) return { chord: parts.join('+'), complete: false };
  const named = key.length === 1 ? key.toUpperCase() : key;
  return { chord: [...parts, named].join('+'), complete: parts.length > 0 };
}

hotkey.addEventListener('keydown', (event) => {
  event.preventDefault();
  const { chord, complete } = chordFrom(event);
  hotkey.value = chord;
  if (!complete) {
    hotkeyNote.classList.remove('failed');
    hotkeyNote.textContent = chord ? 'Now press a key.' : 'Hold a modifier first.';
    return;
  }
  invoke<string>('set_hotkey', { hotkey: chord })
    .then((stored) => {
      hotkey.value = stored;
      hotkeyNote.classList.remove('failed');
      hotkeyNote.textContent = 'Saved.';
      if (settings) settings = { ...settings, hotkey: stored };
    })
    .catch((err) => {
      hotkeyNote.textContent = String(err);
      hotkeyNote.classList.add('failed');
      // Put the working chord back, so the box never shows one that is
      // not actually registered.
      if (settings) hotkey.value = settings.hotkey;
    });
});

hotkey.addEventListener('focus', () => {
  hotkeyNote.classList.remove('failed');
  hotkeyNote.textContent = 'Press the chord you want.';
});

hotkey.addEventListener('blur', () => {
  hotkeyNote.textContent = '';
  if (settings) hotkey.value = settings.hotkey;
});

/// Ask whether our entries are in Claude Code's settings file, and say
/// what turning this off and on actually does to it.
///
/// Read from that file rather than from our own record of it, because the
/// user can edit or delete the entries by hand and our record would then
/// be describing a file that no longer says that.
function refreshHooks() {
  invoke<boolean>('hooks_installed')
    .then((installed) => {
      claudeHooks.checked = installed;
      hooksNote.classList.remove('failed');
      hooksNote.textContent = installed
        ? 'Four entries in ~/.claude/settings.json let oTerm read exactly when a turn starts, finishes or asks you something. Turning this off removes them and leaves the rest of that file alone.'
        : 'Not set up. Detection falls back to reading the terminal, which is what every other tool gets. Turning this on adds four entries to ~/.claude/settings.json.';
    })
    .catch((err) => {
      // No Claude Code on this machine is the usual reason, and it is
      // not a failure worth alarming anybody about.
      claudeHooks.checked = false;
      hooksNote.classList.remove('failed');
      hooksNote.textContent = `Claude Code settings not readable here (${err}). Detection still works by reading the terminal.`;
    });
}

claudeHooks.addEventListener('change', () => {
  const command = claudeHooks.checked ? 'install_hooks' : 'uninstall_hooks';
  invoke<boolean>(command)
    .then(refreshHooks)
    .catch((err) => {
      hooksNote.textContent = `Could not change the Claude Code entries: ${err}`;
      hooksNote.classList.add('failed');
      refreshHooks();
    });
});

/// Same idea as the Claude Code toggle above, against a file rather than
/// a set of JSON entries.
function refreshPi() {
  invoke<boolean>('pi_extension_installed')
    .then((installed) => {
      piExtension.checked = installed;
      piNote.classList.remove('failed');
      piNote.textContent = installed
        ? 'An extension in ~/.pi/agent/extensions lets oTerm read exactly when a turn starts, finishes or asks you something. Turning this off deletes it and leaves your other extensions alone.'
        : 'Not set up. Detection falls back to reading the terminal, which is what every other tool gets. Turning this on writes one file to ~/.pi/agent/extensions.';
    })
    .catch((err) => {
      // Usually just means Pi is not installed here.
      piExtension.checked = false;
      piNote.classList.remove('failed');
      piNote.textContent = `Pi's extensions directory not readable here (${err}). Detection still works by reading the terminal.`;
    });
}

piExtension.addEventListener('change', () => {
  const command = piExtension.checked ? 'install_pi_extension' : 'uninstall_pi_extension';
  invoke<boolean>(command)
    .then(refreshPi)
    .catch((err) => {
      piNote.textContent = `Could not change the Pi extension: ${err}`;
      piNote.classList.add('failed');
      refreshPi();
    });
});

// --- updates ---------------------------------------------------------

const updateBox = field<HTMLElement>('update');
const updateLine = field<HTMLElement>('update-line');
const updateWarning = field<HTMLElement>('update-warning');
const updateNow = field<HTMLButtonElement>('update-now');
const updateNotes = field<HTMLButtonElement>('update-notes');
const updateLater = field<HTMLButtonElement>('update-later');
const updateNote = field<HTMLElement>('update-note');
const checkUpdates = field<HTMLInputElement>('check-updates');
const settingsButtons = document.querySelectorAll<HTMLElement>('.icon.settings-open');

/// The last status the backend sent, kept for the link to the release.
let release: UpdateStatus | null = null;

/// Put a check's answer on screen.
///
/// A new release is a dot on the settings control and a line at the top
/// of the sheet, and nothing else. Anything louder than that on every
/// launch would be the opposite of a window that stays out of the way.
function showUpdate(status: UpdateStatus) {
  release = status;
  const notice = status.available && !status.dismissed;
  updateBox.hidden = !notice;
  for (const button of settingsButtons) button.classList.toggle('has-update', notice);
  if (!notice) return;

  updateLine.textContent = `oTerm ${status.latest} is out. This is ${status.current}.`;
  // Homebrew keeps its own record of what it installed, and the script
  // replaces the bundle without telling it, so brew would be left
  // describing a copy it no longer controls.
  const canInstall = status.installable && !status.homebrew;
  updateNow.hidden = !canInstall;
  updateWarning.classList.toggle('warn', canInstall);
  if (canInstall) {
    // Said before the click rather than after it. The update quits the
    // app, and somebody halfway through a session would otherwise watch
    // their window and everything running in it disappear.
    updateWarning.textContent =
      'oTerm closes and restarts to update, and anything running in a session goes with it.';
  } else if (status.homebrew) {
    updateWarning.textContent =
      'Homebrew installed this copy, so it is the one that should update it: brew upgrade --cask overterm';
  } else {
    updateWarning.textContent = 'Install it the way you installed this one.';
  }
}

/// Read what the last check stored. Cheap: it is a file, not a request.
function refreshUpdate() {
  invoke<UpdateStatus>('update_status')
    .then(showUpdate)
    .catch((err) => console.error('[update] could not read the stored check:', err));
}

/// The check itself runs on launch, in the background, and says so when
/// it finishes. Read as well as listened for, since a check that beat the
/// webview to it has already been stored by the time anything is asked.
listen<UpdateStatus>('overterm://update', (event) => showUpdate(event.payload));

updateNow.addEventListener('click', () => {
  invoke<string>('update_command')
    .then(async (command) => {
      // In a session the user can watch rather than hidden behind a
      // progress bar. The app is a terminal: an install it shows is one
      // somebody can see go wrong, and a shell is left behind to act on
      // whatever the script said.
      closeSettings();
      const session = await addSession();
      if (session) write(session, `${command}\r`);
    })
    .catch((err) => {
      settingsNote.textContent = `Could not start the update: ${err}`;
      settingsNote.classList.add('failed');
    });
});

updateNotes.addEventListener('click', () => {
  const url = release?.releaseUrl;
  if (url) openUrl(url).catch((err) => console.error(`could not open ${url}:`, err));
});

updateLater.addEventListener('click', () => {
  // Put away here rather than after the write, so the notice goes when it
  // is clicked even if storing that fails. The worst that costs is being
  // told again next launch.
  updateBox.hidden = true;
  for (const button of settingsButtons) button.classList.remove('has-update');
  invoke('dismiss_update_notice').catch((err) =>
    console.error('[update] could not put the notice away:', err),
  );
});

/// When the stored check happened, in words. Roughly is enough: it is
/// there to say the checking works, not to be read off a clock.
function lastChecked(stamp: number): string {
  if (!stamp) return 'Nothing has been checked yet.';
  const minutes = Math.floor(Date.now() / 60_000 - stamp / 60);
  if (minutes < 1) return 'Checked just now.';
  if (minutes < 60) return `Checked ${minutes} minutes ago.`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `Checked ${hours} hours ago.`;
  return `Checked ${Math.floor(hours / 24)} days ago.`;
}

// Its own listener rather than a place in the list of inputs above:
// that loop runs before this section, so the field is not there yet.
checkUpdates.addEventListener('change', saveSettings);

function showUpdateSettings(current: Settings) {
  checkUpdates.checked = current.check_for_updates;
  updateNote.textContent = current.check_for_updates
    ? `Asks GitHub for the newest release on launch, and at most once every few hours after that. ${lastChecked(current.last_update_check)}`
    : 'Off, so nothing is asked and a new release goes unnoticed.';
}

const firstRun = field<HTMLElement>('first-run');

/// Say once that the app wrote into a config file the user owns.
///
/// It reports itself on stderr, which nobody sees outside a dev build, so
/// on an installed copy this was completely silent.
function maybeShowFirstRun(current: Settings) {
  if (!current.claude_hooks_installed || current.claude_hooks_notice_seen) return;
  firstRun.hidden = false;
}

field<HTMLButtonElement>('first-run-ok').addEventListener('click', () => {
  firstRun.hidden = true;
  invoke('dismiss_hooks_notice').catch(() => {
    // Saying it twice is better than the write failing loudly over a
    // terminal somebody is trying to use.
  });
});

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
  refreshHooks();
  // Read again on every open. A check that finished before the webview
  // was listening would otherwise not show up until the next launch.
  refreshUpdate();
  settingsSheet.hidden = false;
}

function closeSettings() {
  settingsSheet.hidden = true;
  focusActive();
}

for (const button of settingsButtons) {
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

/// Whether any session is waiting on the user, which decides the colour
/// of the cue: the window glows for the whole window, and a session
/// asking a question is more urgent than one that finished.
function wantsInput(): boolean {
  return sessions.some((s) => s.state === 'needsInput');
}

listen<{ active: boolean; cues: Cues }>('overterm://attention', (event) => {
  const { active, cues } = event.payload;
  body.classList.toggle('attention', active && cues.glow);
  body.classList.toggle('attention-needs', active && wantsInput());
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

/// Send bytes to one session's PTY.
///
/// Which session is a parameter rather than "whichever tab is on screen",
/// because not everything that comes through here was typed. A terminal
/// answers device queries by itself: Claude Code asks for the cursor
/// position several times a second and xterm replies to every one, and
/// anything asking about the palette gets an answer the same way. Those
/// come from the tab whose program asked, which is not always the tab
/// being looked at, and sending them to the active one types them into
/// somebody else's shell.
function write(session: TerminalSession, data: string) {
  // A session that has not finished spawning has no id to write to.
  if (!session.id) return;
  invoke('write_pty', { sessionId: session.id, data }).catch(() => {});
  track(session, data);
}

/// Send bytes to the tab on screen. Everything the user types is this,
/// since only the visible terminal can have the keyboard.
function writeActive(data: string) {
  const terminal = activeTerminal();
  if (terminal) write(terminal, data);
}

function track(session: Session, data: string) {
  // A line break grows the draft. Checked before the escape guard below,
  // which would otherwise drop it for starting with ESC.
  if (data === NEWLINE) {
    session.pending += '\n';
    showDraft();
    return;
  }
  // Arrow keys, history recall and the like move a cursor inside the
  // program that this side cannot see. Leave the mirror alone rather than
  // let it drift.
  if (data.startsWith('\x1b')) return;
  for (const ch of data) {
    if (ch === '\r' || ch === '\x03' || ch === '\x15') {
      session.pending = '';
    } else if (ch === '\n') {
      session.pending += '\n'; // Ctrl+J, the line break that needs no setup
    } else if (ch === '\x7f' || ch === '\b') {
      session.pending = session.pending.slice(0, -1);
    } else if (ch >= ' ') {
      session.pending += ch;
    }
  }
  showDraft();
}

/// The bar is one line tall, so a draft with breaks in it is shown with
/// them marked rather than flattened away.
function showDraft() {
  barInput.value = (active?.pending ?? '').replace(/\n/g, BAR_BREAK);
}

/// How a line break in a draft is drawn in the one-line bar.
const BAR_BREAK = ' \u21b5 ';

// The bar input forwards keystrokes rather than submitting its contents.
// What is showing is already in the program's own buffer, so sending the
// whole box on Enter would send it twice.
barInput.addEventListener('keydown', (event) => {
  if (active?.kind === 'codex') {
    codexBarKey(active, event);
    return;
  }
  if (event.metaKey || event.altKey) return;
  if (event.ctrlKey) {
    if (event.key === 'c') writeActive('\x03');
    else if (event.key === 'u') writeActive('\x15');
    // The line break that works in every terminal with no setup, and the
    // only one that reached the session from here before.
    else if (event.key === 'j') writeActive('\n');
    else return;
  } else if (event.key === 'Enter') {
    // Same split the full terminal makes: shift means a line break.
    writeActive(event.shiftKey ? NEWLINE : '\r');
  } else if (event.key === 'Escape') {
    // How you interrupt Claude Code, and the bar is exactly where you
    // are when you want to. It changes no text, so the draft mirror
    // above is left alone.
    writeActive('\x1b');
  } else if (event.key === 'Backspace') {
    writeActive('\x7f');
  } else if (event.key.length === 1) {
    writeActive(event.key);
  } else {
    return; // arrows, tab, function keys: not modelled
  }
  event.preventDefault();
});

/// In a Codex tab the bar is an ordinary text box holding the same draft as
/// the composer: there is no program on the other end reading keystrokes,
/// so the whole message goes when Enter is pressed.
function codexBarKey(session: CodexSession, event: KeyboardEvent) {
  if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
    event.preventDefault();
    void session.chat.submit();
  } else if (event.key === 'Escape') {
    event.preventDefault();
    if (session.chat.isWorking()) interruptCodex(session);
  }
}

barInput.addEventListener('input', () => {
  if (active?.kind !== 'codex') return;
  active.pending = barInput.value.split(BAR_BREAK).join('\n');
  active.chat.draft = active.pending;
});

barInput.addEventListener('paste', (event) => {
  if (active?.kind === 'codex') return; // the text box takes it as typed
  event.preventDefault();
  const text = event.clipboardData?.getData('text');
  if (text) writeActive(text);
});

/// Keys this window handles rather than the terminal.
///
/// Returning false is not enough on its own. xterm leaves the browser's
/// default alone when this handler refuses a key, so the keypress event
/// still fires, and its own keypress path asks this handler again with an
/// event that is not a keydown. Waving that one through means the
/// character gets sent after all: a shifted enter sent the line break and
/// then a bare carriage return, and the carriage return submitted.
/// Cancelling the keydown stops the keypress happening at all.
function claimKey(event: KeyboardEvent) {
  event.preventDefault();
  return false;
}

function handleTerminalKey(event: KeyboardEvent): boolean {
  if (event.type !== 'keydown') return true;
  if (event.key === 'Enter' && event.shiftKey && !event.ctrlKey && !event.metaKey) {
    writeActive(NEWLINE);
    return claimKey(event);
  }
  if (!event.metaKey) return true;
  switch (event.key) {
    case 'f':
      openFind();
      return claimKey(event);
    case ',':
      openSettings();
      return claimKey(event);
    case 't':
      addSession();
      return claimKey(event);
    case 'w':
      if (active) closeSession(active);
      return claimKey(event);
    case 'c':
      // Nothing selected means nothing to copy, so let the key through
      // rather than swallow it.
      return copySelection() ? claimKey(event) : true;
    case 'k':
      activeTerminal()?.term.clear();
      return claimKey(event);
    case '=':
    case '+':
      zoom(1);
      return claimKey(event);
    case '-':
      zoom(-1);
      return claimKey(event);
    case '0':
      resetZoom();
      return claimKey(event);
    default:
      return true;
  }
}

/// Longest text label a tab has room for before it is cut off.
const TAB_LABEL_MAX = 5;

/// Marks for the agents the app recognises, keyed on the program name
/// the backend matched. Keyed on that rather than the label, so renaming
/// an agent in the settings file does not cost it its icon.
///
/// Loaded from the files in ./brand rather than pasted in as strings, so
/// swapping one is dropping a file in. Each carries its own colours,
/// because a brand mark is not always one flat colour. Pi's is the
/// exception and uses currentColor: it is one shape drawn black on light
/// and white on dark, and a fixed fill would leave it invisible against
/// one of the two themes. An agent added through the settings file has no
/// entry here and falls back to a glyph and a colour, which is why both
/// of those exist.
const MARKS: Record<string, string> = {
  claude: claudeMark,
  gemini: geminiMark,
  codex: codexMark,
  kimi: kimiMark,
  ollama: ollamaMark,
  antigravity: antigravityMark,
  pi: piMark,
};

function setLabel(session: Session, agent: { id: string; label: string; icon: string | null; color: string | null; cwd: string | null }) {
  const el = session.tab.querySelector('.label') as HTMLElement;
  const mark = MARKS[agent.id];
  if (mark) {
    // Markup goes in as it is; anything else is a file the bundler gave
    // us a URL for, so it needs an element to hang off.
    el.innerHTML = mark.startsWith('<') ? mark : `<img src="${mark}" alt="" />`;
  } else if (agent.icon) {
    el.textContent = agent.icon;
  } else {
    el.textContent =
      agent.label.length > TAB_LABEL_MAX
        ? `${agent.label.slice(0, TAB_LABEL_MAX - 1)}\u2026`
        : agent.label;
  }
  session.tab.classList.toggle('has-icon', Boolean(mark || agent.icon));
  // The tool's own colour, so the tab reads as the same thing as the
  // terminal underneath it.
  el.style.color = agent.color ?? '';
  session.tab.title = agent.label;
  session.cwd = agent.cwd;
  render();
}

/// Put a session on screen and take the previous one off.
function activate(session: Session) {
  if (active === session) return;
  for (const other of sessions) {
    other.pane.hidden = other !== session;
    other.tab.classList.toggle('active', other === session);
  }
  active = session;
  // The pane was display:none until a moment ago, so it has no measured
  // size yet and fitting it now would reflow the agent's TUI to nothing.
  requestAnimationFrame(() => {
    fitActive();
    if (mode === 'panel' && document.hasFocus()) focusActive();
  });
  // The bar shows one session's draft and one session's output line.
  showDraft();
  render();
}

/// Start another session and switch to it.
///
/// Answers with the session, so a caller with something to run in it can
/// write to it once it exists. Nothing when the spawn failed, which the
/// terminal itself has already said.
async function addSession(): Promise<TerminalSession | null> {
  const { term, fit, search } = newTerminal();

  const pane = document.createElement('div');
  pane.className = 'pane';
  pane.hidden = true;
  terminalsEl.appendChild(pane);

  const tab = document.createElement('button');
  tab.className = 'tab';
  tab.innerHTML = '<span class="dot"></span><span class="label"></span>';
  tabsEl.insertBefore(tab, tabAdd);

  term.open(pane);

  const session: TerminalSession = {
    kind: 'terminal',
    id: '',
    term,
    fit,
    search,
    pane,
    tab,
    state: 'idle',
    stateSince: Date.now(),
    lastCause: '',
    lastTurnMs: null,
    pending: '',
    cwd: null,
  };
  sessions.push(session);
  tab.addEventListener('click', () => activate(session));
  setLabel(session, { id: '', label: String(sessions.length), icon: null, color: null, cwd: null });

  search.onDidChangeResults(({ resultIndex, resultCount }) => {
    if (active !== session) return;
    findCount.textContent = resultCount ? `${resultIndex + 1}/${resultCount}` : 'none';
    findBox.classList.toggle('no-matches', findInput.value !== '' && resultCount === 0);
  });

  // On screen before spawning, so the terminal has a real size to report.
  activate(session);
  await new Promise(requestAnimationFrame);
  fit.fit();

  const onEvent = new Channel<PtyEvent>();
  onEvent.onmessage = (msg) => {
    if (msg.event === 'output') {
      term.write(new Uint8Array(msg.data.bytes));
    } else if (msg.event === 'agentStateChanged') {
      setAgentState(session, msg.data.state, msg.data.cause);
    } else if (msg.event === 'agentChanged') {
      setLabel(session, msg.data.agent);
    } else {
      const code = msg.data.code ?? 'signal';
      term.write(`\r\n\x1b[90m[process exited: ${code}]\x1b[0m\r\n`);
    }
  };

  try {
    session.id = await invoke<string>('spawn_session', {
      cols: term.cols,
      rows: term.rows,
      onEvent,
    });
  } catch (err) {
    term.write(`\r\n\x1b[31mfailed to start session: ${err}\x1b[0m\r\n`);
    return null;
  }

  term.attachCustomKeyEventHandler(handleTerminalKey);
  // Bound to this session rather than to the active one. See `write`.
  term.onData((data) => write(session, data));
  term.onResize(({ cols, rows }) => {
    invoke('resize_pty', { sessionId: session.id, cols, rows }).catch(() => {});
  });
  return session;
}

/// End a session and drop its tab. The last one is kept: a window with no
/// terminal in it has nothing to show and no way back.
function closeSession(session: Session) {
  if (sessions.length <= 1) return;
  if (session.kind === 'terminal') {
    if (session.id) invoke('kill_session', { sessionId: session.id }).catch(() => {});
    session.term.dispose();
  } else {
    invoke('codex_close', { threadId: session.threadId }).catch(() => {});
  }
  const index = sessions.indexOf(session);
  sessions.splice(index, 1);
  session.tab.remove();
  session.pane.remove();
  if (active === session) {
    active = null;
    activate(sessions[Math.min(index, sessions.length - 1)]);
  }
  render();
}

// --- Codex threads ---------------------------------------------------

type CodexThread = { id: string; title: string; workspace: string | null; updatedAt: number | null };

type CodexEvent =
  | { event: 'view'; data: ThreadView }
  | { event: 'agentStateChanged'; data: { state: AgentState; cause: string } }
  | { event: 'problem'; data: { message: string | null } };

const codexAdd = document.getElementById('codex-add') as HTMLButtonElement;
const codexPicker = document.getElementById('codex-picker')!;
const codexPickerNote = document.getElementById('codex-picker-note')!;
const codexThreadList = document.getElementById('codex-thread-list')!;
codexAdd.innerHTML = codexMark;

/// "5m ago" rather than a timestamp: the list is for picking the thread
/// you were just in, and recency is what tells them apart.
function ago(seconds: number | null): string {
  if (!seconds) return '';
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - seconds) / 60));
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

function codexSessionFor(threadId: string): CodexSession | undefined {
  return sessions.find((s): s is CodexSession => s.kind === 'codex' && s.threadId === threadId);
}

async function refreshCodexPicker() {
  codexPickerNote.classList.remove('failed');
  codexPickerNote.textContent = 'Reading your Codex threads…';
  codexThreadList.replaceChildren();
  try {
    const threads = await invoke<CodexThread[]>('codex_list_threads');
    codexPickerNote.textContent = threads.length
      ? 'Pick a thread to carry into oTerm. The Codex desktop app keeps running it.'
      : 'No Codex threads yet. Start one in the Codex desktop app first.';
    for (const thread of threads) {
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'codex-thread';
      const title = document.createElement('span');
      title.className = 'codex-thread-title';
      title.textContent = thread.title;
      const meta = document.createElement('span');
      meta.className = 'codex-thread-meta';
      const open = codexSessionFor(thread.id) ? 'open in oTerm' : '';
      meta.textContent = [thread.workspace, ago(thread.updatedAt), open].filter(Boolean).join(' · ');
      row.append(title, meta);
      row.addEventListener('click', () => {
        closeCodexPicker();
        openCodexThread(thread);
      });
      codexThreadList.appendChild(row);
    }
  } catch (err) {
    codexPickerNote.textContent = String(err);
    codexPickerNote.classList.add('failed');
  }
}

function openCodexPicker() {
  if (mode !== 'panel') return;
  codexPicker.hidden = false;
  void refreshCodexPicker();
}

function closeCodexPicker() {
  codexPicker.hidden = true;
  focusActive();
}

codexAdd.addEventListener('click', openCodexPicker);
codexPicker.querySelector('.codex-picker-close')!.addEventListener('click', closeCodexPicker);
codexPicker.addEventListener('click', (event) => {
  if (event.target === codexPicker) closeCodexPicker();
});

function interruptCodex(session: CodexSession) {
  invoke('codex_interrupt', { threadId: session.threadId }).catch((err) =>
    session.chat.showProblem(String(err)),
  );
}

/// Subscribe a tab to its thread. Also what Reconnect does: the backend
/// treats opening an open thread as starting that tab's connection over.
async function connectCodex(session: CodexSession) {
  const onEvent = new Channel<CodexEvent>();
  onEvent.onmessage = (msg) => {
    if (msg.event === 'view') {
      session.chat.update(msg.data);
      session.cwd = shortPath(msg.data.cwd);
      session.tab.title = `Codex: ${msg.data.title}`;
      if (active === session) render();
    } else if (msg.event === 'agentStateChanged') {
      setAgentState(session, msg.data.state, msg.data.cause);
    } else {
      session.chat.showProblem(msg.data.message);
    }
  };
  session.chat.showProblem(null);
  try {
    session.id = await invoke<string>('codex_open_thread', { threadId: session.threadId, onEvent });
  } catch (err) {
    session.chat.showProblem(String(err));
  }
}

/// Open a thread in a tab of its own, or go to the tab it is already in.
function openCodexThread(thread: CodexThread) {
  const existing = codexSessionFor(thread.id);
  if (existing) {
    activate(existing);
    return;
  }
  const tab = document.createElement('button');
  tab.className = 'tab has-icon codex';
  tab.innerHTML = '<span class="dot"></span><span class="label"></span>';
  (tab.querySelector('.label') as HTMLElement).innerHTML = codexMark;
  tab.title = `Codex: ${thread.title}`;
  tabsEl.insertBefore(tab, tabAdd);

  // Declared before the chat so its callbacks can reach the session.
  let session!: CodexSession;
  const chat = new CodexChat(codexMark, {
    send: async (text) => {
      await invoke('codex_send', { threadId: thread.id, text });
    },
    interrupt: () => interruptCodex(session),
    answer: async (decision) => {
      await invoke('codex_answer', { threadId: thread.id, decision });
    },
    reconnect: () => void connectCodex(session),
    draftChanged: (text) => {
      session.pending = text;
      if (active === session) showDraft();
    },
  });
  terminalsEl.appendChild(chat.pane);
  session = {
    kind: 'codex',
    id: '',
    threadId: thread.id,
    chat,
    pane: chat.pane,
    tab,
    state: 'idle',
    stateSince: Date.now(),
    lastCause: '',
    lastTurnMs: null,
    pending: '',
    cwd: null,
  };
  sessions.push(session);
  tab.addEventListener('click', () => activate(session));
  chat.pane.addEventListener('keydown', (event) => codexPaneKey(session, event));
  activate(session);
  void connectCodex(session);
}

/// The window shortcuts a terminal tab gets from xterm's key handler.
function codexPaneKey(session: CodexSession, event: KeyboardEvent) {
  if (!event.metaKey) return;
  if (event.key === 't') addSession();
  else if (event.key === 'w') closeSession(session);
  else if (event.key === ',') openSettings();
  else return;
  event.preventDefault();
}

tabAdd.addEventListener('click', () => {
  addSession();
});

async function start() {
  // Before any session spawns: a terminal reports its size to the PTY on
  // spawn, and that size depends on the font.
  try {
    const current = await invoke<Settings>('settings');
    settings = current;
    showSettings(current);
    maybeShowFirstRun(current);
  } catch {
    // The defaults a terminal is built with are already correct.
  }

  // What the last check stored, which is on disk before the window
  // exists. The check running now reports itself when it finishes.
  refreshUpdate();

  await addSession();
  applyMode(await invoke<WindowMode>('window_mode'));
}

// Refit on any container size change; onResize above forwards it to the
// PTY. Skipped in bar mode, where the hidden terminal measures as zero and
// would reflow the agent's TUI down to nothing.
let refitQueued = false;
new ResizeObserver(() => {
  if (refitQueued || mode !== 'panel' || !active) return;
  refitQueued = true;
  requestAnimationFrame(() => {
    refitQueued = false;
    const pane = active?.pane;
    if (pane && pane.clientHeight > 0 && pane.clientWidth > 0) fitActive();
  });
}).observe(terminalsEl);

start().catch((err) => {
  console.error('could not start', err);
});
