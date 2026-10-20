/** Queries for the build cache and the remote cache it talks to. */

import { useMutation, useQuery } from "@tanstack/react-query";

import { api } from "./client";
import type { CachePlan, CacheStatus, FileHash, RemoteStatus } from "./types";

export const cacheKeys = {
  status: (project: string) => ["cache", "status", project] as const,
  plan: (project: string, target: string | null) => ["cache", "plan", project, target] as const,
  remote: (project: string) => ["cache", "remote", project] as const,
};

/** What this project's local cache holds, and how it's configured. */
export function useCacheStatus(project: string | undefined) {
  return useQuery({
    queryKey: cacheKeys.status(project ?? ""),
    queryFn: () => api.get<CacheStatus>(`/api/cache/status?project=${project}`),
    enabled: Boolean(project),
  });
}

/**
 * What a run would reuse and what it would rebuild.
 *
 * Not cached for long: the whole point is to reflect the working tree as it is
 * right now, and a stale plan is worse than no plan — somebody would act on it.
 */
export function useCachePlan(project: string | undefined, target: string | null) {
  return useQuery({
    queryKey: cacheKeys.plan(project ?? "", target),
    queryFn: () => {
      const params = new URLSearchParams({ project: project ?? "" });
      if (target) params.set("target", target);
      return api.get<CachePlan>(`/api/cache/plan?${params}`);
    },
    enabled: Boolean(project),
    staleTime: 0,
    gcTime: 0,
  });
}

/** The configured remote cache's status, proxied through the daemon. */
export function useRemoteCache(project: string | undefined) {
  return useQuery({
    queryKey: cacheKeys.remote(project ?? ""),
    queryFn: () => api.get<RemoteStatus>(`/api/cache/remote?project=${project}`),
    enabled: Boolean(project),
    refetchInterval: 15_000,
  });
}

/** Render milliseconds the way somebody would say them out loud. */
export function humanizeMs(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

/** Render a byte count the way a person would say it. */
export function humanizeBytes(bytes: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
}

/** Every file the cache saw under one key, each with its hash. */
export interface CacheKeyFiles {
  key: string;
  target: string;
  workspace: string;
  /** Whether an entry was stored under this key. When false, the inputs are
   *  what the run keyed the step on and the outputs are as they stand on disk
   *  right now — the step is still building, failed, or stores nothing. */
  stored: boolean;
  at: string;
  algorithm: string;
  inputs: FileHash[];
  outputs: FileHash[];
  env: string[];
  upstream: Record<string, string>;
}

/**
 * The files behind a cache key, fetched when a step is opened rather than
 * carried in the run's stream. Refetched while the step may still be writing
 * its outputs, so the list fills in as it builds.
 */
export function useCacheKeyFiles(
  project: string | undefined,
  key: string | null | undefined,
  live: boolean,
) {
  return useQuery({
    queryKey: ["cache", "files", project ?? "", key ?? ""] as const,
    queryFn: () =>
      api.get<CacheKeyFiles>(
        `/api/cache/files?${new URLSearchParams({ project: project ?? "", key: key ?? "" })}`,
      ),
    enabled: Boolean(project && key),
    refetchInterval: live ? 3_000 : false,
    retry: false,
  });
}

/** The checkout as the file picker browses it: what git wouldn't ignore. */
export interface ProjectTree {
  /** The workspace root every path is relative to. */
  root: string;
  /** Every file, git-ignored ones included — those listed after the rest. */
  files: string[];
  dirs: string[];
  /** The top of each subtree git ignores (`target`, `node_modules`, `*.log`
   *  files): listed so outputs can be picked from them, and marked. */
  ignored?: string[];
  /** More files than the daemon sends; the ones git keeps come first. */
  truncated: boolean;
}

/** Every file and directory of the checkout, for picking a step's inputs and
 *  outputs. Fetched when the picker opens, not before. */
export function useProjectTree(project: string | undefined, enabled: boolean) {
  return useQuery({
    queryKey: ["cache", "tree", project ?? ""] as const,
    queryFn: () => api.get<ProjectTree>(`/api/cache/tree?project=${project}`),
    enabled: Boolean(project) && enabled,
    staleTime: 30_000,
  });
}

/** Write a step's picked inputs and outputs into its workflow file's `cache:`.
 *  Resolves to the file written, relative to the workspace root. */
export function useSaveTargetFiles() {
  return useMutation({
    mutationFn: (body: {
      project: string;
      target: string;
      inputs: string[];
      /** Cache ids of earlier steps whose outputs it reads (`cache.inputs_from`). */
      inputs_from: string[];
      outputs: string[];
    }) =>
      api.post<{ path: string }>("/api/cache/target-files", body),
  });
}
