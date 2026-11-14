// Generated from proto/oven.proto by build.rs. Don't edit it: change the .proto, then
// run `CIABATTA_REGENERATE=1 cargo test generated_typescript` to write it here.

/**
 * Where a job has got to.
 */
export type JobState =
  | "JOB_STATE_UNSPECIFIED"
  | "JOB_STATE_QUEUED"
  | "JOB_STATE_RUNNING"
  | "JOB_STATE_SUCCEEDED"
  | "JOB_STATE_FAILED"
  | "JOB_STATE_CANCELLED";

/**
 * What an executor is doing.
 */
export type ExecutorState =
  | "EXECUTOR_STATE_UNSPECIFIED"
  | "EXECUTOR_STATE_IDLE"
  | "EXECUTOR_STATE_BUSY"
  | "EXECUTOR_STATE_OFFLINE";

/**
 * Whether an executor found on the network can be added.
 */
export type Availability =
  | "AVAILABILITY_UNSPECIFIED"
  | "AVAILABILITY_AVAILABLE"
  | "AVAILABILITY_PAIRED_HERE"
  | "AVAILABILITY_PAIRED_ELSEWHERE";

/**
 * What a client asks for: one workflow run, at one commit.
 */
export interface JobSpec {
  /**
   * The cache project it belongs to.
   */
  project: string;
  /**
   * The URL the executor clones.
   */
  repo: string;
  /**
   * The commit it checks out — always a full hash.
   */
  commit: string;
  /**
   * The branch the commit is on, so the run sees the same CIABATTA_BRANCH
   * (and CIABATTA_PATH) it would have locally.
   */
  branch?: string | null;
  /**
   * The workflows to run, first named first.
   */
  workflows: string[];
  /**
   * The workspace root, relative to the top of the repository ("" when they
   * are the same directory).
   */
  root: string;
  /**
   * Where the command was typed, relative to the workspace root.
   */
  cwd: string;
  only: string[];
  solo: boolean;
  all: boolean;
  filter: string[];
  /**
   * `-e KEY=VALUE` flags, as typed.
   */
  env: string[];
  /**
   * Custom arguments, as typed after `...`.
   */
  args: string[];
  force: boolean;
  /**
   * Run only on the executor of this name; unset takes the first free one.
   */
  executor?: string | null;
}

/**
 * One file a job sent back, relative to the workspace root.
 */
export interface OutputFile {
  path: string;
  size: string;
}

/**
 * A job, as the cache keeps it.
 */
export interface Job {
  /**
   * Its execution number: what its outputs are filed under, and what the run
   * sees as CIABATTA_EXECUTION_NUMBER.
   */
  id: number;
  spec?: JobSpec | null;
  state: JobState;
  /**
   * Who asked for it.
   */
  requested_by: string;
  created_at: string;
  /**
   * The executor running it, once one has it.
   */
  executor?: string | null;
  started_at?: string | null;
  finished_at?: string | null;
  /**
   * Somebody asked for it to stop; the executor stops it at its next
   * heartbeat.
   */
  cancel_requested: boolean;
  /**
   * Why it ended the way it did, in a sentence.
   */
  message?: string | null;
  /**
   * The CIABATTA_PATH the run had, for the record.
   */
  ciabatta_path?: string | null;
  /**
   * The files it sent back.
   */
  outputs: OutputFile[];
  /**
   * How many log lines it has.
   */
  log_lines: number;
}

/**
 * A paired executor, as the cache shows it.
 */
export interface ExecutorView {
  id: string;
  name: string;
  state: ExecutorState;
  /**
   * The job it is running.
   */
  job?: number | null;
  /**
   * Where it was paired from.
   */
  address: string;
  version: string;
  /**
   * The OS and architecture it runs on ("linux-x86_64").
   */
  platform: string;
  added_at: string;
  added_by: string;
  last_seen?: string | null;
}

/**
 * A change in whether an executor could be reached: online when it checks in,
 * offline once it has been silent for OFFLINE_AFTER_SECONDS.
 */
export interface StatusChange {
  /**
   * When it changed (RFC 3339).
   */
  at: string;
  online: boolean;
}

/**
 * GET /api/oven/executors/{id}/history: the cache's record of an executor
 * coming and going, oldest first. Only the most recent changes are kept.
 */
export interface ExecutorHistory {
  changes: StatusChange[];
}

/**
 * An executor the cache can see on the network, paired or not.
 */
export interface Discovered {
  id: string;
  name: string;
  /**
   * "http://host:port", ready to pair with.
   */
  address: string;
  version: string;
  availability: Availability;
  /**
   * The OS and architecture it runs on ("linux-x86_64").
   */
  platform: string;
}

/**
 * GET /api/oven/executors
 */
export interface ExecutorList {
  executors: ExecutorView[];
  discovered: Discovered[];
}

/**
 * GET /api/oven/jobs
 */
export interface JobList {
  jobs: Job[];
}

/**
 * What an executor says about itself at GET /oven/info.
 */
export interface ExecutorInfo {
  id: string;
  name: string;
  version: string;
  platform: string;
  /**
   * The cache it is paired with, if any.
   */
  paired_with?: string | null;
}

/**
 * What the cache sends an executor to pair with it: POST /oven/pair.
 */
export interface PairRequest {
  /**
   * The code the executor printed.
   */
  code: string;
  /**
   * Where the executor reaches the cache from now on.
   */
  server: string;
  /**
   * The executor's credential for that cache. The cache keeps only its hash.
   */
  token: string;
  /**
   * What the cache calls it, when that's not the name it gave itself.
   */
  name?: string | null;
}

/**
 * What a person asks the cache to pair with: POST /api/oven/executors.
 */
export interface PairWith {
  /**
   * "host:port" or a URL, discovered or typed.
   */
  address: string;
  code: string;
  /**
   * The URL the executor should use for the cache: where the page was loaded
   * from, unless set.
   */
  server?: string | null;
  /**
   * What to call it on this cache (and what --offload-to names). Unset keeps
   * the name it already has here, or else the one it gave itself.
   */
  name?: string | null;
}

/**
 * The answer to a pairing.
 */
export interface Paired {
  executor?: ExecutorInfo | null;
}

/**
 * An executor waiting for work: POST /api/oven/executors/{id}/poll.
 */
export interface Poll {
  /**
   * The job it was given, or unset when nothing came up in time.
   */
  job?: Job | null;
}

/**
 * An executor saying what it is doing: POST /api/oven/executors/{id}/heartbeat.
 */
export interface Heartbeat {
  job?: number | null;
}

/**
 * The cache's answer to a heartbeat.
 */
export interface HeartbeatReply {
  /**
   * Stop the job it named.
   */
  cancel: boolean;
}

/**
 * A batch of log lines from an executor.
 */
export interface LogBatch {
  lines: string[];
}

/**
 * What a reader of a job's log gets: lines from `from` on.
 */
export interface LogPage {
  lines: string[];
  /**
   * Where to read from next time.
   */
  next: number;
}

/**
 * How an executor reports that a job is over.
 */
export interface Finish {
  state: JobState;
  message?: string | null;
  ciabatta_path?: string | null;
  outputs: OutputFile[];
}
