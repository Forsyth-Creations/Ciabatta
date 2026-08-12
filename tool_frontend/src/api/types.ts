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
}
