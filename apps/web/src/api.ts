/** The one way this page talks to `anchor-serve`.
 *
 * Its own module because two components need it: the page, and the file panel it opens inside a run. A
 * second copy of this would be a second place for the error handling to be subtly different.
 */

export async function api<T>(path: string, method = 'GET', body?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    headers: body === undefined ? { Accept: 'application/json' }
      : { Accept: 'application/json', 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const data = await response.json().catch(() => null);
  if (!response.ok) throw new Error(data?.error ?? `${method} ${path} → ${response.status}`);
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
