import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import { Markdown, saferHref } from './markdown';

const shown = (text: string) => renderToStaticMarkup(<Markdown text={text} />);

describe('Markdown documents', () => {
  it('renders tables, task lists, nested lists, quotes, deletion and reference links', () => {
    const html = shown('# Title\n\n| Name | Value |\n| :--- | ---: |\n| **A** | 2 |\n\n- [x] done\n- [ ] pending\n  - nested\n\n> quote\n\n~~old~~ [reference][ref]\n\n[ref]: https://example.com');
    for (const fragment of ['<h1>Title</h1>', '<table>', '<th', '<td', 'text-align:right',
      'type="checkbox"', 'checked=""', 'nested', '<blockquote>', '<del>old</del>', 'href="https://example.com"']) {
      expect(html).toContain(fragment);
    }
  });
  it('supports fenced and indented code and leaves unterminated fences readable', () => {
    expect(shown('~~~python\nprint(1)\n~~~')).toContain('hljs');
    expect(shown('    <b>code</b>')).toContain('&lt;b&gt;code&lt;/b&gt;');
    expect(shown('```\nstill going')).toContain('still going');
  });
  it('renders math and preserves Mermaid source while the browser loads its renderer', () => {
    expect(shown('$x^2$')).toContain('katex');
    const html = shown('```mermaid\ngraph LR\n A-->B\n```');
    expect(html).toContain('正在绘制图表');
    expect(html).toContain('A--&gt;B');
  });
  it('connects footnotes to their sanitized identifiers', () => {
    const html = shown('Text[^a]\n\n[^a]: Footnote');
    const target = /href="#([^"]+)"/.exec(html)?.[1];
    expect(target).toBeTruthy();
    expect(html).toContain(`id="${target}"`);
  });
  it('resolves relative images against the document directory', () => {
    const html = renderToStaticMarkup(<Markdown text="![plot](images/plot.png)"
      fileBase="/runs/r/files/w/reports/paper.md" />);
    expect(html).toContain('src="/runs/r/files/w/reports/images/plot.png?download=1"');
  });
});

describe('untrusted content', () => {
  it('refuses executable URL schemes while keeping normal links', () => {
    for (const url of ['javascript:alert(1)', 'data:text/html,x', 'vbscript:x', 'file:///etc/passwd']) {
      expect(saferHref(url)).toBeUndefined();
    }
    expect(shown('[click](javascript:alert%281%29)')).not.toContain('<a ');
    expect(saferHref('https://example.com')).toBe('https://example.com');
    expect(saferHref('image.png')).toBe('image.png');
  });
  it('keeps safe inline HTML while removing scripts and event handlers', () => {
    const html = shown('<strong>safe</strong><script>alert(1)</script><img src="x" onerror="alert(1)">');
    expect(html).toContain('<strong>safe</strong>');
    expect(html).not.toMatch(/<script|onerror|alert\(1\)/);
  });
});
