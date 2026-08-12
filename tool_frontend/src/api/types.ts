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

export type Priority = "low" | "medium" | "high";

export interface Todo {
  id: number;
  text: string;
  done: boolean;
  priority: Priority;
  created_at: string;
}
