import { beforeEach, describe, expect, it, vi } from 'vitest';
import { openUrl } from '@tauri-apps/plugin-opener';
import { renderMarkdown } from './markdown';

vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn(() => Promise.resolve()) }));

function render(text: string): HTMLElement {
  const root = document.createElement('div');
  renderMarkdown(root, text);
  return root;
}

function tags(root: HTMLElement, selector: string): string[] {
  return [...root.querySelectorAll(selector)].map((node) => node.textContent ?? '');
}

beforeEach(() => {
  vi.mocked(openUrl).mockClear();
});

describe('code talk stays literal', () => {
  it('keeps underscores in identifiers and dunder names', () => {
    const text = 'Rename my_file_name.py and test_user_login, then edit __init__.py and __init__.';
    const root = render(text);
    expect(root.textContent).toBe(text);
    expect(root.querySelectorAll('em, strong')).toHaveLength(0);
  });

  it('keeps glob stars next to slashes', () => {
    const text = 'Match src/**/*.ts and lib/**/*.js here.';
    const root = render(text);
    expect(root.textContent).toBe(text);
    expect(root.querySelectorAll('em, strong')).toHaveLength(0);
  });

  it('keeps backslashes that do not escape punctuation', () => {
    const text = 'Open C:\\Users\\me\\docs and match \\d+ there.';
    expect(render(text).textContent).toBe(text);
  });

  it('still unescapes backslashed punctuation', () => {
    expect(render('2 \\* 3 and \\_private').textContent).toBe('2 * 3 and _private');
  });
});

describe('inline formatting', () => {
  it('renders triple stars as strong and em together', () => {
    const root = render('***both***');
    expect(tags(root, 'em strong, strong em')).toEqual(['both']);
    expect(root.textContent).toBe('both');
  });

  it('renders bold, italic, inline code and strikethrough', () => {
    const root = render('**bold** *it* `code` ~~gone~~');
    expect(tags(root, 'strong')).toEqual(['bold']);
    expect(tags(root, 'em')).toEqual(['it']);
    expect(tags(root, 'code.codex-inline-code')).toEqual(['code']);
    expect(tags(root, 's')).toEqual(['gone']);
  });

  it('keeps a line that is only a triple-backtick span inline', () => {
    const root = render('```npm test```\nThen check the output.\n\nNext paragraph.');
    expect(root.querySelector('pre')).toBeNull();
    expect(tags(root, 'code.codex-inline-code')).toEqual(['npm test']);
    expect(root.textContent).toContain('Then check the output.');
    expect(tags(root, '.codex-text')).toHaveLength(2);
  });

  it('turns a hard break into br', () => {
    const root = render('first  \nsecond');
    expect(root.querySelectorAll('br')).toHaveLength(1);
  });

  it('shows image alt text without loading the image', () => {
    const root = render('![a diagram](https://example.com/a.png)');
    expect(root.querySelector('img')).toBeNull();
    expect(root.textContent).toBe('a diagram');
  });
});

describe('block structure', () => {
  it('renders headings at their level', () => {
    const root = render('# Top\n\n### Third');
    expect(tags(root, 'h1.codex-heading')).toEqual(['Top']);
    expect(tags(root, 'h3.codex-heading')).toEqual(['Third']);
  });

  it('continues numbering after a fence interrupts a list', () => {
    const root = render('1. a\n```\ncode\n```\n2. b');
    const lists = root.querySelectorAll('ol');
    expect(lists).toHaveLength(2);
    expect(lists[1].getAttribute('start')).toBe('2');
    expect(tags(root, 'pre.codex-code')).toEqual(['code']);
  });

  it('nests indented lists', () => {
    const root = render('- parent\n  - child\n    - grandchild');
    expect(tags(root, 'ul > li > ul > li > ul > li')).toEqual(['grandchild']);
  });

  it('treats a four-space child as a nested item', () => {
    const root = render('- parent\n    - grandchild');
    expect(tags(root, 'ul > li > ul > li')).toEqual(['grandchild']);
    expect(root.textContent).not.toContain('-');
  });

  it('keeps a wrapped list item in one list', () => {
    const root = render('1. first item\n   wraps here\n2. second item');
    expect(root.querySelectorAll('ol')).toHaveLength(1);
    const items = tags(root, 'ol > li');
    expect(items).toHaveLength(2);
    expect(items[0].replace(/\s+/g, ' ')).toBe('first item wraps here');
  });

  it('lets a longer fence contain a shorter one', () => {
    const root = render('````md\n```js\nconst a = 1;\n```\n````\nafter');
    expect(tags(root, 'pre.codex-code')).toEqual(['```js\nconst a = 1;\n```']);
    expect(tags(root, '.codex-text')).toEqual(['after']);
  });

  it('renders blockquotes and rules', () => {
    const root = render('> quoted\n\n---\n\nafter');
    expect(tags(root, 'blockquote')).toEqual(['quoted']);
    expect(root.querySelectorAll('hr')).toHaveLength(1);
  });

  it('renders tables', () => {
    const root = render('| a | b |\n|---|---|\n| 1 | 2 |');
    expect(tags(root, 'table th')).toEqual(['a', 'b']);
    expect(tags(root, 'table td')).toEqual(['1', '2']);
  });

  it('replaces what was rendered before', () => {
    const root = render('old');
    renderMarkdown(root, 'new');
    expect(root.textContent).toBe('new');
  });
});

