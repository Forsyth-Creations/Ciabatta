/** Types and queries for runs. */

import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, api } from "./client";
import type { EnvReport } from "./types";
import { useWorkspace } from "./workspace";

/**
 * When something in a run started and finished, as RFC 3339 timestamps. Both
 * are null until it gets there — and stay null on a run recorded before the
 * daemon kept them.
 */
export interface Timed {
  started_at?: string | null;
  finished_at?: string | null;
}

export type StepStatus =
  | "pending"
  | "running"
  | "success"
  | "failed"
  | "skipped"
  /** Its outputs are coming down from the remote cache — see `restoring`. */
  | "restoring"
  /** Cut short: the run was stopped, or the daemon restarted under it. */
  | "stopped";

/** A step's outputs on their way down from the remote cache. */
export interface Restoring {
  /** `checking` while the server is asked; `downloading` once it said yes. */
  phase: "checking" | "downloading";
  server: string;
  files_done: number;
  files_total: number;
  bytes_done: number;
  bytes_total: number;
  /** The file on its way down. */
  current?: string;
}

export interface StepView extends Timed {
  name: string;
  status: StepStatus;
  /** Recovery nodes are the "fix-it" branches a failure can divert into. */
  recover: boolean;
  action: string | null;
  /** Where the step's action runs, relative to the run's root. Null means the
   *  root itself. */
  cwd: string | null;
  /** The exact command the engine hands to a shell — an inline `run` as
   *  written, a `script` as the `bash <path>` it becomes. */
  shell: string | null;
  needs: string[];
  on_error: string | null;
  logs: string[];
  /**
   * Lines the daemon dropped off the front of `logs` once its buffer filled.
   *
   * The buffer is capped because the stream sends the whole run state on every
   * change, so an uncapped log made each frame bigger than the last. The count
   * is sent so the viewer can say the log starts in the middle rather than
   * letting it be read as the beginning.
   */
  dropped_logs: number;

  // Set when the run is a monorepo workflow graph, whose nodes come from
  // several packages at once and have to say which.
  workspace: string | null;
  description: string | null;
  owner: string | null;
  kind: string | null;
  /** The special, identifiable publishing phase. */
  push: boolean;
  persistent: boolean;
  /** From the workflow's `background:` array: started first, gates nothing, stopped when the run ends. */
  background: boolean;
  timeout: string | null;
  requires: string[];

  /** Variables this step sets for itself, on top of the run's environment. */
  env: Record<string, string>;
  /** Variables this step's `when` / `skip_if` conditions test. Commands and
   *  scripts aren't scanned. */
  env_refs: string[];
  /**
   * The env files this step's workflow names (`env_file:`). The run searches
   * them ahead of every workspace's `.env`; empty for a workflow naming none.
   */
  env_files: string[];

  /** The five things this target is defined by. */
  deps: TargetDeps;

  /** What the cache decided about this step in this run, and why. Absent when
   *  the cache never looked at it — caching off, a dry run, `--authoritative`,
   *  or a condition that skipped the step. */
  cache?: CacheReport | null;

  /** Present while its outputs are being restored from the remote cache. */
  restoring?: Restoring | null;
}

/** "3 of 12 files · 4.1 MB of 20 MB", or what the lookup is doing. */
export function restoringLine(restoring: Restoring, humanize: (n: number) => string): string {
  if (restoring.phase === "checking") return "asking the remote cache…";
  return (
    `downloading ${restoring.files_done} of ${restoring.files_total} file(s) · ` +
    `${humanize(restoring.bytes_done)} of ${humanize(restoring.bytes_total)}`
  );
}

/** How far through a download is, 0–100, for a progress bar. */
export function restoringPercent(restoring: Restoring): number | null {
  if (restoring.phase !== "downloading" || restoring.bytes_total <= 0) return null;
  return Math.min(100, (restoring.bytes_done / restoring.bytes_total) * 100);
}

/** Whether the shared cache was in play for the run. */
export interface RemoteCacheStatus {
  url: string;
  connected: boolean;
  read_only: boolean;
  error: string | null;
}

/** A cache entry, as much of it as the run view needs. */
export interface CacheEntryInfo {
  key: string;
  created_at: string;
  last_used_at: string | null;
  size: number;
  outputs: number;
  duration_ms: number;
}

/** What the cache decided about one step during a run. */
export interface CacheReport {
  outcome: "fresh" | "hit" | "rebuild" | "uncached";
  source: "local" | "remote" | null;
  key: string | null;
  summary: string;
  /** Why it rebuilt, structured — the same shape the cache page's plan uses. */
  reason: { kind: string; [field: string]: unknown } | null;
  decided_at: string;
  /** The entry that was reused, on a hit. */
  entry: CacheEntryInfo | null;
  /** The last build of this target, on a rebuild — what it was compared to. */
  previous: CacheEntryInfo | null;
  diff: {
    files: { path: string; kind: "added" | "removed" | "modified"; additions: number; deletions: number }[];
    files_total: number;
    env: string[];
    upstream: string[];
    summary: string;
  } | null;
  input_files: number;
  input_bytes: number;
  env: string[];
  upstream: { step: string; fingerprint: string | null; unaccounted: boolean }[];
  /** Needed steps that forced this one to rebuild: they ran without declaring
   *  what they produce. */
  blocked_by: string[];
  saved_ms: number;
  remote: RemoteCacheStatus | null;
  stored: {
    at: string;
    size: number;
    outputs: number;
    uploaded: boolean | null;
    skipped: string | null;
  } | null;
  /** What to change so this step can be reused next time. */
  hints: string[];
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

