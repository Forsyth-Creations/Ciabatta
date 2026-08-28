/** Shapes returned by the daemon's JSON API. */

export interface Health {
  ok: boolean;
  version: string;
  pid: number;
  started_at: string;
}

export interface Project {
  id: string;
  path: string;
  name: string;
}

/** Where an environment variable's effective value comes from. */
export type EnvOrigin = "environment" | "env_file" | "config" | "unset";

/**
 * One environment variable a graph depends on.
 *
 * `steps` is what makes this a dependency rather than a footnote: it names the
 * steps that read or declare the variable, so a graph view can draw an edge
 * from the value to the work that needs it.
 */
export interface EnvVar {
  key: string;
  /** Masked when `secret`; null when nothing supplies it, or when it `varies`. */
  value: string | null;
  secret: boolean;
  /** Declared in REQUIRED_ENV — the run refuses to start without it. */
  required: boolean;
  origin: EnvOrigin;
  /** The `.env` file that supplied it, when `origin` is `env_file`. */
  file: string | null;
  steps: string[];
  /** Set by several steps to different values, so there's no single one. */
  varies: boolean;
}

export interface EnvReport {
  /** The `.env` files sourced, in application order. */
  files: string[];
  required: string[];
  /** Required variables still empty or unset. */
  missing: string[];
  vars: EnvVar[];
}

export type Priority = "low" | "medium" | "high";

export interface Todo {
  id: number;
  text: string;
  done: boolean;
  priority: Priority;
  created_at: string;
  /** The project this task belongs to; null for tasks from before scoping. */
  project: string | null;
}

// ─── Caching ─────────────────────────────────────────────────────────────────

/** One file, hashed. */
export interface FileHash {
  path: string;
  sha256: string;
  size: number;
}

/** What ciabatta would do with one stage. */
export type Decision =
  | { outcome: "fresh"; key: string; outputs: number }
  | { outcome: "hit"; key: string; source: "local" | "remote"; outputs: number }
  | { outcome: "rebuild"; key: string; reason: RebuildReason }
  | { outcome: "uncached"; reason: string };

/** Why a stage has to run. Every variant names something you can go and look at. */
export type RebuildReason =
  | { kind: "never_built" }
  | { kind: "inputs_changed"; changed: string[]; total: number }
  | { kind: "outputs_missing"; missing: string[] }
  | { kind: "outputs_modified"; modified: string[] }
  | { kind: "no_outputs" }
  | { kind: "upstream_reran"; steps: string[] };

export type ChangeKind = "added" | "removed" | "modified";

/** One line of a diff. */
export type DiffLine =
  | { op: "context"; old: number; new: number; text: string }
  | { op: "added"; new: number; text: string }
  | { op: "removed"; old: number; text: string };

export interface Hunk {
  old_start: number;
  old_lines: number;
  new_start: number;
  new_lines: number;
  lines: DiffLine[];
}

export interface FileDiff {
  path: string;
  kind: ChangeKind;
  additions: number;
  deletions: number;
  hunks: Hunk[];
  /** Why there are no hunks: binary, too large, or not snapshotted. */
  note: string | null;
}

export interface EnvDiff {
  name: string;
  kind: ChangeKind;
  before: string | null;
  after: string | null;
}

export interface UpstreamDiff {
  step: string;
  kind: ChangeKind;
  before: string | null;
  after: string | null;
}

/**
 * Everything that changed since a stage last ran.
 *
 * The three arrays mirror a stage's three dependencies — its input files, the
 * environment variables it declared, and the outputs of the stages it needs.
 */
export interface CacheDiff {
  previous_key: string | null;
  previous_at: string | null;
  files: FileDiff[];
  env: EnvDiff[];
  upstream: UpstreamDiff[];
}

/** One stage in a cache plan. */
export interface PlannedStep {
  name: string;
  needs: string[];
  workspace: string;
  decision: Decision;
  key: string | null;
  inputs: FileHash[];
  outputs: FileHash[];
  diff: CacheDiff | null;
}

export interface CachePlan {
  reused: number;
  rebuilt: number;
  saved_ms: number;
  caching: boolean;
  steps: PlannedStep[];
}

export interface RemoteRef {
  url: string;
  name: string | null;
  project: string | null;
  read_only: boolean;
  enabled: boolean;
}

export interface CacheStatus {
  enabled: boolean;
  why_disabled: string | null;
  inputs: string[];
  outputs: string[];
  exclude: string[];
  env: string[];
  remote: RemoteRef | null;
  path: string;
  entries: number;
  bytes: number;
  human: string;
  build_time_ms: number;
  by_workspace: Record<string, number>;
  oldest: string | null;
  newest: string | null;
}

/** Counters a remote cache keeps since it started. */
export interface RemoteCounters {
  hits: number;
  misses: number;
  uploads: number;
  bytes_served: number;
  bytes_stored: number;
}

export interface RemoteProject {
  project: { id: string; name: string; created_at: string; created_by: string | null };
  counters: RemoteCounters;
  hit_rate: number | null;
  entries: number;
}

/** The ciabatta build a remote cache hands out for one platform. */
export interface ReleaseBuild {
  sha256: string;
  size: number;
  modified_at: string | null;
}

export interface Release {
  version: string;
  notes: string | null;
  builds: Record<string, ReleaseBuild>;
}

export interface RemoteStats {
  storage: {
    entries: number;
    bytes: number;
    human: string;
    oldest: string | null;
    newest: string | null;
    path: string;
  };
  counters: RemoteCounters;
  hit_rate: number | null;
  retention: { description: string };
  sessions: number;
  release: Release;
  started_at: string;
  projects: RemoteProject[];
}

/** What the daemon knows about this project's remote cache. */
export type RemoteStatus =
  | { configured: false }
  | {
      configured: true;
      url: string;
      project: string | null;
      read_only: boolean;
      reachable: true;
      stats: RemoteStats;
    }
  | {
      configured: true;
      url: string;
      project: string | null;
      read_only: boolean;
      reachable: false;
      error: string;
    };
