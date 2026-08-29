/** Types and queries for runs. */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "./client";
import type { EnvReport } from "./types";
import { useWorkspace } from "./workspace";

export type StepStatus = "pending" | "running" | "success" | "failed" | "skipped";

export interface StepView {
  name: string;
  status: StepStatus;
  /** Recovery nodes are the "fix-it" branches a failure can divert into. */
  recover: boolean;
  action: string | null;
  needs: string[];
  on_error: string | null;
  logs: string[];

  // Set when the run is a monorepo workflow graph, whose nodes come from
  // several packages at once and have to say which.
  workspace: string | null;
  description: string | null;
  owner: string | null;
  kind: string | null;
  /** The special, identifiable publishing phase. */
  push: boolean;
  persistent: boolean;
  timeout: string | null;
  requires: string[];

  /** Variables this step sets for itself, on top of the run's environment. */
  env: Record<string, string>;
  /** Variables this step reads — in its command, cwd, or conditions. */
  env_refs: string[];
  /**
   * The `.env` files this step resolves through, outermost first — its own
   * workspace's last, since the nearest file wins.
   *
   * Empty for a step that just sees the run's environment, which is every step
   * of a plain single-project workflow.
   */
  env_files: string[];

  /** The five things this target is defined by. */
  deps: TargetDeps;
}

/**
 * Everything a target depends on, and everything it produces.
 *
 * The graph already draws `needs`. The other four — the files it reads, the
 * files it writes, the variables it keys on, and the commands it runs — were
 * only ever visible by opening the config, which is exactly when somebody is
 * asking why a step rebuilt.
 */
export interface TargetDeps {
  name: string;
  /** The sub-workspace it came from, when the run is a monorepo graph. */
  workspace: string | null;
  /** Where its globs resolve from, relative to the project root. */
  dir: string;

  /** The commands it runs, as they go into its cache key. */
  commands: string[];

  /** The globs it declares, its own or the ones it inherited. */
  inputs: string[];
  outputs: string[];
  /** Excluded from its inputs — including sub-workspaces excluded for it. */
  exclude: string[];

  /** What those globs currently match. */
  input_files: number;
  input_bytes: number;
  output_files: number;
  output_bytes: number;

  /** Variables folded into its cache key. */
  env: string[];
  /** Variables it reads without declaring — not in the key, and so a risk. */
  env_refs: string[];

  needs: string[];

  cached: boolean;
  why_uncached: string | null;
}

/** The variables a target reads but never declared, so a change to one of them
 *  would not invalidate its cache entry. */
export function undeclaredEnv(deps: TargetDeps): string[] {
  return deps.env_refs.filter((key) => !deps.env.includes(key));
}

export interface EdgeView {
  from: string;
  to: string;
  /** `needs` (normal dependency), `error` (failure branch), or `retry`. */
  kind: "needs" | "error" | "retry";
}

export interface StageView {
  name: string;
  status: string;
}

export interface PendingChoice {
  step: string;
  message: string;
  options: string[];
}

export interface WorkflowView {
  name: string;
  status: string;
  error: string | null;
  stages: StageView[];
  steps: StepView[];
  edges: EdgeView[];
  logs: string[];
  pending: PendingChoice | null;
  /**
   * The environment this run started with, resolved once when it was created:
   * every variable its steps depend on, with the value they see.
   */
  env: EnvReport;
}

export interface RunSummary {
  id: number;
  project: string;
  workflows: string[];
  created_at: string;
  done: boolean;
}

export interface RunState {
  workflows: WorkflowView[];
  done: boolean;
  dry_run: boolean;
  run: RunSummary;
  seq: number;
}

export const runKeys = {
  runs: ["run", "runs"] as const,
  run: (id: number) => ["run", "run", id] as const,
  workflows: (project: string) => ["run", "workflows", project] as const,
};

export function useRuns() {
  return useQuery({
    queryKey: runKeys.runs,
    queryFn: () => api.get<RunSummary[]>("/api/run/runs"),
    refetchInterval: 3_000,
  });
}

export function useRunWorkflows(project: string) {
  return useQuery({
    queryKey: runKeys.workflows(project),
    queryFn: () =>
      api.get<{ workflows: string[] }>(
        `/api/run/workflows?project=${encodeURIComponent(project)}`,
      ),
    select: (data) => data.workflows,
  });
}

/**
 * Everything runnable in this project: every workflow the monorepo declares.
 *
 * A workflow is the only kind of thing ciabatta runs, so this is the whole
 * list — `build` across every package that defines one.
 */
export function useRunTargets(project: string) {
  const declared = useRunWorkflows(project);
  const workspace = useWorkspace(project);

  const targets: RunTarget[] = [];
  for (const name of workspace.data?.workflows ?? []) {
    const members = (workspace.data?.members ?? [])
      .filter((member) => member.workflows.some((w) => w.name === name))
      .map((member) => member.name);
    // The first description anyone wrote for this workflow name. They should
    // agree across packages; when they don't, one is still better than none.
    const description =
      (workspace.data?.members ?? [])
        .flatMap((member) => member.workflows)
        .find((w) => w.name === name && w.description)?.description ?? null;
    targets.push({ name, kind: "workflow", description, members });
  }
  // A project that isn't a monorepo still declares workflows inline; those come
  // back from the run API rather than the workspace walk.
  for (const name of declared.data ?? []) {
    if (!targets.some((target) => target.name === name)) {
      targets.push({ name, kind: "workflow", description: null, members: [] });
    }
  }

  return {
    targets,
    // The workspace query fails on a project that isn't a monorepo, which is
    // an ordinary state rather than an error.
    isLoading: declared.isLoading || workspace.isLoading,
  };
}

export interface StartRunBody {
  project: string;
  dry_run: boolean;
  /** Values for variables the run needs but the daemon's environment lacks. */
  env?: Record<string, string>;
  /**
   * The workflow to run. The daemon compiles the cross-workspace graph itself,
   * so a run started here and one started by `ciabatta build` can't disagree
   * about what runs.
   */
  workflow?: string;
  /** Further workflows folded into the same graph, as `ciabatta build test`. */
  workflows?: string[];
  /** With `workflow`: start only from these sub-workspaces. */
  only?: string[];
  /** With `workflow`: don't follow dependencies into other sub-workspaces. */
  isolated?: boolean;
  /** With `workflow`: run only the steps these terms select (CLI `--filter`). */
  filter?: string[];
}

/** One thing this project can run. */
export interface RunTarget {
  name: string;
  kind: "workflow";
  description: string | null;
  /** Which sub-workspaces define this workflow. */
  members: string[];
}

export function useStartRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: StartRunBody) => api.post<RunSummary>("/api/run/runs", body),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: runKeys.runs }),
  });
}

/**
 * The variables a rejected start is waiting on, or null if it failed for some
 * other reason.
 *
 * The daemon answers 422 with a `missing_env` list rather than starting a run
 * that would abort at its own `REQUIRED_ENV` gate, so the launcher can ask for
 * the values and post again.
 */
export function missingEnvFrom(error: unknown): string[] | null {
  if (!(error instanceof ApiError) || error.status !== 422) return null;
  const missing = error.body?.missing_env;
  return Array.isArray(missing) && missing.length > 0 ? (missing as string[]) : null;
}

export function useChoose(runId: number) {
  return useMutation({
    mutationFn: (body: { workflow: string; step: string; option: number }) =>
      api.post(`/api/run/runs/${runId}/choose`, body),
  });
}
