import { afterEach, describe, expect, it, vi } from 'vitest';
import { ApiError, request } from './api';

afterEach(() => vi.unstubAllGlobals());
describe('API transport', () => {
  it('sends credentials only in the authorization header', async () => {
    const fetch = vi.fn().mockResolvedValue(new Response('{"revision":1}'));
    vi.stubGlobal('fetch', fetch);
    expect(await request('test-secret', '/api/graphs/g/draft', 'PUT', { expected_revision: 0 })).toEqual({ revision: 1 });
    expect(fetch.mock.calls[0][0]).toBe('/api/graphs/g/draft');
    expect(fetch.mock.calls[0][1].headers.Authorization).toBe('Bearer test-secret');
    expect(fetch.mock.calls[0][1].body).not.toContain('test-secret');
  });
  it('distinguishes optimistic conflicts', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 409 })));
    await expect(request('token', '/api/graphs/g/draft')).rejects.toMatchObject({ status: 409 });
  });
  it('exposes sanitized schema paths, not submitted data', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: { issues: [{ loc: ['body', 'nodes', 0], msg: 'Missing reference' }] } }), { status: 422 })));
    await expect(request('token', '/api/graphs/validate')).rejects.toThrow('body.nodes.0: Missing reference');
  });
  it('does not treat a lost acknowledgement as a failed server write', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new TypeError('network')));
    await expect(request('token', '/api/graphs/g/draft')).rejects.toThrow('请求确认超时');
  });
  it('requires reauthentication on 401', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('{}', { status: 401 })));
    await expect(request('token', '/api/graphs')).rejects.toBeInstanceOf(ApiError);
  });
});
