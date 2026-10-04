import MarkdownIt, { type StateInline, type Token } from 'markdown-it';
import { openUrl } from '@tauri-apps/plugin-opener';

const STAR = 0x2a;
const UNDERSCORE = 0x5f;

/// Codex replies are mostly about code, where `__init__` and `snake_case`
/// are names and stars touching a slash are globs (`src/**/*.ts`).
/// CommonMark would pair those up as emphasis, so they are kept as text
/// before the emphasis rule sees them.
function literalDelimiters(state: StateInline, silent: boolean): boolean {
  const marker = state.src.charCodeAt(state.pos);
  if (marker !== STAR && marker !== UNDERSCORE) return false;
  let end = state.pos;
  while (end < state.posMax && state.src.charCodeAt(end) === marker) end += 1;
  if (marker === STAR && state.src[state.pos - 1] !== '/' && state.src[end] !== '/') return false;
  if (!silent) state.pending += state.src.slice(state.pos, end);
  state.pos = end;
  return true;
}

const md = new MarkdownIt({ html: false, linkify: false, typographer: false });
md.inline.ruler.before('emphasis', 'literal_delimiters', literalDelimiters);

const BLOCK_TAGS = new Set(['p', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'ul', 'ol', 'li', 'blockquote', 'thead', 'tbody', 'tr', 'th', 'td']);

const BLOCK_CLASSES: Record<string, string> = {
  paragraph_open: 'codex-text',
  heading_open: 'codex-heading',
  bullet_list_open: 'codex-list unordered',
  ordered_list_open: 'codex-list ordered',
  blockquote_open: 'codex-quote',
};

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function httpUrl(destination: string): URL | null {
  try {
    const url = new URL(destination);
    return url.protocol === 'http:' || url.protocol === 'https:' ? url : null;
  } catch {
    return null;
  }
}

function plainText(tokens: Token[]): string {
  return tokens
    .map((token) => {
      if (token.children) return plainText(token.children);
      return token.type === 'softbreak' ? '\n' : token.content;
    })
    .join('');
}

function openBlock(parent: HTMLElement, token: Token): HTMLElement {
  if (token.hidden) return parent;
  if (token.type === 'table_open') {
    const scroller = el('div', 'codex-table');
    const table = el('table');
    scroller.appendChild(table);
    parent.appendChild(scroller);
    return table;
  }
  const node = document.createElement(BLOCK_TAGS.has(token.tag) ? token.tag : 'div');
  const className = BLOCK_CLASSES[token.type];
  if (className) node.className = className;
  const start = token.attrGet('start');
  if (start !== null) node.setAttribute('start', String(start));
  const align = String(token.attrGet('style') ?? '').match(/text-align:(left|right|center)/)?.[1];
  if (align) node.style.textAlign = align;
  parent.appendChild(node);
  return node;
}

function appendLeafBlock(parent: HTMLElement, token: Token) {
  switch (token.type) {
    case 'inline':
      appendInline(parent, token.children ?? []);
      break;
    case 'fence':
    case 'code_block':
      parent.appendChild(el('pre', 'codex-code', token.content.replace(/\n$/, '')));
      break;
    case 'hr':
      parent.appendChild(el('hr', 'codex-rule'));
      break;
    default:
      if (token.content) parent.appendChild(el('p', 'codex-text', token.content.trimEnd()));
  }
}

function openLink(parent: HTMLElement, token: Token): HTMLElement {
  const destination = String(token.attrGet('href') ?? '');
  const url = httpUrl(destination);
  if (!url) {
    const reference = el('span', 'codex-link-ref');
    reference.title = md.normalizeLinkText(destination);
    parent.appendChild(reference);
    return reference;
  }
  const anchor = el('a', 'codex-link');
  anchor.href = url.href;
  anchor.title = url.href;
  anchor.addEventListener('click', (event) => {
    event.preventDefault();
    openUrl(url.href).catch((err) => console.error(`could not open ${url.href}:`, err));
  });
  parent.appendChild(anchor);
  return anchor;
}

/// Link text that is itself a URL can point somewhere else than it says,
/// so the real host is shown next to it when the two disagree.
function closeLink(link: HTMLElement) {
  if (!(link instanceof HTMLAnchorElement)) return;
  const shown = httpUrl(link.textContent?.trim() ?? '');
  const real = new URL(link.getAttribute('href') ?? '');
  if (shown && shown.host !== real.host) link.after(' ', el('span', 'codex-link-host', `(${real.host})`));
}

function appendInline(target: HTMLElement, tokens: Token[]) {
  const parents = [target];
  for (const token of tokens) {
    const parent = parents[parents.length - 1];
    switch (token.type) {
      case 'softbreak':
        parent.appendChild(document.createTextNode('\n'));
        break;
      case 'hardbreak':
        parent.appendChild(el('br'));
        break;
      case 'code_inline':
        parent.appendChild(el('code', 'codex-inline-code', token.content));
        break;
      case 'image':
        parent.appendChild(document.createTextNode(plainText(token.children ?? [])));
        break;
      case 'strong_open':
      case 'em_open':
      case 's_open':
        parents.push(parent.appendChild(document.createElement(token.tag)));
        break;
      case 'link_open':
        parents.push(openLink(parent, token));
        break;
      case 'link_close':
        closeLink(parents.pop()!);
        break;
      default:
        if (token.nesting === 1) parents.push(parent.appendChild(el('span')));
        else if (token.nesting === -1) parents.pop();
        else parent.appendChild(document.createTextNode(token.content));
    }
  }
}

/// Fills `target` with a Codex reply rendered from Markdown. Every element is
/// created by hand from markdown-it's tokens and every piece of Codex text goes
/// in as a text node, so HTML in a reply stays inert text and only http(s)
/// links become anchors.
export function renderMarkdown(target: HTMLElement, text: string) {
  target.replaceChildren();
  const parents = [target];
  for (const token of md.parse(text, {})) {
    const parent = parents[parents.length - 1];
    if (token.nesting === 1) parents.push(openBlock(parent, token));
    else if (token.nesting === -1) parents.pop();
    else appendLeafBlock(parent, token);
  }
}
