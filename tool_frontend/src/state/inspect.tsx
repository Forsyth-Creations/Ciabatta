/**
 * Cache inspect mode: a lens over the run graph that answers "why didn't this
 * step use the cache?" for every step at once.
 *
 * Off, the run page shows what ran. On, it shows what the cache made of each
 * step — reused, rebuilt and why, or never consulted — and lights the
 * upstream steps that forced a rebuild. The switch is in the run page's
 * header; the state is a context rather than that page's own so it survives
 * moving from one run to the next, and is remembered between visits —
 * somebody chasing a cache problem wants it on for the next run too.
 */

import { createContext, useContext, useMemo, useState } from "react";
import type { ReactNode } from "react";

const STORAGE_KEY = "ciabatta-cache-inspect";

interface InspectModeValue {
  on: boolean;
  toggle: () => void;
}

const InspectModeContext = createContext<InspectModeValue | undefined>(undefined);

function stored(): boolean {
  try {
    return localStorage.getItem(STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

export function InspectModeProvider({ children }: { children: ReactNode }) {
  const [on, setOn] = useState(stored);

  const value = useMemo<InspectModeValue>(
    () => ({
      on,
      toggle: () => {
        const next = !on;
        try {
          localStorage.setItem(STORAGE_KEY, String(next));
        } catch {
          // A private window: it just won't be remembered.
        }
        setOn(next);
      },
    }),
    [on],
  );

  return <InspectModeContext.Provider value={value}>{children}</InspectModeContext.Provider>;
}

export function useInspectMode(): InspectModeValue {
  const context = useContext(InspectModeContext);
  if (!context) throw new Error("useInspectMode must be used inside an InspectModeProvider");
  return context;
}
