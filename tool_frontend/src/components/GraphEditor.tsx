/**
 * The run page's graph in edit mode: prune steps, and add or remove what each
 * step — and each workflow — needs, then save it back to the workflow files.
 *
 * It edits the graph the *files* declare, not the run's: a run may be
 * filtered, and what gets saved is the workflow, not this run of it. Every
 * change is held in a draft until Save, which sends each workflow whole — a
 * set, not a patch — so what is on screen is exactly what lands on disk.
 *
 * The whole-workspace graph (`ciabatta self graph`, `/graph`) is this editor
 * over every workflow at once, with `showDangling` marking what nothing
 * connects to — worked out again on every edit, so connecting a node clears
 * its mark before anything is saved.
 */

import { useEffect, useMemo, useState } from "react";
import {
  Alert,
  Autocomplete,
  Box,
  Button,
  Chip,
  Divider,
  Paper,
  Stack,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import DeleteIcon from "@mui/icons-material/DeleteOutline";
import SaveIcon from "@mui/icons-material/Save";
import UndoIcon from "@mui/icons-material/Undo";
import { StepGraph, type GraphStep } from "@ciabatta/graphs";

import {
  type DeclaredUnit,
  type UnitEdit,
  useDeclared,
  useSaveDeclared,
} from "../api/workspace";
import { ErrorNote, Loading } from "./Page";
import { findDangling, resolveNeed } from "./dangling";
import { monoFontStack } from "../theme";

/** A workflow being edited: what each kept step needs, and the workflow's own. */
interface Draft {
  needs: string[];
  steps: { name: string; needs: string[] }[];
}

const unitKey = (u: { member: string; workflow: string }) => `${u.member}:${u.workflow}`;
const nodeId = (unit: string, step: string) => `${unit}:${step}`;

function draftOf(unit: DeclaredUnit): Draft {
  return {
    needs: [...unit.needs],
    steps: unit.steps.map((s) => ({ name: s.name, needs: [...s.needs] })),
  };
}

/** Whether `from` already waits — directly or not — on `on`. */
function waitsOn(draft: Draft, from: string, on: string): boolean {
  const seen = new Set<string>();
  const stack = [from];
  while (stack.length) {
    const at = stack.pop()!;
    if (at === on) return true;
    if (seen.has(at)) continue;
    seen.add(at);
    stack.push(...(draft.steps.find((s) => s.name === at)?.needs ?? []));
  }
  return false;
}

/** Each change a draft makes to its file, in words. */
function describeChanges(unit: DeclaredUnit, draft: Draft): string[] {
  const changes: string[] = [];
  const kept = new Set(draft.steps.map((s) => s.name));
  for (const step of unit.steps) {
    if (!kept.has(step.name)) changes.push(`remove step ${step.name}`);
  }
  for (const step of draft.steps) {
    const was = unit.steps.find((s) => s.name === step.name)!;
    for (const n of step.needs)
      if (!was.needs.includes(n)) changes.push(`${step.name} needs ${n}`);
    for (const n of was.needs)
      if (kept.has(n) && !step.needs.includes(n))
        changes.push(`${step.name} no longer needs ${n}`);
  }
  for (const n of draft.needs)
    if (!unit.needs.includes(n)) changes.push(`workflow needs ${n}`);
  for (const n of unit.needs)
    if (!draft.needs.includes(n)) changes.push(`workflow no longer needs ${n}`);
  return changes;
}

/** A target for "add a dependency": a step of the same workflow, or another
 *  workflow (which the whole workflow then needs). */
interface Option {
  kind: "step" | "workflow";
  value: string;
}

export function GraphEditor({
  project,
  units,
  status,
  onClose,
  title = "Editing the graph",
  height = 520,
  showDangling = false,
}: {
  project: string;
  /** The workflows to edit, `<member>:<workflow>` each, or `["*"]` for all. */
  units: string[];
  /** The run's status for each step, by cache id, to colour the nodes. */
  status: Record<string, string>;
  /** Shows a Done button that calls this. */
  onClose?: () => void;
  title?: string;
  height?: number | string;
  /** Mark dangling workflows and loose steps, and list them beside the graph. */
  showDangling?: boolean;
}) {
  const declared = useDeclared(project, units);
  const save = useSaveDeclared();
  const [drafts, setDrafts] = useState<Record<string, Draft>>({});
  const [selected, setSelected] = useState<string | null>(null);
  const [saved, setSaved] = useState<string[] | null>(null);

  // A fresh read of the files (on open, and after a save) is the new baseline.
  useEffect(() => {
    if (!declared.data) return;
    setDrafts(
      Object.fromEntries(declared.data.units.map((u) => [unitKey(u), draftOf(u)])),
    );
  }, [declared.data]);

  const byKey = useMemo(
    () => new Map((declared.data?.units ?? []).map((u) => [unitKey(u), u])),
    [declared.data],
  );

  const report = useMemo(
    () => (showDangling && declared.data ? findDangling(declared.data.units, drafts) : null),
    [showDangling, declared.data, drafts],
  );

  const steps: GraphStep[] = useMemo(() => {
    // A loaded workflow's exits: the steps nothing else in it waits on.
    const exitsOf = (target: string) => {
      const other = drafts[target];
      const otherUnit = byKey.get(target);
      if (!other || !otherUnit) return [];
      const needed = new Set(other.steps.flatMap((s) => s.needs));
      return other.steps
        .filter((s) => !needed.has(s.name))
        .filter((s) => !otherUnit.steps.find((d) => d.name === s.name)?.recover)
        .map((s) => nodeId(target, s.name));
    };
    const out: GraphStep[] = [];
    for (const [key, draft] of Object.entries(drafts)) {
      const unit = byKey.get(key);
      if (!unit) continue;
      // Another loaded workflow this one waits for hands it its exits — and
      // so does one that runs this one as an after action.
      const runsAfter = [...byKey.entries()]
        .filter(([, u]) => u.after.some((a) => resolveNeed(a, u.member, u.workflow) === key))
        .map(([k]) => k);
      const upstream = [
        ...[...draft.needs, ...unit.depends_on].map((need) => resolveNeed(need, unit.member, unit.workflow)),
        ...runsAfter,
      ].flatMap(exitsOf);
      const dangles = report?.dangling.has(key) ?? false;
      const loose = report?.loose.get(key) ?? [];
      for (const step of draft.steps) {
        const id = nodeId(key, step.name);
        const info = unit.steps.find((s) => s.name === step.name)!;
        out.push({
          name: id,
          label: step.name,
          workspace: key,
          status: dangles || loose.includes(step.name) ? "warning" : (status[id] ?? "pending"),
          needs: [
            ...step.needs.map((n) => nodeId(key, n)),
            ...(step.needs.length === 0 && !info.recover ? upstream : []),
          ],
          recover: info.recover,
          background: info.background,
          highlight: id === selected,
          detail: info.recover ? "recovery" : step.needs.length ? `needs ${step.needs.join(", ")}` : "",
          note: dangles
            ? "dangling: nothing connects to this workflow"
            : loose.includes(step.name)
              ? "loose: nothing orders this step"
              : null,
        });
      }
    }
    return out;
  }, [drafts, byKey, selected, status, report]);

  if (declared.error) return <ErrorNote error={declared.error} />;
  if (!declared.data) return <Loading label="Reading the workflow files…" />;

  const changes = [...byKey.entries()].flatMap(([key, unit]) =>
    drafts[key] ? describeChanges(unit, drafts[key]).map((c) => `${key}: ${c}`) : [],
  );

  const update = (key: string, change: (draft: Draft) => Draft) => {
    setSaved(null);
    setDrafts((all) => ({ ...all, [key]: change(all[key]) }));
  };

  const pick = selected
    ? Object.keys(drafts).find((key) => selected.startsWith(`${key}:`))
    : undefined;
  const unit = pick ? byKey.get(pick) : undefined;
  const draft = pick ? drafts[pick] : undefined;
  const stepName = pick && selected ? selected.slice(pick.length + 1) : undefined;
  const step = draft?.steps.find((s) => s.name === stepName);
  const info = unit?.steps.find((s) => s.name === stepName);

  const onSave = () => {
    const edits: UnitEdit[] = [...byKey.entries()]
      .filter(([key, u]) => drafts[key] && describeChanges(u, drafts[key]).length > 0)
      .map(([key, u]) => ({ member: u.member, workflow: u.workflow, ...drafts[key] }));
    save.mutate(
      { project, units: edits },
      {
        onSuccess: (result) => {
          setSaved(result.written);
          setSelected(null);
        },
      },
    );
  };

  return (
    <Stack spacing={1.5}>
      <Stack direction="row" alignItems="center" spacing={1} flexWrap="wrap" useFlexGap>
        <Typography variant="subtitle2">{title}</Typography>
        <Typography variant="caption" color="text.secondary">
          Click a step to prune it or change what it needs. Nothing is written until you save.
        </Typography>
        <Box sx={{ flexGrow: 1 }} />
        <Button
          size="small"
          startIcon={<UndoIcon />}
          disabled={changes.length === 0}
          onClick={() => {
            setDrafts(
              Object.fromEntries(declared.data.units.map((u) => [unitKey(u), draftOf(u)])),
            );
            setSelected(null);
          }}
        >
          Discard changes
        </Button>
        <Button
          size="small"
          variant="contained"
          startIcon={<SaveIcon />}
          disabled={changes.length === 0 || save.isPending}
          onClick={onSave}
        >
          Save {changes.length > 0 && `(${changes.length})`}
        </Button>
        {onClose && (
          <Button size="small" onClick={onClose}>
            Done
          </Button>
        )}
      </Stack>

      {save.error && <ErrorNote error={save.error} />}
      {saved && (
        <Alert severity="success">
          Saved {saved.join(", ")}. Run again to use the new graph.
        </Alert>
      )}
      {declared.data.units
        .filter((u) => u.locked)
        .map((u) => (
          <Alert key={unitKey(u)} severity="info">
            {unitKey(u)} can&apos;t be edited here: {u.locked}
          </Alert>
        ))}

      <Stack direction={{ xs: "column", md: "row" }} spacing={1.5} alignItems="stretch">
        <Box sx={{ flex: 1, minWidth: 0 }}>
          <StepGraph
            steps={steps}
            height={height}
            holdLayout
            onStepClick={(name) => setSelected(name === selected ? null : name)}
          />
        </Box>
        <Paper variant="outlined" sx={{ width: { md: 360 }, p: 1.5, flexShrink: 0 }}>
          {step && unit && draft && info && pick ? (
            <StepPanel
              unitKey={pick}
              unit={unit}
              draft={draft}
              step={step}
              targets={declared.data.targets}
              drafts={drafts}
              byKey={byKey}
              onChange={(change) => update(pick, change)}
              onChangeOther={update}
              onRemoved={() => setSelected(null)}
            />
          ) : (
            <Typography variant="body2" color="text.secondary">
              Pick a step in the graph.
            </Typography>
          )}
          {report && (
            <>
              <Divider sx={{ my: 1.5 }} />
              <DanglingList
                dangling={[...report.dangling]}
                loose={[...report.loose.entries()]}
                firstStep={(key) => drafts[key]?.steps[0]?.name}
                onPick={setSelected}
              />
            </>
          )}
          <Divider sx={{ my: 1.5 }} />
          <Typography variant="overline" color="text.secondary">
            Changes to save
          </Typography>
          {changes.length === 0 ? (
            <Typography variant="body2" color="text.secondary">
              None yet.
            </Typography>
          ) : (
            <Box component="ul" sx={{ m: 0, pl: 2.5 }}>
              {changes.map((c) => (
                <Typography component="li" variant="body2" key={c} sx={{ fontFamily: monoFontStack, fontSize: 12 }}>
                  {c}
                </Typography>
              ))}
            </Box>
          )}
        </Paper>
      </Stack>
    </Stack>
  );
}

/** What the whole-workspace graph found dangling, each a link to its node. */
function DanglingList({
  dangling,
  loose,
  firstStep,
  onPick,
}: {
  dangling: string[];
  loose: [string, string[]][];
  firstStep: (key: string) => string | undefined;
  onPick: (node: string) => void;
}) {
  if (dangling.length === 0 && loose.length === 0) {
    return (
      <Alert severity="success" variant="outlined">
        Nothing dangles: every workflow is connected.
      </Alert>
    );
  }
  return (
    <Stack spacing={1}>
      {dangling.length > 0 && (
        <Box>
          <Typography variant="overline" color="text.secondary">
            Dangling workflows ({dangling.length})
          </Typography>
          <Typography variant="caption" color="text.secondary" component="p" sx={{ mb: 0.5 }}>
            Nothing needs them, they need nothing, and no other package shares their name. Connect
            them, or remove them if they&apos;re no longer used.
          </Typography>
          <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
            {dangling.map((key) => (
              <Chip
                key={key}
                size="small"
                color="warning"
                variant="outlined"
                label={key}
                sx={{ fontFamily: monoFontStack }}
                onClick={() => {
                  const first = firstStep(key);
                  if (first) onPick(nodeId(key, first));
                }}
              />
            ))}
          </Stack>
        </Box>
      )}
      {loose.length > 0 && (
        <Box>
          <Typography variant="overline" color="text.secondary">
            Loose steps
          </Typography>
          <Typography variant="caption" color="text.secondary" component="p" sx={{ mb: 0.5 }}>
            Their workflow orders its other steps, but nothing orders these.
          </Typography>
          <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
            {loose.flatMap(([key, names]) =>
              names.map((name) => (
                <Chip
                  key={nodeId(key, name)}
                  size="small"
                  color="warning"
                  variant="outlined"
                  label={`${key} › ${name}`}
                  sx={{ fontFamily: monoFontStack }}
                  onClick={() => onPick(nodeId(key, name))}
                />
              )),
            )}
          </Stack>
        </Box>
      )}
    </Stack>
  );
}

/** Whether workflow `from` already waits — directly or not — on `on`. */
function workflowWaitsOn(
  drafts: Record<string, Draft>,
  byKey: Map<string, DeclaredUnit>,
  from: string,
  on: string,
): boolean {
  const seen = new Set<string>();
  const stack = [from];
  while (stack.length) {
    const at = stack.pop()!;
    if (at === on) return true;
    if (seen.has(at)) continue;
    seen.add(at);
    const unit = byKey.get(at);
    if (!unit) continue;
    const needs = [...(drafts[at]?.needs ?? unit.needs), ...unit.depends_on];
    stack.push(...needs.map((n) => resolveNeed(n, unit.member, unit.workflow)));
  }
  return false;
}

function StepPanel({
  unitKey: key,
  unit,
  draft,
  step,
  targets,
  drafts,
  byKey,
  onChange,
  onChangeOther,
  onRemoved,
}: {
  unitKey: string;
  unit: DeclaredUnit;
  draft: Draft;
  step: { name: string; needs: string[] };
  targets: string[];
  drafts: Record<string, Draft>;
  byKey: Map<string, DeclaredUnit>;
  onChange: (change: (draft: Draft) => Draft) => void;
  /** Change another loaded workflow's draft: making it wait for this one. */
  onChangeOther: (key: string, change: (draft: Draft) => Draft) => void;
  onRemoved: () => void;
}) {
  const [query, setQuery] = useState("");
  const [dependentQuery, setDependentQuery] = useState("");
  // Loaded workflows whose own `needs` name this one: those can be undone here.
  const neededBy = Object.entries(drafts)
    .filter(([k]) => k !== key && byKey.has(k))
    .flatMap(([k, d]) =>
      d.needs
        .filter((n) => resolveNeed(n, k.split(":")[0], byKey.get(k)!.workflow) === key)
        .map((n) => ({ key: k, need: n })),
    );
  const runsAfterOrBeside = [...byKey.entries()]
    .filter(([k, u]) => k !== key && [...u.after, ...u.background].some((a) => resolveNeed(a, u.member, u.workflow) === key))
    .map(([k]) => k);
  // Loaded, editable workflows that could be made to wait for this one
  // without a cycle.
  const dependentOptions = Object.keys(drafts)
    .filter((k) => k !== key && byKey.has(k) && !byKey.get(k)!.locked)
    .filter((k) => !neededBy.some((n) => n.key === k))
    .filter((k) => !workflowWaitsOn(drafts, byKey, key, k));
  const locked = Boolean(unit.locked);
  // A step another step routes to (on_error / retry) can't go on its own.
  const routedFrom = draft.steps
    .filter((s) => unit.steps.find((d) => d.name === s.name)?.routes.includes(step.name))
    .map((s) => s.name);

  const options: Option[] = [
    ...draft.steps
      .filter((s) => s.name !== step.name && !step.needs.includes(s.name))
      // Needing a step that already waits on this one would be a cycle.
      .filter((s) => !waitsOn(draft, s.name, step.name))
      .map((s) => ({ kind: "step" as const, value: s.name })),
    ...targets
      .filter((t) => t !== key && !draft.needs.includes(t))
      .map((t) => ({ kind: "workflow" as const, value: t })),
  ];

  const remove = () => {
    onChange((d) => ({
      ...d,
      steps: d.steps
        .filter((s) => s.name !== step.name)
        // Whatever waited on the pruned step now waits on what it waited on,
        // so the order around the gap holds.
        .map((s) =>
          s.needs.includes(step.name)
            ? {
                ...s,
                needs: [
                  ...new Set(s.needs.flatMap((n) => (n === step.name ? step.needs : [n]))),
                ],
              }
            : s,
        ),
    }));
    onRemoved();
  };

  return (
    <Stack spacing={1.25}>
      <Box>
        <Typography variant="subtitle2" sx={{ fontFamily: monoFontStack }}>
          {step.name}
        </Typography>
        <Typography variant="caption" color="text.secondary">
          {key}
          {unit.file && ` · ${unit.file}`}
        </Typography>
      </Box>

      <Box>
        <Typography variant="overline" color="text.secondary">
          Needs
        </Typography>
        <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
          {step.needs.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              Nothing in this workflow.
            </Typography>
          )}
          {step.needs.map((n) => (
            <Chip
              key={n}
              size="small"
              label={n}
              disabled={locked}
              onDelete={() =>
                onChange((d) => ({
                  ...d,
                  steps: d.steps.map((s) =>
                    s.name === step.name ? { ...s, needs: s.needs.filter((x) => x !== n) } : s,
                  ),
                }))
              }
            />
          ))}
        </Stack>
      </Box>

      <Box>
        <Typography variant="overline" color="text.secondary">
          The whole workflow needs
        </Typography>
        <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
          {draft.needs.length === 0 && unit.depends_on.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              No other workflows.
            </Typography>
          )}
          {draft.needs.map((n) => (
            <Chip
              key={n}
              size="small"
              variant="outlined"
              label={n}
              disabled={locked}
              onDelete={() => onChange((d) => ({ ...d, needs: d.needs.filter((x) => x !== n) }))}
            />
          ))}
          {unit.depends_on.map((n) => (
            <Tooltip key={n} title="From [workspace] depends_on in the sub-workspace's config — edit it there.">
              <Chip size="small" variant="outlined" label={`${n} (depends_on)`} />
            </Tooltip>
          ))}
        </Stack>
      </Box>

      <Box>
        <Typography variant="overline" color="text.secondary">
          Waits for this workflow
        </Typography>
        <Stack direction="row" spacing={0.5} flexWrap="wrap" useFlexGap>
          {neededBy.length === 0 && runsAfterOrBeside.length === 0 && (
            <Typography variant="body2" color="text.secondary">
              Nothing loaded here.
            </Typography>
          )}
          {neededBy.map(({ key: other, need }) => (
            <Chip
              key={`${other}<${need}`}
              size="small"
              variant="outlined"
              label={other}
              disabled={Boolean(byKey.get(other)?.locked)}
              onDelete={() =>
                onChangeOther(other, (d) => ({ ...d, needs: d.needs.filter((x) => x !== need) }))
              }
            />
          ))}
          {runsAfterOrBeside.map((other) => (
            <Tooltip key={other} title="Runs it as an after action or in the background — edit that in its workflow file.">
              <Chip size="small" variant="outlined" label={`${other} (after/background)`} />
            </Tooltip>
          ))}
        </Stack>
      </Box>

      <Autocomplete<string>
        size="small"
        options={dependentOptions}
        inputValue={dependentQuery}
        onInputChange={(_, value) => setDependentQuery(value)}
        value={null}
        onChange={(_, other) => {
          if (!other) return;
          setDependentQuery("");
          onChangeOther(other, (d) => ({ ...d, needs: [...d.needs, key] }));
        }}
        renderInput={(params) => (
          <TextField {...params} label="Make a workflow wait for this one" placeholder="Search workflows" />
        )}
      />

      <Autocomplete<Option>
        size="small"
        disabled={locked}
        options={options}
        groupBy={(o) =>
          o.kind === "step" ? "A step of this workflow" : "Another workflow (the whole workflow waits)"
        }
        getOptionLabel={(o) => o.value}
        inputValue={query}
        onInputChange={(_, value) => setQuery(value)}
        value={null}
        onChange={(_, option) => {
          if (!option) return;
          setQuery("");
          onChange((d) =>
            option.kind === "step"
              ? {
                  ...d,
                  steps: d.steps.map((s) =>
                    s.name === step.name ? { ...s, needs: [...s.needs, option.value] } : s,
                  ),
                }
              : { ...d, needs: [...d.needs, option.value] },
          );
        }}
        renderInput={(params) => <TextField {...params} label="Add a dependency" placeholder="Search targets" />}
      />

      <Tooltip
        title={
          routedFrom.length
            ? `${routedFrom.join(", ")} route${routedFrom.length === 1 ? "s" : ""} here on error or retry, so this step can't be removed on its own.`
            : draft.steps.length === 1
              ? "A workflow needs at least one step."
              : "Remove this step from the workflow. Whatever needed it will need what it needed instead."
        }
      >
        <Box component="span">
          <Button
            size="small"
            color="error"
            startIcon={<DeleteIcon />}
            disabled={locked || routedFrom.length > 0 || draft.steps.length === 1}
            onClick={remove}
          >
            Remove step
          </Button>
        </Box>
      </Tooltip>
    </Stack>
  );
}
