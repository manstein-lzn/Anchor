/** The renderer, and mostly the one property that matters about it.
 *
 * What it renders is model output and command output — untrusted text that arrived from a provider, a
 * page on the internet, or a program. The reason this is hand-written rather than a Markdown library is
 * that libraries produce HTML and HTML needs a sanitiser, and a sanitiser is a promise that a parser is
 * correct. These tests hold it to the other arrangement: there is no HTML, so there is nothing to
 * sanitise, and a link that should not be followed is not a link.
 */

import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { Markdown, parse, saferHref } from './markdown';

const shown = (text: string) => renderToStaticMarkup(<Markdown text={text} />);

describe('what may be rendered as markup', () => {
  it('refuses a link that would run a script', () => {
    const html = shown('[click me](javascript:alert(1))');

    expect(html).not.toContain('javascript:');
    expect(html).not.toContain('<a ');
    expect(html).toContain('click me'), 'the words survive; only the link does not';
  });

  it('refuses the other schemes that are an injection point', () => {
    for (const url of ['data:text/html,<script>1</script>', 'vbscript:x', 'file:///etc/passwd']) {
      expect(saferHref(url)).toBeUndefined();
    }
  });

  it('keeps the ones a trace actually contains', () => {
    expect(saferHref('https://arxiv.org/abs/2401.1')).toBe('https://arxiv.org/abs/2401.1');
    expect(saferHref('/runs/1/files/a')).toBe('/runs/1/files/a');
    expect(saferHref('#anchor')).toBe('#anchor');
  });

  it('escapes rather than executes what a model wrote', () => {
    const html = shown('<script>fetch("/steal")</script> and <img onerror=alert(1)>');

    expect(html).not.toContain('<script');
    expect(html).not.toContain('<img');
    expect(html).toContain('&lt;script&gt;');
  });
});

describe('the subset it does render', () => {
  it('fences code and does not read markup inside it', () => {
    const blocks = parse('before\n\n```sh\ngrep -q x <f>\n```\n\nafter');

    expect(blocks).toEqual([
      { kind: 'paragraph', text: 'before' },
      { kind: 'code', lang: 'sh', text: 'grep -q x <f>' },
      { kind: 'paragraph', text: 'after' },
    ]);
    expect(shown('```\n<b>bold</b>\n```')).toContain('&lt;b&gt;');
  });

  it('reads headings, lists and inline spans', () => {
    const blocks = parse('# Title\n\n- one\n- two\n\n1. first');

    expect(blocks[0]).toEqual({ kind: 'heading', level: 1, text: 'Title' });
    expect(blocks[1]).toEqual({ kind: 'list', ordered: false, items: ['one', 'two'] });
    expect(blocks[2]).toEqual({ kind: 'list', ordered: true, items: ['first'] });
    expect(shown('a `code` and **bold** and *italic*'))
      .toBe('<div class="markdown"><p>a <code>code</code> and <strong>bold</strong> and <em>italic</em></p></div>');
  });

  it('leaves an unterminated fence as code rather than swallowing the rest as prose', () => {
    expect(parse('```\nstill going')).toEqual([{ kind: 'code', lang: '', text: 'still going' }]);
  });

  it('reads a numbered list item as a list, not as a paragraph that starts with a digit', () => {
    expect(parse('1. first')[0]).toEqual({ kind: 'list', ordered: true, items: ['first'] });
  });
});
