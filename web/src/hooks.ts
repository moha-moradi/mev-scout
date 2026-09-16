import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "./api";

// Poll an async loader every `intervalMs` while mounted. Returns the latest
// value, a loading flag, an error, and a manual refresh trigger.
export function usePolling<T>(
  loader: () => Promise<T>,
  intervalMs: number,
  deps: unknown[] = [],
): { data: T | null; loading: boolean; error: string | null; refresh: () => void } {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const loaderRef = useRef(loader);
  loaderRef.current = loader;
  const genRef = useRef(0);

  useEffect(() => {
    const gen = ++genRef.current;
    let alive = true;

    const pull = () => {
      loaderRef
        .current()
        .then((d) => {
          if (!alive || gen !== genRef.current) return;
          setData(d);
          setError(null);
        })
        .catch((e: unknown) => {
          if (!alive || gen !== genRef.current) return;
          setError(e instanceof ApiError ? e.detail : String(e));
        })
        .finally(() => {
          if (!alive || gen !== genRef.current) return;
          setLoading(false);
        });
    };

    pull();
    const id = setInterval(pull, intervalMs);
    return () => {
      alive = false;
      clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tick, intervalMs, ...deps]);

  const refresh = useCallback(() => setTick((t) => t + 1), []);
  return { data, loading, error, refresh };
}
