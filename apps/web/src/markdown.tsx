/** A small subset of Markdown, rendered to React elements.
 *
 * Not a Markdown library, and the reason is what it renders: **model output and command output**, all
 * of it untrusted. Every library that produces HTML needs `dangerouslySetInnerHTML` and therefore a
 * sanitiser, and a sanitiser is a promise that a parser is correct. This returns elements, so there is
 * no HTML to inject into — the escaping is structural and nothing has to be trusted.
 *
 * The subset is what traces actually contain: fenced code, headings, list items, bold, italic, inline
 * code and links. Anything else stays the text it is, which is the honest failure for a renderer this
 * size — it shows too little formatting rather than something wrong.
 */

import type { ReactNode } from 'react';

type Block =
  | { kind: 'code'; lang: string; text: string }
  | { kind: 'heading'; level: number; text: string }
  | { kind: 'list'; ordered: boolean; items: string[] }
  | { kind: 'paragraph'; text: string };

const FENCE = /^\s*```\s*([\w+-]*)\s*$/;
const HEADING = /^(#{1,6})\s+(.*)$/;
const BULLET = /^\s*[-*+]\s+(.*)$/;
const NUMBERED = /^\s*\d+[.)]\s+(.*)$/;

/** Split into blocks. Line-based, because that is what the format is. */
export function parse(text: string): Block[] {
  const lines = text.replace(/\r\n?/g, '\n').split('\n');
  const blocks: Block[] = [];
  let paragraph: string[] = [];
  let code: string[] | null = null;
  let codeLang = '';
  let list: { ordered: boolean; items: string[] } | null = null;

  const endParagraph = () => {
    if (paragraph.length) blocks.push({ kind: 'paragraph', text: paragraph.join('\n') });
    paragraph = [];
  };
  const endList = () => {
    if (list) blocks.push({ kind: 'list', ...list });
    list = null;
  };

  for (const line of lines) {
    if (code !== null) {
      if (FENCE.test(line)) {
        blocks.push({ kind: 'code', lang: codeLang, text: code.join('\n') });
        code = null;
      } else {
        code.push(line);
      }
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      endParagraph();
      endList();
      code = [];
      codeLang = fence[1] ?? '';
      continue;
    }
    if (!line.trim()) {
      endParagraph();
      endList();
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      endParagraph();
      endList();
      blocks.push({ kind: 'heading', level: heading[1].length, text: heading[2] });
      continue;
    }
    const bullet = BULLET.exec(line);
    const numbered = NUMBERED.exec(line);
    if (bullet || numbered) {
      endParagraph();
      const ordered = Boolean(numbered);
      if (!list || list.ordered !== ordered) {
        endList();
        list = { ordered, items: [] };
      }
      list.items.push((bullet ?? numbered)![1]);
      continue;
    }
    if (list && /^\s{2,}\S/.test(line)) {
      // A wrapped line belongs to the item above it rather than starting a new block.
      list.items[list.items.length - 1] += ` ${line.trim()}`;
      continue;
    }
    endList();
    paragraph.push(line);
  }
  if (code !== null) blocks.push({ kind: 'code', lang: codeLang, text: code.join('\n') });
  endParagraph();
  endList();
  return blocks;
}

const INLINE = /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|(\*[^*\n]+\*)|(\[[^\]\n]+\]\([^)\n]+\))/g;

/** Only what a link may point at.
 *
 * The URL comes from model output, so `javascript:` and `data:` have to be refused rather than handed
 * to the DOM — `href` is an injection point like any other, and the fact that it arrived as Markdown
 * says nothing about where it goes.
 */
export function saferHref(url: string | undefined): string | undefined {
  const trimmed = (url ?? '').trim();
  return /^(https?:\/\/|#|\/)/i.test(trimmed) ? trimmed : undefined;
}

export function inline(text: string, key = 'i'): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    const at = match.index ?? 0;
    if (at > last) out.push(text.slice(last, at));
    const token = match[0];
    const id = `${key}-${at}`;
    if (token.startsWith('`')) {
      out.push(<code key={id}>{token.slice(1, -1)}</code>);
    } else if (token.startsWith('**')) {
      out.push(<strong key={id}>{token.slice(2, -2)}</strong>);
    } else if (token.startsWith('*')) {
      out.push(<em key={id}>{token.slice(1, -1)}</em>);
    } else {
      const parts = /\[([^\]]+)\]\(([^)]+)\)/.exec(token);
      const label = parts?.[1] ?? token;
      const target = saferHref(parts?.[2]);
      out.push(target
        ? <a key={id} href={target} target="_blank" rel="noreferrer noopener">{label}</a>
        : <span key={id}>{label}</span>);
    }
    last = at + token.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

export function Markdown({ text, prefix = 'm' }: { text: string; prefix?: string }) {
  return <div className="markdown">
    {parse(text).map((block, index) => {
      const key = `${prefix}-${index}`;
      if (block.kind === 'code') {
        return <pre key={key} className="code"><code>{block.text}</code></pre>;
      }
      if (block.kind === 'heading') {
        // Every heading is the same size in a transcript. What matters is that a line was meant as a
        // heading, not how deep it was nested, and six sizes in a side panel is noise.
        return <p key={key} className="md-heading">{inline(block.text, key)}</p>;
      }
      if (block.kind === 'list') {
        const items = block.items.map((item, at) => <li key={at}>{inline(item, `${key}-${at}`)}</li>);
        return block.ordered
          ? <ol key={key}>{items}</ol>
          : <ul key={key}>{items}</ul>;
      }
      return <p key={key}>{inline(block.text, key)}</p>;
    })}
  </div>;
}
