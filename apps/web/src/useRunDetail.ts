import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, reason } from './api';
import type { OurRun, OurRunDetail } from './model';

export type RunDetailLoad = {
  detail: OurRunDetail | null; loading: boolean; error: string | null; retry: () => void;
};

/** Run detail belongs to the inspection page and refreshes after its Run projection changes. */
export function useRunDetail(run: string, enabled: boolean, runs: OurRun[]): RunDetailLoad {
  const selected = useMemo(() => runs.find(item => item.run === run), [run, runs]);
  const refreshKey = selected ? `${selected.updated ?? ''}/${selected.projection?.updated ?? ''}` : '';
  const active = Boolean(selected?.running || selected?.projection?.active);
  const [loaded, setLoaded] = useState<{ run: string; detail: OurRunDetail } | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = useRef({ run, enabled, refreshKey, active });
  const generation = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const inFlight = useRef(false);
  const pendingRefresh = useRef(false);
  const lastRefresh = useRef('');
  const lastActive = useRef(false);
  current.current = { run, enabled, refreshKey, active };

  const load = useCallback((clear: boolean) => {
    const requested = current.current;
    if (!requested.enabled || !requested.run) return;
    if (inFlight.current) { pendingRefresh.current = true; return; }
    const requestGeneration = generation.current;
    const requestRun = requested.run;
    const requestController = new AbortController();
    controller.current = requestController; inFlight.current = true;
    if (clear) setLoaded(null);
    setLoading(true); setError(null);
    void api<OurRunDetail>(`/runs/${encodeURIComponent(requestRun)}`, 'GET', undefined, requestController.signal)
      .then(detail => {
        if (!requestController.signal.aborted && generation.current === requestGeneration && current.current.run === requestRun) {
          setLoaded({ run: requestRun, detail });
        }
      })
      .catch(cause => {
        if (!requestController.signal.aborted && generation.current === requestGeneration && current.current.run === requestRun) {
          setError(reason(cause));
        }
      })
      .finally(() => {
        if (controller.current === requestController) controller.current = null;
        if (generation.current !== requestGeneration || current.current.run !== requestRun) return;
        inFlight.current = false; setLoading(false);
        if (pendingRefresh.current) { pendingRefresh.current = false; load(false); }
      });
  }, []);

  useEffect(() => {
    generation.current += 1; controller.current?.abort(); controller.current = null;
    inFlight.current = false; pendingRefresh.current = false; lastRefresh.current = refreshKey;
    lastActive.current = current.current.active; setError(null);
    if (!enabled || !run) { setLoaded(null); setLoading(false); return; }
    load(true);
  }, [run, enabled, load]);
  useEffect(() => {
    if (!enabled || !run) return;
    const wasActive = lastActive.current;
    lastActive.current = active;
    if (active) {
      if (lastRefresh.current !== refreshKey) { lastRefresh.current = refreshKey; load(false); }
      const timer = window.setInterval(() => load(false), 3000);
      return () => window.clearInterval(timer);
    }
    if (wasActive || lastRefresh.current !== refreshKey) { lastRefresh.current = refreshKey; load(false); }
  }, [active, enabled, load, refreshKey, run]);
  useEffect(() => () => { generation.current += 1; controller.current?.abort(); }, []);
  const retry = useCallback(() => { pendingRefresh.current = false; load(loaded?.run !== current.current.run); }, [load, loaded?.run]);
  return { detail: loaded?.run === run ? loaded.detail : null, loading, error, retry };
}
