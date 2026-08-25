import { Channel, invoke } from '@tauri-apps/api/core';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import './style.css';

type AgentState = 'idle' | 'busy' | 'needsInput' | 'done';

type PtyEvent =
  | { event: 'output'; data: { bytes: number[] } }
  | { event: 'agentStateChanged'; data: { state: AgentState; cause: string } }
  | { event: 'exited'; data: { code: number | null } };

const term = new Terminal({
  cursorBlink: true,
  fontSize: 13,
  fontFamily: 'Menlo, Monaco, "SF Mono", monospace',
  scrollback: 10_000,
  theme: {
    background: '#1a1b26',
    foreground: '#c0caf5',
    cursor: '#c0caf5',
  },
});
const fit = new FitAddon();
term.loadAddon(fit);

const container = document.getElementById('terminal')!;
term.open(container);
fit.fit();
term.focus();

// Debug overlay for the detected agent state.
const statusEl = document.getElementById('status')!;
const statusLabel = document.getElementById('status-label')!;
const statusMeta = document.getElementById('status-meta')!;
let stateSince = Date.now();
let lastCause = '';

const stateLabels: Record<AgentState, string> = {
  idle: 'idle',
  busy: 'busy',
  needsInput: 'needs input',
  done: 'done',
};

function setAgentState(state: AgentState, cause: string) {
  statusEl.className = state;
  statusLabel.textContent = stateLabels[state];
  stateSince = Date.now();
  lastCause = cause;
  updateStatusMeta();
}

function updateStatusMeta() {
  const secs = Math.floor((Date.now() - stateSince) / 1000);
  statusMeta.textContent = lastCause ? `${lastCause} · ${secs}s` : `${secs}s`;
}
setInterval(updateStatusMeta, 1000);

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

  const sessionId = await invoke<string>('spawn_session', {
    cols: term.cols,
    rows: term.rows,
    onEvent,
  });

  term.onData((data) => {
    invoke('write_pty', { sessionId, data });
  });
  term.onResize(({ cols, rows }) => {
    invoke('resize_pty', { sessionId, cols, rows });
  });
}

// Refit on any container size change; onResize above forwards it to the PTY.
let refitQueued = false;
new ResizeObserver(() => {
  if (refitQueued) return;
  refitQueued = true;
  requestAnimationFrame(() => {
    refitQueued = false;
    fit.fit();
  });
}).observe(container);

start().catch((err) => {
  term.write(`\r\n\x1b[31mfailed to start session: ${err}\x1b[0m\r\n`);
});
