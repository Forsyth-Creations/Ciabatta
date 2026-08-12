/** Types and queries for runs. */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "./client";
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

export interface RecipeView {
  name: string;
  status: string;
  error: string | null;
  stages: StageView[];
  steps: StepView[];
  edges: EdgeView[];
  logs: string[];
  pending: PendingChoice | null;
}

export interface RunSummary {
  id: number;
  project: string;
  recipes: string[];
  created_at: string;
  done: boolean;
}

export interface RunState {
  recipes: RecipeView[];
  done: boolean;
  dry_run: boolean;
  run: RunSummary;
  seq: number;
}

export const runKeys = {
  runs: ["run", "runs"] as const,
  run: (id: number) => ["run", "run", id] as const,
  recipes: (project: string) => ["run", "recipes", project] as const,
};

export function useRuns() {
  return useQuery({
    queryKey: runKeys.runs,
    queryFn: () => api.get<RunSummary[]>("/api/run/runs"),
    refetchInterval: 3_000,
  });
}

export function useRunRecipes(project: string) {
  return useQuery({
    queryKey: runKeys.recipes(project),
    queryFn: () =>
      api.get<{ recipes: string[] }>(
        `/api/run/recipes?project=${encodeURIComponent(project)}`,
      ),
    select: (data) => data.recipes,
  });
}

/**
 * Everything runnable in this project: the monorepo's workflows first, then the
 * project's own recipes.
 *
 * Workflows lead because in a monorepo they are what people actually run —
 * `build` across every package, rather than one config's publish recipe.
 */
export function useRunTargets(project: string) {
  const recipes = useRunRecipes(project);
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
  for (const name of recipes.data ?? []) {
    targets.push({ name, kind: "recipe", description: null, members: [] });
  }

  return {
    targets,
    // The workspace query fails on a project that isn't a monorepo, which is
    // an ordinary state rather than an error — recipes alone are a fine answer.
    isLoading: recipes.isLoading || workspace.isLoading,
  };
}

export interface StartRunBody {
  project: string;
  recipes: string[];
  dry_run: boolean;
  /** Values for variables the run needs but the daemon's environment lacks. */
  env?: Record<string, string>;
  /**
   * A monorepo workflow to run instead of recipes. The daemon compiles the
   * cross-workspace graph itself, so a run started here and one started by
   * `ciabatta build` can't disagree about what runs.
   */
  workflow?: string;
  /** Further workflows folded into the same graph, as `ciabatta run build test`. */
  workflows?: string[];
  /** With `workflow`: start only from these sub-workspaces. */
  only?: string[];
  /** With `workflow`: don't follow dependencies into other sub-workspaces. */
  isolated?: boolean;
  /** With `workflow`: run only the steps these terms select (CLI `--filter`). */
  filter?: string[];
}

/**
 * Everything this project can run, workflows and recipes together.
 *
 * The two are the same kind of thing — a named graph of steps — and differ only
 * in where they were written down, so the launcher offers them in one list
 * rather than making you know which kind you have before you can start it.
 */
export interface RunTarget {
  name: string;
  kind: "workflow" | "recipe";
  description: string | null;
  /** Which sub-workspaces define this workflow. Empty for a recipe. */
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
    mutationFn: (body: { recipe: string; step: string; option: number }) =>
      api.post(`/api/run/runs/${runId}/choose`, body),
  });
}
