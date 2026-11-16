/** Types and queries for watch sessions. */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";

export type LineStream = "stdout" | "stderr";

export interface LogLine {
  seq: number;
  ts: string;
  stream: LineStream;
  text: string;
}

export interface Bookmark {
  id: number;
  seq: number;
  label: string;
  note: string | null;
  snippet: string;
  created_at: string;
}

export interface Trigger {
  id: number;
  pattern: string;
  is_regex: boolean;
  hits: number;
}

export interface TriggerHit {
  trigger_id: number;
  seq: number;
  ts: string;
  text: string;
}

export type RunStatus =
  | { kind: "running" }
  | { kind: "exited"; code: number }
  | { kind: "signaled" }
  | { kind: "failed"; code: string };

export interface SessionSummary {
  id: number;
  project: string;
  command: string;
  /**
   * What this session is, when its command line doesn't say.
   */
  label: string | null;
  created_at: string;
  running: boolean;
  lines: number;
}

/** The snapshot payload, also the shape of each SSE frame. */
export interface WatchSnapshot {
  command: string;
  started_at: string;
  status: RunStatus;
  total_lines: number;
  buffered_lines: number;
  next_seq: number;
  lines: LogLine[];
  bookmarks: Bookmark[];
  triggers: Trigger[];
  hits: TriggerHit[];
  session: SessionSummary;
}

export const watchKeys = {
  sessions: ["watch", "sessions"] as const,
  session: (id: number) => ["watch", "session", id] as const,
};

export function useWatchSessions() {
  return useQuery({
    queryKey: watchKeys.sessions,
    queryFn: () => api.get<SessionSummary[]>("/api/watch/sessions"),
    refetchInterval: 3_000,
  });
}

export function useCreateSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { project: string; command: string; triggers: string[] }) =>
      api.post<SessionSummary>("/api/watch/sessions", body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: watchKeys.sessions }),
  });
}

export function useStopSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => api.post(`/api/watch/sessions/${id}/stop`),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: watchKeys.sessions }),
  });
}

export function useCloseSession() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => api.delete(`/api/watch/sessions/${id}`),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: watchKeys.sessions }),
  });
}

export interface SearchResult {
  lines: LogLine[];
  total: number;
  capped: boolean;
}

export function useSearch(id: number, query: string, mode: "any" | "all", regex: boolean) {
  return useQuery({
    queryKey: ["watch", "search", id, query, mode, regex],
    queryFn: () => {
      const params = new URLSearchParams({ q: query, mode, regex: regex ? "true" : "false" });
      return api.get<SearchResult>(`/api/watch/sessions/${id}/search?${params}`);
    },
    enabled: query.trim().length > 0,
  });
}

// ─── Triage ──────────────────────────────────────────────────────────────────

export type TriageKind = "only_a" | "only_b" | "similar" | "common";

/** Where one line shape occurs in one log. */
export interface Occurrences {
  /** The line with its timestamps, numbers and ids masked. */
  template: string;
  count: number;
  stderr: number;
  /** The first few hundred line numbers it appears at. */
  seqs: number[];
  /** The first occurrence, as printed. */
  sample: string;
}

export interface DiffPart {
  op: "same" | "a" | "b";
  text: string;
}

export interface TriageGroup {
  kind: TriageKind;
  similarity: number;
  a: Occurrences | null;
  b: Occurrences | null;
  diff?: DiffPart[];
  count_changed: boolean;
}

export interface TriageSummary {
  lines_a: number;
  lines_b: number;
  templates_a: number;
  templates_b: number;
  only_a: number;
  only_b: number;
  only_a_lines: number;
  only_b_lines: number;
  similar: number;
  common: number;
  count_changed: number;
  capped: TriageKind[];
  fuzzy_incomplete: boolean;
}

export interface TriageReport {
  summary: TriageSummary;
  groups: TriageGroup[];
  session_a: SessionSummary;
  session_b: SessionSummary;
}

/**
 * Compare two sessions. The daemon does the work over both whole buffers;
 * `revision` lets the page ask again for sessions that are still producing
 * output, without the query refetching by itself while you read.
 */
export function useTriage(
  a: number | undefined,
  b: number | undefined,
  threshold: number,
  ignore: string,
  revision: number,
) {
  return useQuery({
    queryKey: ["watch", "triage", a, b, threshold, ignore, revision],
    queryFn: () => {
      const params = new URLSearchParams({
        a: String(a),
        b: String(b),
        threshold: String(threshold),
      });
      if (ignore.trim()) params.set("ignore", ignore);
      return api.get<TriageReport>(`/api/watch/triage?${params}`);
    },
    enabled: a !== undefined && b !== undefined && a !== b,
    placeholderData: (previous) => previous,
    staleTime: Infinity,
    retry: false,
  });
}
