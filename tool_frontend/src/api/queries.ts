/**
 * TanStack Query keys and hooks shared across pages.
 *
 * Feature-specific queries live next to their page; this file holds the ones
 * the shell itself needs (health, projects).
 */

import { useQuery } from "@tanstack/react-query";

import { api } from "./client";
import type { EditorExtension, Health, Project } from "./types";

export const queryKeys = {
  health: ["health"] as const,
  projects: ["projects"] as const,
  todos: ["todos"] as const,
  extensions: ["extensions"] as const,
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

/**
 * The packaged editor extensions this binary carries.
 *
 * Unauthenticated, and legitimately empty: a `cargo build` that skipped
 * `yarn package` serves no extensions, and the docs page offers the releases
 * page instead. Nothing here changes while the daemon is up — the files are
 * compiled into it — so this never refetches.
 */
export function useEditorExtensions() {
  return useQuery({
    queryKey: queryKeys.extensions,
    queryFn: () => api.get<EditorExtension[]>("/extensions"),
    staleTime: Infinity,
  });
}

export function useProjects() {
  return useQuery({
    queryKey: queryKeys.projects,
    queryFn: () => api.get<Project[]>("/api/projects"),
  });
}