  /**
   * What `cache.cargo` resolved to, one line per package in the closure. Its
   * paths are already in `inputs`/`outputs`/`exclude`; this says why.
   */
  cargo?: string[];
  /** Why `cache.cargo` couldn't be resolved, when it couldn't. */
  cargo_error?: string;
  /** Files rustc reports the last build reading that no input covers. */
  undeclared_reads?: string[];
  /** Files its inputs match that an output pattern also covers: treated as
   *  outputs only, never part of the key. */
  outputs_in_inputs?: string[];

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

export interface StageView extends Timed {
  name: string;
  status: string;
}

export interface PendingChoice {
  step: string;
  message: string;
  options: string[];
}

/**
 * A question a step asked with `ciabatta input`, waiting on an answer. It
 * lives in the run's state, so it survives a page reload until it is
 * answered, times out, or its step ends.
 */
export interface PendingInput {
  /** Unique within its run; the answer names it. */
  id: number;
  step: string;
  prompt: string;
  /** Longer text from `--details`. */
  details: string | null;
  /** What an empty answer — or a timeout — becomes. */
  default: string | null;
  /** Mask the answer (`--secret`). */
  secret: boolean;
  /** The variable that would have answered it without asking (`--env`). */
  env: string | null;
  /** Only a go-ahead is wanted (`--enter`): show Continue, not a field. */
  enter?: boolean;
  asked_at: string;
  /** When it stops waiting; null waits forever. */
  deadline: string | null;
}

/** A waiting question as the cross-run listing reports it. */
export interface WaitingInput extends PendingInput {
  run: number;
  project: string;
  workflow: string;
}

export interface WorkflowView extends Timed {
  name: string;
  status: string;
  error: string | null;
  stages: StageView[];
  steps: StepView[];
  edges: EdgeView[];
  logs: string[];
  /** As `StepView.dropped_logs`, for the workflow's combined output. */
  dropped_logs: number;
  pending: PendingChoice | null;
  /** `ciabatta input` questions waiting on an answer, oldest first. */
  inputs?: PendingInput[];
  /** What the run opened by warning about — a version mismatch with the
   *  remote cache. */
  warnings?: string[];
  /**
   * The environment this run started with, resolved once when it was created:
   * every variable its steps depend on, with the value they see.
   */
  env: EnvReport;
}

/** `started_at` is when the run's first workflow started, which can lag
 *  `created_at` (when it was asked for); `finished_at` stays null until every
 *  workflow has ended. */
export interface RunSummary extends Timed {
  id: number;
  project: string;
  workflows: string[];
  created_at: string;
  done: boolean;
  /** How it ended — or `running` while it hasn't. */
  status: StepStatus | "stopped";
  /** The directory its steps resolve their `cwd` against. */
  root: string;
  dry_run: boolean;
  /** Started with `--force`: the cache was ignored and every step ran. */
  force?: boolean;
  filter: string[];
  only: string[];
  isolated: boolean;
  /** Custom arguments, as typed after `...`. */
  args?: string[];
  /** The env profile it ran under. */
  env_profile?: string | null;
}

/** How long the daemon keeps a finished run and its logs. */
export interface RunSettings {
  /** Hours from the run's creation. Zero keeps them until deleted by hand. */
  ttl_hours: number;
  default_ttl_hours: number;
}

export interface RunState {
  workflows: WorkflowView[];
  done: boolean;
  dry_run: boolean;
  run: RunSummary;
  seq: number;
}

export const runKeys = {
  /** Every run list — invalidating this refreshes each project's. */
  runs: ["run", "runs"] as const,
  runsFor: (project: string) => ["run", "runs", project] as const,
  run: (id: number) => ["run", "run", id] as const,
  workflows: (project: string) => ["run", "workflows", project] as const,
  settings: ["run", "settings"] as const,
};

/**
 * The selected project's runs, newest first.
 *
 * Keyed by project, and filtered by the daemon: one daemon serves every
 * checkout on the machine, and a list shared between them showed another
 * repo's builds under whichever project was picked.
 */
export function useRuns(project: string) {
  return useQuery({
    queryKey: runKeys.runsFor(project),
    queryFn: () =>
      api.get<RunSummary[]>(`/api/run/runs?project=${encodeURIComponent(project)}`),
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
  /** Ignore the cache and run every step; results are still stored. */
  force?: boolean;
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
  /** Custom arguments, as after `...` on the CLI: each becomes CIABATTA_ARG_*. */
  args?: string[];
  /** Run under this env profile (`--env-profile`). */
  env_profile?: string;
}

/** An env profile: every `.env.<name>` in the workspace. */
export interface EnvProfile {
  name: string;
  files: string[];
  vars: number;
}

/** The env profiles a project has, for the launcher. */
export function useEnvProfiles(project: string) {
  return useQuery({
    queryKey: ["run", "env-profiles", project] as const,
    queryFn: () =>
      api.get<{ profiles: EnvProfile[] }>(
        `/api/run/env-profiles?project=${encodeURIComponent(project)}`,
      ),
    select: (data) => data.profiles,
  });
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
 * that would abort at its own `required_env` gate, so the launcher can ask for
 * the values and post again.
 */
export function missingEnvFrom(error: unknown): string[] | null {
  if (!(error instanceof ApiError) || error.status !== 422) return null;
  const missing = error.body?.missing_env;
  return Array.isArray(missing) && missing.length > 0 ? (missing as string[]) : null;
}

/**
 * Every question waiting on an answer, across every run.
 *
 * Polled from the app shell rather than read off one run's stream, so a step
 * asking a question reaches the operator whichever page they're on.
 */
export function useWaitingInputs() {
  return useQuery({
    queryKey: ["run", "inputs"] as const,
    queryFn: () => api.get<{ inputs: WaitingInput[] }>("/api/run/inputs"),
    refetchInterval: 1_500,
  });
}

/** Answer question `id` of run `run`. */
export function useAnswerInput() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { run: number; id: number; value: string }) =>
      api.post(`/api/run/runs/${body.run}/input`, { id: body.id, value: body.value }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ["run", "inputs"] }),
  });
}