describe('links', () => {
  it('keeps parentheses inside the destination', () => {
    const anchor = render('[wiki](https://en.wikipedia.org/wiki/Foo_(bar))').querySelector('a');
    expect(anchor?.getAttribute('href')).toBe('https://en.wikipedia.org/wiki/Foo_(bar)');
  });

  it('opens http links through the opener plugin', () => {
    const anchor = render('[docs](https://example.com/docs)').querySelector('a.codex-link')!;
    expect(anchor.getAttribute('title')).toBe('https://example.com/docs');
    const click = new MouseEvent('click', { bubbles: true, cancelable: true });
    anchor.dispatchEvent(click);
    expect(click.defaultPrevented).toBe(true);
    expect(openUrl).toHaveBeenCalledWith('https://example.com/docs');
  });

  it('shows the real host when the link text is a different URL', () => {
    const root = render('[https://github.com/login](https://evil.example/phish)');
    const anchor = root.querySelector('a.codex-link')!;
    expect(anchor.getAttribute('href')).toBe('https://evil.example/phish');
    expect(anchor.getAttribute('title')).toBe('https://evil.example/phish');
    expect(tags(root, '.codex-link-host')).toEqual(['(evil.example)']);
  });

  it('adds no host when the link text matches the destination host', () => {
    const root = render('[https://github.com/a](https://github.com/a/b) and [docs](https://example.com)');
    expect(root.querySelectorAll('.codex-link-host')).toHaveLength(0);
  });

  it('renders a file reference as its text with the path as title', () => {
    const root = render('See [codex.ts](/Users/me/dev/codex.ts:12) for it.');
    expect(root.querySelector('a')).toBeNull();
    expect(root.textContent).toBe('See codex.ts for it.');
    expect(root.querySelector('[title]')?.getAttribute('title')).toBe('/Users/me/dev/codex.ts:12');
  });
});

describe('safety', () => {
  it('shows an img tag as literal text', () => {
    const root = render('<img src=x onerror=alert(1)>');
    expect(root.textContent).toBe('<img src=x onerror=alert(1)>');
    expect(root.querySelector('img')).toBeNull();
  });

  it('shows a script tag as literal text', () => {
    const root = render('<script>alert(1)</script>');
    expect(root.textContent).toBe('<script>alert(1)</script>');
    expect(root.querySelector('script')).toBeNull();
  });

  it('shows inline html as literal text', () => {
    const root = render('a <b onclick="x()">bold</b> c');
    expect(root.textContent).toBe('a <b onclick="x()">bold</b> c');
    expect(root.querySelector('b')).toBeNull();
  });

  it('makes no anchor for a javascript link', () => {
    expect(render('[x](javascript:alert(1))').querySelector('a')).toBeNull();
  });

  it('normalises an uppercase http scheme', () => {
    const anchor = render('[x](HTTP://Example.com)').querySelector('a.codex-link');
    expect(anchor?.getAttribute('href')).toBe('http://example.com/');
  });

  it('puts no event handler attributes on any element', () => {
    const root = render(
      [
        '<img src=x onerror=alert(1)>',
        '<a href="https://example.com" onclick="alert(1)">x</a>',
        '[x](javascript:alert(1)) [y](https://example.com "t\\" onmouseover=\\"alert(1)")',
        '<div onload=alert(1)>',
      ].join('\n\n'),
    );
    const attributes = [...root.querySelectorAll('*')].flatMap((node) => [...node.attributes].map((attr) => attr.name));
    expect(attributes.filter((name) => name.toLowerCase().startsWith('on'))).toEqual([]);
  });
});

describe('streaming', () => {
  it('renders an unclosed fence as code to the end', () => {
    const root = render('Here:\n\n```ts\nconst a = 1;\nconst b');
    expect(tags(root, 'pre.codex-code')).toEqual(['const a = 1;\nconst b']);
  });

  it('survives a reply cut off mid-construct', () => {
    for (const partial of ['[link](https://exa', '**bold', '- item\n  - ', '| a |\n|--', '> quote\n> ```']) {
      expect(() => render(partial)).not.toThrow();
    }
  });

  it.each([
    ['mixed markers', '**a** _b_ *c* `d` [e](https://f.example) \\g <h> ~~i~~ src/**/*.j\n- k\n  1. l\n> m\n'],
    ['unclosed brackets', '[x '],
    ['unclosed backticks', '`a ``b '],
  ])('renders a 20 KB reply of %s quickly', (_name, chunk) => {
    const text = chunk.repeat(Math.ceil(20_000 / chunk.length));
    const timings = Array.from({ length: 5 }, () => {
      const started = performance.now();
      render(text);
      return performance.now() - started;
    });
    // Best of five keeps a slow CI runner's GC pause from failing this. The
    // hand-written parser this replaced took about 60 ms on its worst input.
    expect(Math.min(...timings)).toBeLessThan(100);
  });
});
