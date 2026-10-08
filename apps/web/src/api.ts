/** The one way this page talks to the Anchor Host.
 *
 * Its own module because two components need it: the page, and the file panel it opens inside a run. A
 * second copy of this would be a second place for the error handling to be subtly different.
 */

export class ApiError extends Error {
  constructor(message: string, readonly status: number) { super(message); }
}

type ApiKeyPrompt = () => Promise<string>;
let apiKeyPrompt: ApiKeyPrompt | undefined;
let pendingApiKeyPrompt: Promise<string> | undefined;

/** Register the application's accessible key dialog for API requests that receive 401. */
export function registerApiKeyPrompt(prompt: ApiKeyPrompt): () => void {
  apiKeyPrompt = prompt;
  return () => { if (apiKeyPrompt === prompt) apiKeyPrompt = undefined; };
}

export function requestApiKey(): Promise<string> {
  if (!pendingApiKeyPrompt) {
    if (!apiKeyPrompt) return Promise.reject(new Error('需要 Anchor API key；请重新打开页面后输入密钥。'));
    pendingApiKeyPrompt = apiKeyPrompt().finally(() => { pendingApiKeyPrompt = undefined; });
  }
  return pendingApiKeyPrompt;
}

export async function api<T>(path: string, method = 'GET', body?: unknown, signal?: AbortSignal): Promise<T> {
  const timeout = AbortSignal.timeout(30000);
  const send = (key: string) => fetch(path, {
    method,
    signal: signal ? AbortSignal.any([signal, timeout]) : timeout,
    headers: { Accept: 'application/json', ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      ...(key ? { Authorization: `Bearer ${key}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let key = sessionStorage.getItem('anchor-api-key') ?? '';
  let response = await send(key);
  if (response.status === 401) {
    key = await requestApiKey();
    if (key) {
      sessionStorage.setItem('anchor-api-key', key);
      response = await send(key);
    }
  }
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new ApiError(data?.error ?? `${method} ${path} → ${response.status}`, response.status);
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
