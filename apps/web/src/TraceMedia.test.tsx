import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { TraceMedia, traceParts } from './TraceMedia';

const PNG = 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADUlEQVQIHWP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC';
const picture = (mimeType = 'image/png', data = PNG) => ({ type: 'content', content: { type: 'image', mimeType, data } });
const text = (value: string) => ({ type: 'content', content: { type: 'text', text: value } });

describe('native trace media', () => {
  it('preserves mixed text and image order without printing base64', () => {
    const parts = traceParts([text('before'), picture(), text('after')]);
    expect(parts.map(part => part.kind)).toEqual(['text', 'image', 'text']);
    const html = renderToStaticMarkup(<TraceMedia contents={[text('before'), picture(), text('after')]} />);
    expect(html.indexOf('before')).toBeLessThan(html.indexOf('<img'));
    expect(html.indexOf('<img')).toBeLessThan(html.indexOf('after'));
    expect(html).toContain(`src="data:image/png;base64,${PNG}"`);
    expect(html).not.toContain(`<pre>${PNG}`);
    expect(html).toContain('referrerPolicy="no-referrer"');
    expect(html).toContain('aria-label="\u67e5\u770b\u539f\u56fe"');
  });
  it.each([
    picture('image/svg+xml', btoa('<svg onload="alert(1)"></svg>')),
    picture('image/png', btoa('<svg><image href="https://evil.test/a"/></svg>')),
    picture('image/jpeg'), picture('image/png', 'https://evil.test/a.png'),
    picture('image/png', 'AAEC'), picture('image/png', PNG + '='),
    { type: 'content', content: { type: 'resource_link', uri: 'file:///etc/passwd' } },
  ])('never embeds unsupported, mismatched, corrupt or remote media', part => {
    expect(traceParts([part])).toEqual([{ kind: 'unavailable' }]);
    const html = renderToStaticMarkup(<TraceMedia contents={[part]} />);
    expect(html).not.toContain('<img'); expect(html).not.toContain('evil.test');
    expect(html).not.toContain('file:///');
  });
  it('enforces per-image and count limits', () => {
    expect(traceParts([picture('image/png', 'A'.repeat(14 * 1024 * 1024))])).toEqual([{ kind: 'unavailable' }]);
    const parts = traceParts(Array.from({ length: 9 }, () => picture()));
    expect(parts.filter(part => part.kind === 'image')).toHaveLength(8);
    expect(parts[8]).toEqual({ kind: 'unavailable' });
  });
});
