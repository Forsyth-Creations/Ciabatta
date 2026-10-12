import { useCallback, useEffect, useState } from "react";

/**
 * Load something from the API whenever `key` changes.
 *
 * The page doesn't poll — that would fight with someone reading the users
 * table, and keep a headless box awake for nobody. It reloads when asked
 * (the Refresh button bumps `key`) and says how old its snapshot is.
 */
export function useLoad<T>(load: () => Promise<T>, key: unknown) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let live = true;
    setLoading(true);
    load()
      .then((value) => {
        if (!live) return;
        setData(value);
        setError(null);
      })
      .catch((e: Error) => live && setError(e.message))
      .finally(() => live && setLoading(false));
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, tick]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { data, error, loading, reload };
}
