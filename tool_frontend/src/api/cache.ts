/** Queries for the build cache and the remote cache it talks to. */

import { useQuery } from "@tanstack/react-query";

import { api } from "./client";
import type { CachePlan, CacheStatus, RemoteStatus } from "./types";

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
