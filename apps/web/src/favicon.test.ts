import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

describe('Anchor browser tab icon', () => {
  it('links the transparent favicon to the existing Anchor mark', async () => {
    const html = await readFile(new URL('../index.html', import.meta.url), 'utf8');
    const icon = await readFile(new URL('../public/favicon.svg', import.meta.url), 'utf8');
    const source = await readFile(fileURLToPath(new URL('./App.tsx', import.meta.url)), 'utf8');

    expect(html).toContain('<link rel="icon" type="image/svg+xml" href="/favicon.svg" />');
    expect(icon).toContain('viewBox="0 0 24 24"');
    expect(icon).toContain('stroke="#167565"');
    expect(icon).toContain('<path d="M12 22V8" />');
    expect(icon).toContain('<path d="M5 12H2a10 10 0 0 0 20 0h-3" />');
    expect(icon).toContain('<circle cx="12" cy="5" r="3" />');
    expect(source).toContain("import { Anchor,");
  });
});
