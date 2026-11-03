import { Component, type ErrorInfo, type ReactNode } from 'react';

/**
 * Recovers from a stale build. Every deploy replaces the hashed chunk files, so a tab
 * opened before the deploy asks for a file that no longer exists and the dynamic import
 * rejects. Without a boundary the rejection unmounts the whole tree and leaves a blank page.
 * Reloading once picks up the current index.html and its current chunk names; the
 * sessionStorage flag prevents an infinite reload loop when a build is genuinely broken.
 */
const RELOAD_FLAG = 'anchor:chunk-reload-at';
const RELOAD_WINDOW_MS = 10_000;

export function isChunkLoadError(error: unknown): boolean {
  const msg = error instanceof Error ? error.message : String(error ?? '');
  return /Failed to fetch dynamically imported module|Importing a module script failed|error loading dynamically imported module|ChunkLoadError/i.test(msg);
}

export function reloadForStaleChunk(now = Date.now()): boolean {
  try {
    const last = Number(sessionStorage.getItem(RELOAD_FLAG) ?? 0);
    if (last && now - last < RELOAD_WINDOW_MS) return false;
    sessionStorage.setItem(RELOAD_FLAG, String(now));
  } catch { /* sessionStorage unavailable; reload once without guard */ }
  globalThis.location?.reload();
  return true;
}

type Props = { children: ReactNode };
type State = { stale: boolean };

export class ChunkBoundary extends Component<Props, State> {
  state: State = { stale: false };

  static getDerivedStateFromError(error: unknown): State {
    return { stale: isChunkLoadError(error) };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    if (!isChunkLoadError(error)) throw error;
    if (reloadForStaleChunk()) return;
    console.warn('Anchor page chunk is stale; automatic reload already ran.', info.componentStack);
  }

  render(): ReactNode {
    if (!this.state.stale) return this.props.children;
    return (
      <main className="main">
        <p role="alert">
          页面已更新，请刷新后继续。{' '}
          <button onClick={() => {
            try { sessionStorage.removeItem(RELOAD_FLAG); } catch { /* ok */ }
            globalThis.location?.reload();
          }}>刷新</button>
        </p>
      </main>
    );
  }
}
