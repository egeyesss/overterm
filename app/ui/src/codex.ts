import { openUrl } from '@tauri-apps/plugin-opener';

// A Codex desktop thread shown as a chat, inside a tab of its own.
//
// The backend sends the whole thread view every time something on screen
// changes. Rendering is keyed per item, so a message that is still
// streaming is the only element rewritten and everything above it keeps its
// scroll position and text selection.

export type ThreadView = {
  title: string;
  cwd: string | null;
  activity: 'idle' | 'working' | 'waiting' | 'unavailable';
  activeTurnId: string | null;
  turns: Turn[];
  approval: Approval | null;
};

type Turn = { id: string; status: string; items: Item[] };

type Item =
  | { kind: 'user'; id: string; text: string }
  | { kind: 'agent'; id: string; text: string; commentary: boolean }
  | { kind: 'command'; id: string; command: string; status: string; output: string | null; exitCode: number | null }
  | { kind: 'fileChange'; id: string; status: string; paths: string[] }
  | { kind: 'note'; id: string; text: string };

type Approval = {
  requestId: number | string;
  kind: 'command' | 'fileChange' | 'other';
  reason: string | null;
  command: string | null;
  cwd: string | null;
};

export type ChatActions = {
  send(text: string): Promise<void>;
  interrupt(): void;
  answer(approve: boolean): Promise<void>;
  reconnect(): void;
  draftChanged(text: string): void;
};

/// How close to the bottom counts as "reading the latest", so new output
/// keeps the log pinned there without yanking someone who scrolled up.
const STICKY_BOTTOM_PX = 48;

