/** The one way this page talks to `anchor-serve`.
 *
 * Its own module because two components need it: the page, and the file panel it opens inside a run. A
 * second copy of this would be a second place for the error handling to be subtly different.
 */

export async function api<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const send = (key: string) => fetch(path, {
    method,
    headers: { Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...(key ? { Authorization: `Bearer ${key}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let key = sessionStorage.getItem('anchor-api-key') ?? '';
  let response = await send(key);
  if (response.status === 401) {
    key = window.prompt('请输入 Anchor API key') ?? '';
    if (key) {
      sessionStorage.setItem('anchor-api-key', key);
      response = await send(key);
    }
  }
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new Error(data?.error ?? `${method} ${path} → ${response.status}`);
  if (data === null) throw new Error(`${method} ${path} 未返回有效 JSON，请检查 API 服务连接。`);
  return data as T;
}

export function bearerKey(): string {
  return sessionStorage.getItem('anchor-api-key') ?? '';
}

export function setBearerKey(key: string): void {
  sessionStorage.setItem('anchor-api-key', key);
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
