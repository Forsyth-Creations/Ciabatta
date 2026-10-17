/**
 * Subscribe to a watch session's live output.
 *
 * The daemon pushes a frame whenever lines arrive or the process exits, so
 * there is no polling timer here. Lines accumulate in a ref and are flushed
 * into React state on an animation frame — a build spewing thousands of lines a
 * second would otherwise trigger a render per frame and lock the tab up.
 */

import { useCallback, useEffect, useRef, useState } from "react";

import { api, streamUrl } from "../api/client";
import type { Bookmark, LogLine, RunStatus, Trigger, TriggerHit, WatchSnapshot } from "../api/watch";

export interface WatchStreamState {
  lines: LogLine[];
  bookmarks: Bookmark[];
  triggers: Trigger[];
  hits: TriggerHit[];
  status: RunStatus | null;
  command: string;
  running: boolean;
  /** True once the first frame has arrived. */
  ready: boolean;
  /** Set when the connection drops and can't be re-established. */
  error: string | null;
}

const EMPTY: WatchStreamState = {
  lines: [],
  bookmarks: [],
  triggers: [],
  hits: [],
  status: null,
  command: "",
  running: false,
  ready: false,
  error: null,
};

/** Hard cap on lines held in the browser, mirroring the daemon's ring buffer. */
const MAX_CLIENT_LINES = 200_000;

export function useWatchStream(
  sessionId: number,
  maxLines = MAX_CLIENT_LINES,
): WatchStreamState & { refresh: () => Promise<void> } {
  const [state, setState] = useState<WatchStreamState>(EMPTY);

  // Accumulated but not yet rendered.
  const pending = useRef<LogLine[]>([]);
  const latest = useRef<Omit<WatchStreamState, "lines">>(EMPTY);
  const frame = useRef<number | null>(null);

  useEffect(() => {
    setState(EMPTY);
    pending.current = [];

    const source = new EventSource(streamUrl(`/api/watch/sessions/${sessionId}/stream`));

    const flush = () => {
      frame.current = null;
      const incoming = pending.current;
      pending.current = [];

      setState((previous) => {
        const lines = incoming.length ? [...previous.lines, ...incoming] : previous.lines;
        return {
          ...latest.current,
          // Drop from the front once past the cap, matching the daemon's own
          // bounded buffer rather than growing without limit.
          lines: lines.length > maxLines ? lines.slice(lines.length - maxLines) : lines,
        };
      });
    };

    source.onmessage = (event) => {
      const frameData = JSON.parse(event.data) as WatchSnapshot;

      if (frameData.lines?.length) {
        pending.current.push(...frameData.lines);
      }

      latest.current = {
        bookmarks: frameData.bookmarks ?? [],
        triggers: frameData.triggers ?? [],
        hits: frameData.hits ?? [],
        status: frameData.status,
        command: frameData.command,
        running: frameData.session?.running ?? false,
        ready: true,
        error: null,
      };

      if (frame.current === null) {
        frame.current = requestAnimationFrame(flush);
      }
    };

    source.onerror = () => {
      // EventSource retries on its own; only surface it if the session is
      // already finished, where the daemon closes the stream deliberately and
      // a retry would loop forever.
      if (latest.current.ready && !latest.current.running) {
        source.close();
        return;
      }
      setState((previous) => ({ ...previous, error: "Lost the connection to the daemon." }));
    };

    return () => {
      source.close();
      if (frame.current !== null) cancelAnimationFrame(frame.current);
      frame.current = null;
    };
  }, [sessionId, maxLines]);

  /**
   * Re-read the bookmarks and triggers after changing them.
   *
   * A running session's stream sends a frame for the change, but a finished
   * one's stream has closed — so a bookmark added there never appeared until
   * the page was reloaded. Asking for no lines makes this cheap.
   */
  const refresh = useCallback(async () => {
    const snapshot = await api.get<WatchSnapshot>(`/api/watch/sessions/${sessionId}?limit=0`);
    const marks = { bookmarks: snapshot.bookmarks ?? [], triggers: snapshot.triggers ?? [] };
    latest.current = { ...latest.current, ...marks };
    setState((previous) => ({ ...previous, ...marks }));
  }, [sessionId]);

  return { ...state, refresh };
}
