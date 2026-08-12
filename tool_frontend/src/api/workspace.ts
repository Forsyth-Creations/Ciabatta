/**
 * Types and queries for the monorepo: its sub-workspaces, their workflows, and
 * the compiled graph for any one of them.
 *
 * Everything here is declarations read off disk, not a scan, so the whole
 * catalogue arrives in one request and searching happens in the browser.
 */

import { useQuery } from "@tanstack/react-query";

import { api } from "./client";

export interface WorkflowStep {
  name: string;
  description: string | null;
  owner: string | null;
  command: string | null;
  kind: string | null;
  recipe: string | null;
  /** The special, identifiable publishing phase. */
  push: boolean;
  needs: string[];
  requires: string[];
  timeout: string | null;
  retries: number;
  persistent: boolean;
  continue_on_error: boolean;
  recover: boolean;
  on_error: string | null;
}

export interface WorkflowSummary {
  name: string;
  description: string | null;
  owner: string;
  /** Cross-package dependencies, as `"member"` or `"member:workflow"`. */
  needs: string[];
  requires: string[];
  tags: string[];
  required_env: string[];
  steps: WorkflowStep[];
}

export interface MemberSummary {
  name: string;
  /** Path relative to the monorepo root. */
  path: string;
  dir: string;
  description: string | null;
  owner: string;
  tags: string[];
  depends_on: string[];
  workflows: WorkflowSummary[];
}

export interface ToolSummary {
  tool: string;
  hint: string | null;
  description: string | null;
}

export interface WorkspaceCatalogue {
  root: string;
  members: MemberSummary[];
  /** Every distinct workflow name defined anywhere in the repo. */
  workflows: string[];
  toolchain: ToolSummary[];
}

export interface GraphNode extends WorkflowStep {
  id: string;
  /** Which sub-workspace this node came from. */
  workspace: string | null;
  cwd: string | null;
  /** Its dependency layer, or null for a recovery node. */
  wave: number | null;
}

export interface GraphEdge {
  from: string;
  to: string;
  kind: "needs" | "on_error";
}

export interface MissingTool {
  tool: string;
  hint: string | null;
  needed_by: string[];
}

export interface WorkflowGraph {
  workflow: string;
  root: string;
  empty: boolean;
  units: { member: string; workflow: string; steps: number }[];
  nodes: GraphNode[];
  edges: GraphEdge[];
  /** Node ids per dependency layer — the order the graph is drawn in. */
  waves: string[][];
  missing_tools: MissingTool[];
}

export const workspaceKeys = {
  catalogue: (project: string) => ["workspace", "catalogue", project] as const,
  graph: (project: string, workflow: string) =>
    ["workspace", "graph", project, workflow] as const,
};

export function useWorkspace(project: string) {
  return useQuery({
    queryKey: workspaceKeys.catalogue(project),
    queryFn: () =>
      api.get<WorkspaceCatalogue>(`/api/workspace?project=${encodeURIComponent(project)}`),
    // Declarations change when someone edits a TOML, not continuously.
    staleTime: 30_000,
    retry: false,
  });
}

/** One `.env` variable that moved since ciabatta last ran in this project. */
export interface EnvChange {
  kind: "added" | "removed" | "changed" | "file_added" | "file_removed";
  file: string;
  key?: string;
  keys?: number;
}

export interface EnvDrift {
  /** The `.env` files this project sources. */
  watching: string[];
  /** True the first time a project is seen — everything looks new, nothing moved. */
  first_run: boolean;
  /** Whether there is anything worth telling the user about. */
  noteworthy: boolean;
  files: string[];
  changes: EnvChange[];
  summary: string | null;
  checked_at: string | null;
}

/**
 * Which environment variables have changed since ciabatta last ran here.
 *
 * A read-only peek: polling this never acknowledges the drift, so the terminal
 * run that follows still reports it. Whoever never opens this tab is precisely
 * the person who most needs telling.
 */
export function useEnvDrift(project: string) {
  return useQuery({
    queryKey: ["workspace", "env-drift", project] as const,
    queryFn: () =>
      api.get<EnvDrift>(`/api/workspace/env-drift?project=${encodeURIComponent(project)}`),
    // A pull happens while you're looking at another window, so this is worth
    // re-checking on focus rather than only on mount.
    refetchOnWindowFocus: true,
    staleTime: 10_000,
    retry: false,
  });
}

export function useWorkflowGraph(project: string, workflow: string | null) {
  return useQuery({
    queryKey: workspaceKeys.graph(project, workflow ?? ""),
    queryFn: () =>
      api.get<WorkflowGraph>(
        `/api/workspace/graph?project=${encodeURIComponent(project)}&workflow=${encodeURIComponent(
          workflow ?? "",
        )}`,
      ),
    enabled: Boolean(workflow),
    retry: false,
  });
}

/**
 * Whether anything about a workflow matches a search term.
 *
 * Deliberately generous — names, descriptions, owners, tags, and the commands
 * steps actually run — because "which package runs `protoc`?" is exactly the
 * question a monorepo can't otherwise answer.
 */
export function workflowMatches(
  member: MemberSummary,
  workflow: WorkflowSummary,
  term: string,
): boolean {
  const needle = term.trim().toLowerCase();
  if (!needle) return true;

  const haystack = [
    member.name,
    member.description ?? "",
    member.owner,
    ...member.tags,
    workflow.name,
    workflow.description ?? "",
    workflow.owner,
    ...workflow.tags,
    ...workflow.steps.flatMap((step) => [
      step.name,
      step.description ?? "",
      step.command ?? "",
      ...step.requires,
    ]),
  ];
  return haystack.some((value) => value.toLowerCase().includes(needle));
}
