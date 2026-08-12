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
   * What this session is, when its command line doesn't say. A `persistent`
   * workflow step sets it to its graph node id, so the session it leaves
   * running after a build is identifiable as that step.
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
