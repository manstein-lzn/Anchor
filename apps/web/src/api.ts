export class ApiError extends Error {
  constructor(public status: number, message: string) { super(message); }
}

let serverClockOffsetMs = 0;

export function observeServerTime(value: string | null, receivedAt = Date.now()) {
  if (!value) return;
  const observed = Number(new Date(value));
  if (Number.isFinite(observed)) serverClockOffsetMs = observed - receivedAt;
}

export function serverNow(receivedAt = Date.now()) { return receivedAt + serverClockOffsetMs; }

export async function request<T>(token: string, path: string, method = 'GET', body?: unknown, headers: Record<string, string> = {}): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method, headers: { ...headers, Authorization: `Bearer ${token}`, ...(body === undefined ? {} : { 'Content-Type': 'application/json' }) },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout(20000),
    });
    observeServerTime(response.headers.get('X-Anchor-Server-Time'));
  } catch {
    throw new Error('无法连接 API，或请求确认超时。草稿保留在当前页面，请检查连接后重试。');
  }
  const data = await response.json().catch(() => null);
  if (!response.ok) {
    const details = data?.error?.issues?.map((issue: { loc?: unknown[]; msg: string }) =>
      `${issue.loc?.join('.') ?? ''}: ${issue.msg}`).join('\n');
    throw new ApiError(response.status,
      response.status === 401 ? '认证失效，请重新连接。当前编辑内容仍保留。' :
      response.status === 409 ? '修订冲突：服务器内容已变化，或上次保存已成功但确认丢失。请先导出本地内容，再重新载入比较。' :
      details || data?.error?.message || `API 请求失败 (${response.status})`);
  }
  return data as T;
}