/// Home written as ~, the way the terminal tabs show their directory.
export function shortPath(path: string | null): string | null {
  return path?.replace(/^\/Users\/[^/]+(?=\/|$)/, '~') ?? null;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function safeMarkdownUrl(destination: string): string | null {
  if (!/^https?:\/\//i.test(destination)) return null;
  try {
    return new URL(destination).href;
  } catch {
    return null;
  }
}

function appendInlineMarkdown(target: HTMLElement, text: string) {
  let rest = text;
  while (rest) {
    if (rest.startsWith('\\') && rest.length > 1) {
      target.appendChild(document.createTextNode(rest[1]));
      rest = rest.slice(2);
      continue;
    }

    const code = rest.match(/^(`+)([\s\S]*?)\1/);
    if (code) {
      target.appendChild(el('code', 'codex-inline-code', code[2].replace(/\n/g, ' ')));
      rest = rest.slice(code[0].length);
      continue;
    }

    const link = rest.match(/^\[([^\]]+)\]\((https?:\/\/[^)\s]+)(?:\s+["'][^)]*["'])?\)/i);
    if (link) {
      const url = safeMarkdownUrl(link[2]);
      if (url) {
        const anchor = el('a', 'codex-link');
        appendInlineMarkdown(anchor, link[1]);
        anchor.href = url;
        anchor.addEventListener('click', (event) => {
          event.preventDefault();
          void openUrl(url).catch((error) => console.error(`could not open ${url}:`, error));
        });
        target.appendChild(anchor);
      } else {
        target.appendChild(document.createTextNode(link[0]));
      }
      rest = rest.slice(link[0].length);
      continue;
    }

    const strong = rest.match(/^(\*\*|__)(?=\S)([\s\S]*?\S)\1/);
    if (strong) {
      const node = el('strong');
      appendInlineMarkdown(node, strong[2]);
      target.appendChild(node);
      rest = rest.slice(strong[0].length);
      continue;
    }

    const emphasis = rest.match(/^(\*|_)(?=\S)([\s\S]*?\S)\1/);
    if (emphasis) {
      const node = el('em');
      appendInlineMarkdown(node, emphasis[2]);
      target.appendChild(node);
      rest = rest.slice(emphasis[0].length);
      continue;
    }

    const nextSpecial = rest.search(/[\\`[*_]/);
    if (nextSpecial < 0) {
      target.appendChild(document.createTextNode(rest));
      break;
    }
    if (nextSpecial > 0) {
      target.appendChild(document.createTextNode(rest.slice(0, nextSpecial)));
      rest = rest.slice(nextSpecial);
      continue;
    }

    target.appendChild(document.createTextNode(rest[0]));
    rest = rest.slice(1);
  }
}

function appendMarkdownParagraph(target: HTMLElement, lines: string[]) {
  const paragraph = el('div', 'codex-text');
  lines.forEach((line, index) => {
    if (index) paragraph.appendChild(document.createElement('br'));
    appendInlineMarkdown(paragraph, line);
  });
  target.appendChild(paragraph);
}

function appendMarkdownList(target: HTMLElement, lines: string[], ordered: boolean) {
  const list = el(ordered ? 'ol' : 'ul', `codex-list ${ordered ? 'ordered' : 'unordered'}`);
  for (const line of lines) {
    const match = ordered ? line.match(/^ {0,3}\d+[.)]\s+(.*)$/) : line.match(/^ {0,3}[-+*]\s+(.*)$/);
    if (!match) break;
    const item = el('li');
    appendInlineMarkdown(item, match[1]);
    list.appendChild(item);
  }
  target.appendChild(list);
}

/// Agent replies are Markdown. This is deliberately a small renderer rather
/// than an HTML parser: every node is created by hand and all Codex text enters
/// through text nodes, so even HTML-looking model output remains inert text.
function fillMarkdownish(target: HTMLElement, text: string) {
  target.replaceChildren();
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  let paragraph: string[] = [];
  const flushParagraph = () => {
    if (paragraph.length) appendMarkdownParagraph(target, paragraph);
    paragraph = [];
  };

  for (let index = 0; index < lines.length; ) {
    const line = lines[index];
    const fence = line.match(/^ {0,3}```[^\n]*$/);
    if (fence) {
      flushParagraph();
      const code: string[] = [];
      index += 1;
      while (index < lines.length && !/^ {0,3}```\s*$/.test(lines[index])) {
        code.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) index += 1;
      target.appendChild(el('pre', 'codex-code', code.join('\n')));
      continue;
    }

    const heading = line.match(/^ {0,3}(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (heading) {
      flushParagraph();
      const title = el(`h${heading[1].length}` as keyof HTMLElementTagNameMap, 'codex-heading');
      appendInlineMarkdown(title, heading[2]);
      target.appendChild(title);
      index += 1;
      continue;
    }

    if (/^ {0,3}[-+*]\s+/.test(line)) {
      flushParagraph();
      const items: string[] = [];
      while (index < lines.length && /^ {0,3}[-+*]\s+/.test(lines[index])) items.push(lines[index++]);
      appendMarkdownList(target, items, false);
      continue;
    }

    if (/^ {0,3}\d+[.)]\s+/.test(line)) {
      flushParagraph();
      const items: string[] = [];
      while (index < lines.length && /^ {0,3}\d+[.)]\s+/.test(lines[index])) items.push(lines[index++]);
      appendMarkdownList(target, items, true);
      continue;
    }

    if (!line.trim()) {
      flushParagraph();
      index += 1;
      continue;
    }

    paragraph.push(line);
    index += 1;
  }
  flushParagraph();
}

function statusLabel(status: string): string {
  switch (status) {
    case 'inProgress':
      return 'running';
    case 'completed':
      return 'done';
    default:
      return status;
  }
}

function renderItem(item: Item): HTMLElement {
  const node = el('div', `codex-item ${item.kind}`);
  switch (item.kind) {
    case 'user':
      node.appendChild(el('div', 'codex-bubble', item.text.replace(/\n+$/, '')));
      break;
    case 'agent':
      node.classList.toggle('commentary', item.commentary);
      fillMarkdownish(node, item.text);
      break;
    case 'command': {
      const head = el('div', 'codex-command-head');
      head.appendChild(el('code', 'codex-command', item.command));
      const status = el('span', `codex-status status-${item.status}`, statusLabel(item.status));
      if (item.exitCode !== null && item.exitCode !== 0) status.textContent = `exit ${item.exitCode}`;
      head.appendChild(status);
      if (item.output && item.output.trim()) {
        // The command line is the toggle, so output costs no extra row.
        const details = el('details', 'codex-output');
        const summary = el('summary');
        summary.appendChild(head);
        details.append(summary, el('pre', undefined, item.output.replace(/\n$/, '')));
        node.appendChild(details);
      } else {
        node.appendChild(head);
      }
      break;
    }
    case 'fileChange': {
      const head = el('div', 'codex-command-head');
      const count = item.paths.length;
      head.appendChild(el('span', 'codex-files', count === 1 ? 'Edited 1 file' : `Edited ${count} files`));
      head.appendChild(el('span', `codex-status status-${item.status}`, statusLabel(item.status)));
      node.appendChild(head);
      const list = el('ul', 'codex-paths');
      for (const path of item.paths) list.appendChild(el('li', undefined, path));
      node.appendChild(list);
      break;
    }
    case 'note':
      node.textContent = item.text;
      break;
  }
  return node;
}

export class CodexChat {
  readonly pane: HTMLDivElement;
  private readonly head: HTMLElement;
  private readonly titleEl: HTMLElement;
  private readonly cwdEl: HTMLElement;
  private readonly problemEl: HTMLElement;
  private readonly problemText: HTMLElement;
  private readonly log: HTMLElement;
  private readonly empty: HTMLElement;
  private readonly working: HTMLElement;
  private readonly approvalEl: HTMLElement;
  private readonly composer: HTMLTextAreaElement;
  private readonly sendButton: HTMLButtonElement;
  private readonly hint: HTMLElement;
  private readonly rendered = new Map<string, { node: HTMLElement; json: string }>();
  private view: ThreadView | null = null;
  private pendingView: ThreadView | null = null;
  private frameQueued = false;
  private sending = false;
  /// Whether the log follows new output. Only the user scrolling up turns
  /// it off, so output that arrives while the pane is hidden still counts.
  private followBottom = true;

  constructor(mark: string, private readonly actions: ChatActions) {
    this.pane = el('div', 'pane codex-pane');
    this.pane.hidden = true;

    this.head = el('header', 'codex-head');
    const markEl = el('span', 'codex-mark');
    markEl.innerHTML = mark;
    this.titleEl = el('span', 'codex-title', 'Codex');
    this.cwdEl = el('span', 'codex-cwd');
    this.head.append(markEl, this.titleEl, this.cwdEl);

    this.problemEl = el('div', 'codex-problem');
    this.problemEl.hidden = true;
    this.problemText = el('span');
    const reconnect = el('button', undefined, 'Reconnect');
    reconnect.type = 'button';
    reconnect.addEventListener('click', () => actions.reconnect());
    this.problemEl.append(this.problemText, reconnect);

    this.log = el('div', 'codex-log');
    this.empty = el('p', 'codex-empty', 'Connecting to Codex…');
    this.working = el('div', 'codex-working', 'Codex is working…');
    this.working.hidden = true;
    this.log.append(this.empty, this.working);

    this.approvalEl = el('div', 'codex-approval');
    this.approvalEl.hidden = true;

    const form = el('form', 'codex-composer');
    this.composer = el('textarea');
    this.composer.rows = 2;
    this.composer.placeholder = 'Message Codex';
    this.composer.spellcheck = false;
    this.sendButton = el('button', 'codex-send', 'Send');
    this.sendButton.type = 'submit';
    this.hint = el('div', 'codex-hint', 'Enter sends · Shift+Enter new line');
    const row = el('div', 'codex-composer-row');
    row.append(this.composer, this.sendButton);
    form.append(row, this.hint);

    form.addEventListener('submit', (event) => {
      event.preventDefault();
      void this.submit();
    });
    this.composer.addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
        event.preventDefault();
        void this.submit();
      } else if (event.key === 'Escape' && this.isWorking()) {
        event.preventDefault();
        this.actions.interrupt();
      }
    });
    this.composer.addEventListener('input', () => actions.draftChanged(this.composer.value));

    this.pane.append(this.head, this.problemEl, this.log, this.approvalEl, form);

    this.log.addEventListener('scroll', () => {
      // A hidden log measures as zero and says nothing about the reader.
      if (this.log.clientHeight === 0) return;
      const gap = this.log.scrollHeight - this.log.scrollTop - this.log.clientHeight;
      this.followBottom = gap < STICKY_BOTTOM_PX;
    });
    // While the window is collapsed or the tab is in the background the log
    // is display:none, so scrolling it does nothing. It changes size when it
    // is shown again, which is the moment to catch up on what arrived.
    new ResizeObserver(() => this.keepAtBottom()).observe(this.log);
  }

  private keepAtBottom() {
    if (this.followBottom && this.log.clientHeight > 0) {
      this.log.scrollTop = this.log.scrollHeight;
    }
  }

  get draft(): string {
    return this.composer.value;
  }

  set draft(text: string) {
    this.composer.value = text;
  }

  focus() {
    this.composer.focus();
  }

  isWorking(): boolean {
    return this.view?.activity === 'working' || this.view?.activity === 'waiting';
  }

  /// Send whatever is in the composer. Also what the bar's Enter does, so
  /// the bar and the composer are one draft.
  async submit(): Promise<void> {
    const text = this.composer.value.trim();
    if (!text || this.sending) return;
    this.sending = true;
    this.sendButton.disabled = true;
    try {
      await this.actions.send(text);
      // Whoever just sent something wants to see the answer.
      this.followBottom = true;
      this.keepAtBottom();
      this.composer.value = '';
      this.actions.draftChanged('');
    } catch (err) {
      this.showProblem(String(err));
    } finally {
      this.sending = false;
      this.sendButton.disabled = false;
    }
  }

  showProblem(message: string | null) {
    this.problemEl.hidden = message === null;
    this.problemText.textContent = message ?? '';
  }

  /// Keep only the latest view and draw it on the next frame. Streaming
  /// sends one view per token burst, faster than it is worth drawing.
  update(view: ThreadView) {
    this.pendingView = view;
    if (this.frameQueued) return;
    this.frameQueued = true;
    requestAnimationFrame(() => {
      this.frameQueued = false;
      if (this.pendingView) this.render(this.pendingView);
      this.pendingView = null;
    });
  }

  private render(view: ThreadView) {
    this.view = view;
    this.titleEl.textContent = view.title;
    this.cwdEl.textContent = shortPath(view.cwd) ?? '';
    this.showProblem(null);

    const wanted: HTMLElement[] = [];
    const seen = new Set<string>();
    for (const turn of view.turns) {
      for (const item of turn.items) {
        const key = `${turn.id}/${item.id}`;
        seen.add(key);
        const json = JSON.stringify(item);
        let entry = this.rendered.get(key);
        if (!entry || entry.json !== json) {
          const node = renderItem(item);
          // A command someone opened stays open while its output grows.
          const wasOpen = entry?.node.querySelector('details')?.open;
          if (wasOpen) node.querySelector('details')?.setAttribute('open', '');
          entry?.node.replaceWith(node);
          entry = { node, json };
          this.rendered.set(key, entry);
        }
        wanted.push(entry.node);
      }
      if (turn.status === 'interrupted' || turn.status === 'failed') {
        const key = `${turn.id}/#end`;
        seen.add(key);
        let entry = this.rendered.get(key);
        if (!entry) {
          const text = turn.status === 'interrupted' ? 'Stopped' : 'This turn failed';
          entry = { node: el('div', 'codex-item note', text), json: '' };
          this.rendered.set(key, entry);
        }
        wanted.push(entry.node);
      }
    }
    for (const [key, entry] of this.rendered) {
      if (!seen.has(key)) {
        entry.node.remove();
        this.rendered.delete(key);
      }
    }

    this.empty.hidden = wanted.length > 0;
    this.empty.textContent = 'Nothing in this thread yet. Send Codex a message.';
    wanted.push(this.working);
    this.working.hidden = view.activity !== 'working';
    let index = 1; // after the empty-state line, which stays first
    for (const node of wanted) {
      const current = this.log.children[index];
      if (current !== node) this.log.insertBefore(node, current ?? null);
      index++;
    }

    this.renderApproval(view.approval);
    this.hint.textContent = this.isWorking()
      ? 'Enter adds to what Codex is doing · Esc stops it'
      : 'Enter sends · Shift+Enter new line';
    this.keepAtBottom();
  }

  private renderApproval(approval: Approval | null) {
    this.approvalEl.hidden = approval === null;
    if (!approval) return;
    const key = JSON.stringify(approval);
    if (this.approvalEl.dataset.key === key) return;
    this.approvalEl.dataset.key = key;
    this.approvalEl.replaceChildren();
    const title =
      approval.kind === 'command'
        ? 'Codex wants to run a command'
        : approval.kind === 'fileChange'
          ? 'Codex wants to change files'
          : 'Codex is asking you something';
    this.approvalEl.appendChild(el('div', 'codex-approval-title', title));
    if (approval.reason) this.approvalEl.appendChild(el('p', 'codex-approval-reason', approval.reason));
    if (approval.command) this.approvalEl.appendChild(el('pre', 'codex-code', approval.command));
    if (approval.kind === 'other') {
      this.approvalEl.appendChild(el('p', 'codex-approval-reason', 'Answer this one in the Codex desktop app.'));
      return;
    }
    const buttons = el('div', 'codex-approval-actions');
    const approve = el('button', 'approve', 'Approve');
    const decline = el('button', 'decline', 'Decline');
    for (const [button, value] of [
      [approve, true],
      [decline, false],
    ] as const) {
      button.type = 'button';
      button.addEventListener('click', async () => {
        approve.disabled = decline.disabled = true;
        try {
          await this.actions.answer(value);
        } catch (err) {
          this.showProblem(String(err));
          approve.disabled = decline.disabled = false;
        }
      });
    }
    buttons.append(approve, decline);
    this.approvalEl.appendChild(buttons);
  }
}
