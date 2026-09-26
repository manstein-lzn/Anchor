import { Children, isValidElement, type ReactElement } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import rehypeRaw from 'rehype-raw';
import rehypeSanitize, { defaultSchema } from 'rehype-sanitize';
import rehypeKatex from 'rehype-katex';
import rehypeHighlight from 'rehype-highlight';
import 'katex/dist/katex.min.css';
import 'highlight.js/styles/github.css';
import { MermaidDiagram } from './MermaidDiagram';
import { ANCHOR_PREFIX } from './links';

// Sanitize user HTML before trusted math/highlighting plugins produce their own markup.
const schema = {
  ...defaultSchema,
  attributes: {
    ...defaultSchema.attributes,
    code: [...(defaultSchema.attributes?.code ?? []), ['className', /^language-./, 'math-inline', 'math-display']],
  },
};

export function saferHref(url: string | undefined): string | undefined {
  const value = (url ?? '').trim();
  if (!value || /[\u0000-\u0020\u007f]/.test(value)) return undefined;
  return /^(https?:\/\/|mailto:|#|\/)/i.test(value) || !/^[^/?#]*:/.test(value) ? value : undefined;
}

/** CommonMark + GFM, with sanitized HTML and explicit renderers for executable-looking content. */
export function Markdown({ text, prefix = 'm', fileBase }:
  { text: string; prefix?: string; fileBase?: string }) {
  const resolve = (url: string) => {
    const safe = saferHref(url);
    // Anchor references are handled by the workbench, not by an element id, so they must not be
    // rewritten into the sanitizer's `user-content-` namespace.
    if (safe?.startsWith(ANCHOR_PREFIX)) return safe;
    if (safe?.startsWith('#')) return `#user-content-${safe.slice(1)}`;
    if (!safe || !fileBase || /^(?:[a-z]+:|\/|#)/i.test(safe)) return safe ?? '';
    // Relative images and attachments are relative to this Markdown file, not the web application.
    const target = new URL(safe, new URL(fileBase, 'https://anchor.invalid'));
    target.searchParams.set('download', '1');
    return target.pathname + target.search + target.hash;
  };
  return <div className="markdown">
    <ReactMarkdown remarkPlugins={[remarkGfm, remarkMath]}
      rehypePlugins={[rehypeRaw, [rehypeSanitize, schema], [rehypeKatex, { trust: false, strict: 'ignore' }], rehypeHighlight]}
      remarkRehypeOptions={{ clobberPrefix: `md-${prefix}-` }}
      urlTransform={resolve}
      components={{
        a: ({ node: _node, href, children, ...props }) => href
          ? <a {...props} href={href} target={href.startsWith('#') ? undefined : '_blank'} rel="noreferrer noopener">{children}</a>
          : <span>{children}</span>,
        img: ({ node: _node, ...props }) => <img {...props} loading="lazy" />,
        table: ({ node: _node, ...props }) => <div className="markdown-table"><table {...props} /></div>,
        pre: ({ children }) => {
          const child = Children.toArray(children)[0];
          if (isValidElement(child)) {
            const code = child as ReactElement<{ className?: string; children?: string }>;
            if (code.props.className?.split(' ').includes('language-mermaid')) {
              return <MermaidDiagram source={String(code.props.children ?? '').replace(/\n$/, '')} />;
            }
          }
          return <pre className="code">{children}</pre>;
        },
      }}>{text}</ReactMarkdown>
  </div>;
}
