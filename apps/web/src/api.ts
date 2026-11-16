/** The one way this page talks to the Anchor Host.
 *
 * Its own module because two components need it: the page, and the file panel it opens inside a run. A
 * second copy of this would be a second place for the error handling to be subtly different.
 */

export class ApiError extends Error {
  constructor(message: string, readonly status: number, readonly code?: string, readonly retry?: string) { super(message); }
}

async function apiResponse(path: string, method = 'GET', body?: unknown, signal?: AbortSignal, accept = 'application/json', extraHeaders?: Record<string, string>): Promise<Response> {
  const send = () => {
    const timeout = AbortSignal.timeout(30000);
    const headers = new Headers(extraHeaders);
    headers.set('Accept', accept);
    if (body !== undefined) headers.set('Content-Type', 'application/json');
    return fetch(path, {
      method,
      signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  };
  const response = await send();
  if (!response.ok) {
    const data = await response.json().catch(() => null);
    throw new ApiError(data?.error ?? `${method} ${path} → ${response.status}`, response.status, data?.code, data?.retry);
  }
  return response;
}

export async function apiBlob(path: string, signal?: AbortSignal): Promise<Blob> {
  return (await apiResponse(path, 'GET', undefined, signal, 'application/octet-stream')).blob();
}

export async function api<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal, headers?: Record<string, string>): Promise<T> {
  const response = await apiResponse(path, method, body, signal, 'application/json', headers);
  return jsonResponse<T>(response, path, method);
}

/** Keep the Host's concurrency token with the exact representation it describes. */
export async function apiVersioned<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal, headers?: Record<string, string>): Promise<{ data: T; etag: string | null }> {
  const response = await apiResponse(path, method, body, signal, 'application/json', headers);
  return { data: await jsonResponse<T>(response, path, method), etag: response.headers.get('ETag') };
}

async function jsonResponse<T>(response: Response, path: string, method: string): Promise<T> {
  // A successful DELETE answers 204 with no body. Only an OK response may be
  // empty; anything else without a JSON body is a transport problem.
  const empty = response.status === 204 || response.status === 205;
  const data = empty ? null : await response.json().catch(() => null);
  if (data === null) {
    if (empty) return undefined as T;
    throw new Error(`${method} ${path} 未返回有效 JSON，请检查 API 服务连接。`);
  }
  return data as T;
}

/** How a fetch failure reads to a person. The server's own message when there is one. */
export function reason(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A size, at the scale a person reads it — a workspace holds both a 20-byte note and a 4MB dump. */
export function human(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
