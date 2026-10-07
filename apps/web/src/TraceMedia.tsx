import { useMemo, useState } from 'react';
import { ImageOff, ZoomIn } from 'lucide-react';
import { Modal, ToolButton } from './ui';
import './TraceMedia.css';

const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const MAX_TOTAL_BYTES = 20 * 1024 * 1024;
const MAX_IMAGES = 8;
const UNAVAILABLE = '\u56fe\u7247\u4e0d\u53ef\u7528';

type Part = { kind: 'text'; text: string }
  | { kind: 'image'; src: string; mime: string; bytes: number }
  | { kind: 'unavailable' };

function object(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}

export function traceParts(contents: unknown[]): Part[] {
  let images = 0, total = 0;
  return contents.map(part => {
    const wrapped = object(part);
    const content = wrapped?.type === 'content' ? object(wrapped.content) : undefined;
    if (content?.type === 'text' && typeof content.text === 'string') return { kind: 'text', text: content.text };
    if (content?.type !== 'image') return { kind: 'unavailable' };
    images += 1;
    const mime = content.mimeType, data = content.data;
    if (images > MAX_IMAGES || !['image/png', 'image/jpeg', 'image/webp'].includes(String(mime))
      || typeof data !== 'string' || !data.length || data.length > 4 * Math.ceil(MAX_IMAGE_BYTES / 3)
      || !/^[A-Za-z0-9+/]*={0,2}$/.test(data) || data.length % 4 !== 0) return { kind: 'unavailable' };
    try {
      const decoded = atob(data);
      total += decoded.length;
      if (decoded.length > MAX_IMAGE_BYTES || total > MAX_TOTAL_BYTES || btoa(decoded) !== data) return { kind: 'unavailable' };
      const matches = mime === 'image/png' ? decoded.startsWith('\x89PNG\r\n\x1a\n')
        : mime === 'image/jpeg' ? decoded.startsWith('\xff\xd8\xff')
          : decoded.startsWith('RIFF') && decoded.slice(8, 12) === 'WEBP';
      if (!matches) return { kind: 'unavailable' };
      return { kind: 'image', src: `data:${mime};base64,${data}`, mime: String(mime), bytes: decoded.length };
    } catch { return { kind: 'unavailable' }; }
  });
}

function Unavailable() {
  return <p className="trace-media-unavailable"><ImageOff size={15} aria-hidden="true" />{UNAVAILABLE}</p>;
}

function Picture({ part, index }: { part: Extract<Part, { kind: 'image' }>; index: number }) {
  const [failed, setFailed] = useState(false), [expanded, setExpanded] = useState(false);
  const alt = `\u5de5\u4f5c\u8bb0\u5f55\u56fe\u7247 ${index + 1}`;
  if (failed) return <Unavailable />;
  return <figure className="trace-picture">
    <img src={part.src} alt={alt} loading="lazy" decoding="async" referrerPolicy="no-referrer" onError={() => setFailed(true)} />
    <figcaption><span>{part.mime.slice(6).toUpperCase()}</span><span>{Math.max(1, Math.round(part.bytes / 1024))} KiB</span>
      <ToolButton icon={ZoomIn} label={'\u67e5\u770b\u539f\u56fe'} onClick={() => setExpanded(true)} /></figcaption>
    {expanded && <Modal title={alt} className="trace-picture-modal" close={() => setExpanded(false)}>
      <img src={part.src} alt={alt} referrerPolicy="no-referrer" />
    </Modal>}
  </figure>;
}

export function TraceMedia({ contents }: { contents: unknown[] }) {
  const parts = useMemo(() => traceParts(contents), [contents]);
  return <div className="trace-media">{parts.map((part, index) => part.kind === 'image'
    ? <Picture part={part} index={index} key={part.src + index} />
    : part.kind === 'text' ? <pre className="trace-media-text" key={index}>{part.text}</pre>
      : <Unavailable key={index} />)}</div>;
}
