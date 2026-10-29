/**
 * Types and queries for the monorepo: its sub-workspaces, their workflows, and
 * the compiled graph for any one of them.
 *
 * Everything here is declarations read off disk, not a scan, so the whole
 * catalogue arrives in one request and searching happens in the browser.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "./client";
import type { EnvReport } from "./types";

export interface WorkflowStep {
  name: string;
  description: string | null;
  owner: string | null;
  command: string | null;
  kind: string | null;
  workflow: string | null;
  /** The special, identifiable publishing phase. */
  push: boolean;
  needs: string[];
  requires: string[];
  timeout: string | null;
  retries: number;
  persistent: boolean;
  /** From the workflow's `background:` array: started first, gates nothing, stopped when the run ends. */
  background: boolean;
  /** From the workflow's `after:` array: runs once the rest of the run is over, whichever way it went. */
  after?: boolean;
  continue_on_error: boolean;
  /** A failure is only a warning: dependents run, the run still succeeds. */
  warn_on_error?: boolean;
  recover: boolean;
  on_error: string | null;
  /** Variables this step sets for itself, on top of the run's environment. */
  env: Record<string, string>;
  /** Variables this step's `when` / `skip_if` conditions test. Commands and
   *  scripts aren't scanned. */
  env_refs: string[];
}

/** When a workflow last actually ran, merged with what the remote cache knows. */
export interface WorkflowHistory {
  last_run_at: string;
  last_outcome: "success" | "failed" | "stopped";
  last_duration_ms: number;
  days_since: number | null;
  runs: number;
  failures: number;
  stale: boolean;
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
  /** `null` when nothing has ever recorded a run — which is not the same as stale. */
  history: WorkflowHistory | null;
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
  /** Its workflow's `wave_grouping`, when it has one. */
  wave_group?: string | null;
  /** `<member>:<workflow>:<step>` — which workflow it came from. */
  cache_id?: string | null;
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
  /**
   * Every environment variable this graph depends on, resolved against the
   * daemon's own environment — what a run started from here would see.
   */
  env: EnvReport;
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

/** One workflow's graph as its file declares it, for the graph editor. */
export interface DeclaredUnit {
  member: string;
  workflow: string;
  /** The workflow file, relative to the workspace root, when it can be edited. */
  file: string | null;
  /** Why it can't be edited here, when it can't. */
  locked: string | null;
  /** The workflow's own `needs`: other sub-workspaces' workflows. */
  needs: string[];
  /** `[workspace] depends_on`: also orders it, but lives in the config. */
  depends_on: string[];
  steps: {
    name: string;
    needs: string[];
    /** Where its `on_error` / `retry` route: those steps can't be pruned. */
    routes: string[];
    recover: boolean;
    background: boolean;
  }[];
}

export interface Declared {
  units: DeclaredUnit[];
  /** Every `<member>:<workflow>` in the workspace. */
  targets: string[];
}

/** What the files say about `units` (`<member>:<workflow>` each). */
export function useDeclared(project: string | undefined, units: string[]) {
  const key = units.join(",");
  return useQuery({
    queryKey: ["workspace", "declared", project ?? "", key] as const,
    queryFn: () =>
      api.get<Declared>(
        `/api/workspace/declared?project=${project}&units=${encodeURIComponent(key)}`,
      ),
    enabled: Boolean(project) && units.length > 0,
  });
}

/** The graph as it should be saved: every step kept, with all it needs. */
export interface UnitEdit {
  member: string;
  workflow: string;
  needs: string[];
  steps: { name: string; needs: string[] }[];
}

/** Write edited graphs back to their workflow files — all of them, or none. */
export function useSaveDeclared() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (body: { project: string; units: UnitEdit[] }) =>
      api.post<{ written: string[] }>("/api/workspace/declared", body),
    onSuccess: () => client.invalidateQueries({ queryKey: ["workspace"] }),
  });
}
