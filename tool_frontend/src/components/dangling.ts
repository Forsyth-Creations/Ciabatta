/**
 * What dangles in a workspace, worked out live from the graph editor's drafts
 * so the answer changes as the user connects things.
 *
 * The same rules as `src/workspace/dangling.rs`, which `ciabatta self graph`
 * prints — keep the two in step.
 *
 * A workflow **dangles** when it names no other workflow (`needs`,
 * `background`, `after`, or its sub-workspace's `depends_on`), no other
 * workflow names it, and no other sub-workspace has a workflow of the same
 * name to run alongside under `ciabatta <name>`.
 *
 * A step is **loose** when its workflow orders some steps but not that one:
 * it needs nothing, nothing needs it, and it isn't a recovery or background
 * step, nor the source or target of an `on_error` / `retry` route.
 */

import type { DeclaredUnit } from "../api/workspace";

/** The editable part of a workflow: what the editor holds a draft of. */
export interface GraphDraft {
  needs: string[];
  steps: { name: string; needs: string[] }[];
}

export interface DanglingReport {
  /** `<member>:<workflow>`s nothing connects to. */
  dangling: Set<string>;
  /** Loose steps by workflow. */
  loose: Map<string, string[]>;
}

/** A need, as the `<member>:<workflow>` it means from `member`'s `workflow`:
 *  a bare member names its workflow of the same name, `self` names `member`. */
export function resolveNeed(need: string, member: string, workflow: string): string {
  const [who, what] = need.includes(":")
    ? [need.slice(0, need.indexOf(":")).trim(), need.slice(need.indexOf(":") + 1).trim()]
    : [need.trim(), workflow];
  return `${who === "self" ? member : who}:${what}`;
}

const keyOf = (u: { member: string; workflow: string }) => `${u.member}:${u.workflow}`;

/** Every existing workflow `unit` names, with its draft's `needs`. */
export function linksOut(unit: DeclaredUnit, draft: GraphDraft, known: Set<string>): string[] {
  const own = keyOf(unit);
  return [
    ...new Set(
      [...draft.needs, ...unit.depends_on, ...unit.background, ...unit.after]
        .map((n) => resolveNeed(n, unit.member, unit.workflow))
        .filter((t) => known.has(t) && t !== own),
    ),
  ];
}

export function findDangling(
  units: DeclaredUnit[],
  drafts: Record<string, GraphDraft>,
): DanglingReport {
  const known = new Set(units.map(keyOf));
  const outs = new Map(
    units.filter((u) => drafts[keyOf(u)]).map((u) => [keyOf(u), linksOut(u, drafts[keyOf(u)], known)]),
  );
  const reached = new Set([...outs.values()].flat());

  const dangling = new Set<string>();
  const loose = new Map<string, string[]>();
  for (const unit of units) {
    const key = keyOf(unit);
    const draft = drafts[key];
    if (!draft) continue;
    const sharesName = units.some((o) => o.workflow === unit.workflow && o.member !== unit.member);
    if ((outs.get(key) ?? []).length === 0 && !reached.has(key) && !sharesName) dangling.add(key);

    const stray = looseSteps(unit, draft);
    if (stray.length) loose.set(key, stray);
  }
  return { dangling, loose };
}

/** The steps of a workflow nothing in it orders, when it orders any. */
export function looseSteps(unit: DeclaredUnit, draft: GraphDraft): string[] {
  if (!draft.steps.some((s) => s.needs.length > 0)) return [];
  const needed = new Set(draft.steps.flatMap((s) => s.needs));
  const routed = new Set(unit.steps.flatMap((s) => s.routes));
  return draft.steps
    .filter((s) => {
      const info = unit.steps.find((d) => d.name === s.name);
      return (
        s.needs.length === 0 &&
        !info?.routes.length &&
        !info?.recover &&
        !info?.background &&
        !needed.has(s.name) &&
        !routed.has(s.name)
      );
    })
    .map((s) => s.name);
}
