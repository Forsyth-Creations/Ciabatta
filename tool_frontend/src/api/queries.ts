/**
 * TanStack Query keys and hooks shared across pages.
 *
 * Feature-specific queries live next to their page; this file holds the ones
 * the shell itself needs (health, projects).
 */

import { useQuery } from "@tanstack/react-query";

import { api } from "./client";
import type { Health, Project } from "./types";

export const queryKeys = {
  health: ["health"] as const,
  projects: ["projects"] as const,
  todos: ["todos"] as const,
};

/**
 * Poll the daemon's health endpoint.
 *
 * This is the one query that should keep retrying quietly: if the daemon is
 * restarted underneath the page, we want the indicator to recover on its own
 * rather than leaving the UI stuck on an error.
 */
export function useHealth() {
  return useQuery({
    queryKey: queryKeys.health,
    queryFn: () => api.get<Health>("/api/health"),
    refetchInterval: 10_000,
    retry: true,
    staleTime: 5_000,
  });
}

export function useProjects() {
  return useQuery({
    queryKey: queryKeys.projects,
    queryFn: () => api.get<Project[]>("/api/projects"),
  });
}