export function useChoose(runId: number) {
  return useMutation({
    mutationFn: (body: { workflow: string; step: string; option: number }) =>
      api.post(`/api/run/runs/${runId}/choose`, body),
  });
}

/**
 * Ask a run to stop.
 *
 * The daemon *asks* rather than killing: the engine stops scheduling, cuts the
 * step in flight short, and still stops the background tasks it started on the
 * way past. Stopping a run that has already finished is a no-op rather than an
 * error, so a click landing as the last step lands is harmless.
 */
export function useStopRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (runId: number) => api.post(`/api/run/runs/${runId}/stop`, {}),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: runKeys.runs }),
  });
}

/**
 * Start a previous run again, exactly as it was started.
 *
 * The daemon re-runs from the request it stored rather than from anything the
 * page reconstructs, so the filters and the `--only` list come along. The one
 * thing it can't keep is the variables somebody typed into the missing-variable
 * prompt — those are never written to disk — so a re-run can come back with the
 * same 422 the first launch did, and is answered the same way.
 */
export function useRerunRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, env }: { id: number; env?: Record<string, string> }) =>
      api.post<RunSummary>(`/api/run/runs/${id}/rerun`, env ? { env } : {}),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: runKeys.runs }),
  });
}

/** Delete a finished run and its logs. */
export function useDeleteRun() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: number) => api.delete(`/api/run/runs/${id}`),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: runKeys.runs }),
  });
}

export function useRunSettings() {
  return useQuery({
    queryKey: runKeys.settings,
    queryFn: () => api.get<RunSettings>("/api/run/settings"),
  });
}

export function useSetRunSettings() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (ttl_hours: number) =>
      api.post<{ ttl_hours: number; pruned: number }>("/api/run/settings", { ttl_hours }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: runKeys.settings });
      // Shortening the TTL deletes records immediately, so the list is stale
      // the moment this returns.
      queryClient.invalidateQueries({ queryKey: runKeys.runs });
    },
  });
}

/**
 * How long something took, precise enough to compare two runs of it: tenths of
 * a second while that still matters, whole units once it doesn't. The same
 * shape the terminal prints, so the two read alike.
 */
export function formatElapsed(ms: number): string {
  if (ms < 60_000) return `${(Math.max(ms, 0) / 1000).toFixed(1)}s`;
  const seconds = Math.floor(ms / 1000);
  const minutes = Math.floor(seconds / 60);
  const pad = (n: number) => String(n).padStart(2, "0");
  if (minutes < 60) return `${minutes}m${pad(seconds % 60)}s`;
  return `${Math.floor(minutes / 60)}h${pad(minutes % 60)}m`;
}

/** A timestamp as a wall-clock time — `14:03:22` — for "when did it finish". */
export function clockTime(at: string): string {
  return new Date(at).toLocaleTimeString([], { hour12: false });
}

/**
 * How long a timed thing has taken: to its finish once it has one, to `now`
 * while it's still going. Null when it never started.
 */
export function elapsedOf(timed: Timed, now: number): string | null {
  if (!timed.started_at) return null;
  const began = Date.parse(timed.started_at);
  const ended = timed.finished_at ? Date.parse(timed.finished_at) : now;
  return formatElapsed(ended - began);
}

/**
 * The current time, re-read every `ms` while `live` — so a running phase's
 * clock ticks between updates from the daemon, which only arrive when
 * something changes. Stops ticking once nothing is running.
 */
export function useNow(live: boolean, ms = 1_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), ms);
    return () => window.clearInterval(timer);
  }, [live, ms]);
  return now;
}
