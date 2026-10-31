/** @vitest-environment jsdom */
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Files } from './Files';
import { PilotAttachments } from './PilotAttachments';

describe('file downloads', () => {
  let root: Root;
  let container: HTMLDivElement;
  const fetchMock = vi.fn();
  const createObjectURL = vi.fn(() => 'blob:download');
  const revokeObjectURL = vi.fn();
  beforeEach(() => {
    vi.stubGlobal('IS_REACT_ACT_ENVIRONMENT', true);
    vi.stubGlobal('fetch', fetchMock);
    Object.defineProperty(URL, 'createObjectURL', { value: createObjectURL, configurable: true });
    Object.defineProperty(URL, 'revokeObjectURL', { value: revokeObjectURL, configurable: true });
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
    fetchMock.mockReset(); createObjectURL.mockClear(); revokeObjectURL.mockClear();
    container = document.createElement('div'); document.body.append(container); root = createRoot(container);
  });
  afterEach(async () => { await act(async () => root.unmount()); container.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

  it('downloads the fixed delivery and revokes its object URL on unmount', async () => {
    const artifact = { id: 'fs2-frozen', node_id: 'work', invocation: 2 };
    fetchMock.mockResolvedValueOnce(Response.json({ node: 'work', artifact, artifact_availability: 'available', files: [{ path: 'folder/a b.txt', size: 4 }], truncated: false }))
      .mockResolvedValueOnce(new Response('data', { headers: { 'Content-Type': 'text/plain' } }));
    await act(async () => root.render(<Files run="run" node="work" artifact={artifact} />));
    expect(fetchMock.mock.calls[0][0]).toBe('/api/artifacts/fs2-frozen/files');
    await act(async () => (container.querySelector('[aria-label="下载 a b.txt"]') as HTMLButtonElement).click());
    const [path, init] = fetchMock.mock.calls.at(-1)!;
    expect(path).toBe('/api/artifacts/fs2-frozen/files/folder/a%20b.txt?download=1');
    expect(new Headers(init.headers).has('Authorization')).toBe(false);
    expect(createObjectURL).toHaveBeenCalledOnce();
    await act(async () => root.render(null));
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:download');
  });

  it('keeps the inventory when delivered bytes expire during preview', async () => {
    const artifact = { id: 'fs2-frozen', node_id: 'work', invocation: 2 };
    fetchMock.mockResolvedValueOnce(Response.json({ artifact, artifact_availability: 'available', files: [{ path: 'report.md', size: 4 }], truncated: false }))
      .mockResolvedValueOnce(Response.json({ error: 'expired bytes' }, { status: 410 }));
    await act(async () => root.render(<Files run="run" node="work" artifact={artifact} targetPath="report.md" />));
    expect(container.querySelector('[role="alert"]')?.textContent).toContain('交付文件已过期');
    expect(container.querySelector('.file-name')?.textContent).toContain('report.md');
    expect(fetchMock.mock.calls.at(-1)?.[0]).toBe('/api/artifacts/fs2-frozen/files/report.md');
  });

  it('shows the same download failure and permits retry for a Pilot attachment', async () => {
    fetchMock.mockResolvedValueOnce(Response.json({ error: 'attachment unavailable' }, { status: 409 }))
      .mockResolvedValueOnce(new Response('restored'));
    const attachment = { id: 'a', name: 'report.txt', size: 4, sha256: 'a'.repeat(64), media_type: 'text/plain',
      url: '/sessions/s/turns/t/attachments/a', download_url: '/sessions/s/turns/t/attachments/a?download=1' };
    await act(async () => root.render(<PilotAttachments attachments={[attachment]} session="s" turn="t" />));
    const button = container.querySelector('[aria-label="下载 report.txt"]') as HTMLButtonElement;
    await act(async () => button.click());
    expect(container.querySelector('[role="alert"]')?.textContent).toBe('attachment unavailable');
    expect(button.disabled).toBe(false);
    await act(async () => button.click());
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(createObjectURL).toHaveBeenCalledOnce();
  });
});
