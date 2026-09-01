/**
 * The monorepo catalogue and its workflow graphs.
 *
 * Two questions, one page. **What is there to run, and who owns it?** — the
 * searchable list of every sub-workspace's workflows, so nobody has to open six
 * packages to find out what a script does. And **what will actually happen?** —
 * the compiled graph for one workflow, wave by wave, with every node labelled
 * by the sub-workspace it came from.
 *
 * Starting a workflow posts to the run API with a `workflow` field; the daemon
 * compiles the graph itself, so the browser and `ciabatta build` can't disagree
 * about what runs.
 */

import { useMemo, useState } from "react";
import {
  Accordion,
  AccordionDetails,
  AccordionSummary,
  Alert,
  AlertTitle,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  FormControlLabel,
  InputAdornment,
  MenuItem,
  Stack,
  Switch,
  TextField,
  Tooltip,
  Typography,
} from "@mui/material";
import AccountTreeIcon from "@mui/icons-material/AccountTree";
import BoltIcon from "@mui/icons-material/Bolt";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import PlayArrowIcon from "@mui/icons-material/PlayArrow";
import PublishIcon from "@mui/icons-material/Publish";
import SearchIcon from "@mui/icons-material/Search";
import { useNavigate } from "@tanstack/react-router";

import { useStartRun } from "../api/run";
import type { EnvVar } from "../api/types";
import {
  useWorkflowGraph,
  useWorkspace,
  workflowMatches,
  type GraphNode,
  type MemberSummary,
  type WorkflowGraph,
  type WorkflowStep,
  type WorkflowSummary,
} from "../api/workspace";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { EnvDriftBanner } from "../components/EnvDriftBanner";
import { EnvPanel, EnvVarChip, StepEnvChips } from "../components/EnvVars";
import {
  GraphInputsPanel,
  GraphOutputsPanel,
  StageCacheBadge,
} from "../components/CacheFiles";
import { useCachePlan } from "../api/cache";
import type { PlannedStep } from "../api/types";
import { monoFontStack } from "../theme";

export function WorkspacePage() {
  return (
    <>
      <PageHeader
        title="Workspace"
        description="Every workflow in the monorepo — what it does, who owns it, and what it needs first. Pick one to see the graph that would run, across all the packages that take part."
      />
      <RequireProject>{(project) => <Workspace project={project} />}</RequireProject>
    </>
  );
}

function Workspace({ project }: { project: string }) {
  const { data, isLoading, error } = useWorkspace(project);
  const [search, setSearch] = useState("");
  const [workflow, setWorkflow] = useState<string | null>(null);

  if (isLoading) return <Loading label="Reading the workspace…" />;
  if (error) return <ErrorNote error={error} />;
  if (!data) return null;

  if (data.members.length === 0) {
    return (
      <Alert severity="info">
        <AlertTitle>No sub-workspaces yet</AlertTitle>
        Run <code style={{ fontFamily: monoFontStack }}>ciabatta init --lib</code> inside a
        package to opt it in. It writes a <code>[workspace]</code> identity and a starter
        workflow, and the package shows up here.
      </Alert>
    );
  }

  return (
    <Stack spacing={3}>
      <EnvDriftBanner project={project} />
      <Stack direction="row" spacing={2} flexWrap="wrap" useFlexGap alignItems="flex-start">
        <TextField
          size="small"
          placeholder="protoc, Ada, deploy…"
          label="Search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          sx={{ minWidth: 280 }}
          helperText="Names, descriptions, owners, tags, and the commands steps run."
          slotProps={{
            input: {
              startAdornment: (
                <InputAdornment position="start">
                  <SearchIcon fontSize="small" />
                </InputAdornment>
              ),
            },
          }}
        />
        <TextField
          select
          size="small"
          label="Graph a workflow"
          value={workflow ?? ""}
          onChange={(e) => setWorkflow(e.target.value || null)}
          sx={{ minWidth: 220 }}
          helperText="Compiles it across every package that defines one."
        >
          <MenuItem value="">
            <em>None</em>
          </MenuItem>
          {data.workflows.map((name) => (
            <MenuItem key={name} value={name}>
              {name}
            </MenuItem>
          ))}
        </TextField>
        <Box sx={{ flexGrow: 1 }} />
        <Typography variant="caption" color="text.secondary" sx={{ pt: 1 }}>
          {data.members.length} sub-workspace(s) · {data.workflows.length} workflow name(s)
          <br />
          <code style={{ fontFamily: monoFontStack }}>{data.root}</code>
        </Typography>
      </Stack>

      {workflow && <GraphPanel project={project} workflow={workflow} />}

      <Catalogue members={data.members} search={search} onGraph={setWorkflow} />
    </Stack>
  );
}

// ─── Catalogue ──────────────────────────────────────────────────────────────

function Catalogue({
  members,
  search,
  onGraph,
}: {
  members: MemberSummary[];
  search: string;
  onGraph: (workflow: string) => void;
}) {
  // Filter to what matches, dropping any package left with no workflows —
  // an empty card is noise, not information.
  const filtered = useMemo(
    () =>
      members
        .map((member) => ({
          member,
          workflows: member.workflows.filter((w) => workflowMatches(member, w, search)),
        }))
        .filter(({ workflows }) => workflows.length > 0),
    [members, search],
  );

  if (filtered.length === 0) {
    // Nothing to show has two quite different causes: a search that matched
    // nothing, and packages that simply haven't declared a workflow yet.
    return search.trim() ? (
      <Typography variant="body2" color="text.secondary">
        Nothing matches “{search}”.
      </Typography>
    ) : (
      <Alert severity="info">
        <AlertTitle>No workflows yet</AlertTitle>
        These packages have opted in but haven’t declared a workflow. Add one as{" "}
        <code style={{ fontFamily: monoFontStack }}>.ciabatta/workflows/build.toml</code>, and
        every package with a workflow of that name joins the same{" "}
        <code style={{ fontFamily: monoFontStack }}>ciabatta build</code> graph.
      </Alert>
    );
  }

  return (
    <Stack spacing={2}>
      {filtered.map(({ member, workflows }) => (
        <Card key={member.name}>
          <CardContent>
            <Stack direction="row" alignItems="baseline" spacing={1.5} flexWrap="wrap" useFlexGap>
              <Typography variant="h6" sx={{ fontFamily: monoFontStack }}>
                {member.name}
              </Typography>
              <Typography variant="caption" color="text.secondary">
                {member.path}
              </Typography>
              <Chip size="small" variant="outlined" label={member.owner} />
              {member.tags.map((tag) => (
                <Chip key={tag} size="small" label={tag} />
              ))}
            </Stack>

            {member.description && (
              <Typography variant="body2" color="text.secondary" sx={{ mt: 0.5 }}>
                {member.description}
              </Typography>
            )}
            {member.depends_on.length > 0 && (
              <Typography variant="caption" color="text.secondary">
                depends on {member.depends_on.join(", ")}
              </Typography>
            )}

            <Box sx={{ mt: 1.5 }}>
              {workflows.map((workflow) => (
                <WorkflowRow
                  key={workflow.name}
                  workflow={workflow}
                  onGraph={() => onGraph(workflow.name)}
                />
              ))}
            </Box>
          </CardContent>
        </Card>
      ))}
    </Stack>
  );
}

function WorkflowRow({
  workflow,
  onGraph,
}: {
  workflow: WorkflowSummary;
  onGraph: () => void;
}) {
  return (
    <Accordion disableGutters elevation={0} sx={{ "&:before": { display: "none" } }}>
      <AccordionSummary expandIcon={<ExpandMoreIcon />}>
        <Stack
          direction="row"
          spacing={1.5}
          alignItems="center"
          flexWrap="wrap"
          useFlexGap
          sx={{ flexGrow: 1, pr: 2 }}
        >
          <Typography sx={{ fontFamily: monoFontStack, fontWeight: 600 }}>
            {workflow.name}
          </Typography>
          <Typography variant="body2" color="text.secondary" sx={{ flexGrow: 1 }}>
            {workflow.description ?? "(no description)"}
          </Typography>
          <Chip size="small" variant="outlined" label={workflow.owner} />
          <Chip size="small" label={`${workflow.steps.length} step(s)`} />
        </Stack>
      </AccordionSummary>
      <AccordionDetails>
        <Stack spacing={1}>
          {workflow.needs.length > 0 && (
            <Typography variant="caption" color="text.secondary">
              after {workflow.needs.join(", ")}
            </Typography>
          )}
          {workflow.required_env.length > 0 && (
            <Typography variant="caption" color="text.secondary">
              requires the variables {workflow.required_env.join(", ")} to be set
            </Typography>
          )}
          {workflow.steps.map((step) => (
            <StepRow key={step.name} step={step} />
          ))}
          <Box>
            <Button size="small" startIcon={<AccountTreeIcon />} onClick={onGraph}>
              Graph “{workflow.name}” across the workspace
            </Button>
          </Box>
        </Stack>
      </AccordionDetails>
    </Accordion>
  );
}

function StepRow({ step }: { step: WorkflowStep }) {
  return (
    <Box sx={{ pl: 1, borderLeft: 2, borderColor: "divider" }}>
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        <Typography variant="body2" sx={{ fontFamily: monoFontStack }}>
          {step.name}
        </Typography>
        <StepBadges step={step} />
      </Stack>
      <Typography variant="caption" color="text.secondary" display="block">
        {step.description ?? step.command ?? "(no description)"}
      </Typography>
    </Box>
  );
}

/**
 * The markers that change what "this step finished" means — plus the phase it
 * belongs to. Anything not shown here behaves the ordinary way.
 */
/**
 * What the lightning bolt means, in one sentence.
 *
 * Shared by the badge and the section heading deliberately: they are the same
 * claim, and two copies of it are two things to keep true.
 */
const BACKGROUND_TOOLTIP =
  "A background task: started when the run reaches it, and nothing ever waits for it. " +
  "Use it for the mini-servers a build or a dev loop needs running alongside it — a mock API, " +
  "a bundler in watch mode. It gates no step, so it can't hold the graph up, and it is stopped " +
  "when the run finishes. Declare one with `persistent: true`.";

function StepBadges({ step }: { step: WorkflowStep }) {
  return (
    <>
      {step.push && (
        <Tooltip title={step.workflow ? `Publishes the "${step.workflow}" workflow` : "Publishes"}>
          <Chip size="small" color="primary" icon={<PublishIcon />} label="push" />
        </Tooltip>
      )}
      {!step.push && step.kind && <Chip size="small" variant="outlined" label={step.kind} />}
      {step.persistent && (
        <Tooltip title={BACKGROUND_TOOLTIP}>
          <Chip size="small" color="info" variant="outlined" icon={<BoltIcon />} label="background" />
        </Tooltip>
      )}
      {step.timeout && (
        <Tooltip title="Killed past this, and the rest of the graph carries on">
          <Chip size="small" variant="outlined" label={`timeout ${step.timeout}`} />
        </Tooltip>
      )}
      {step.retries > 0 && <Chip size="small" variant="outlined" label={`${step.retries} retries`} />}
      {step.continue_on_error && (
        <Tooltip title="Its failure skips dependents but doesn't stop the run">
          <Chip size="small" variant="outlined" label="non-blocking" />
        </Tooltip>
      )}
      {step.requires.map((tool) => (
        <Chip key={tool} size="small" variant="outlined" label={tool} sx={{ opacity: 0.7 }} />
      ))}
      <StepEnvChips env={step.env} />
    </>
  );
}

// ─── Graph ──────────────────────────────────────────────────────────────────

function GraphPanel({ project, workflow }: { project: string; workflow: string }) {
  const navigate = useNavigate();
  const { data, isLoading, error } = useWorkflowGraph(project, workflow);
  // What each stage would reuse. A cache that isn't configured simply yields
  // nothing here, so the graph looks exactly as it did before caching existed.
  const { data: plan } = useCachePlan(project, workflow);
  const start = useStartRun();
  const [dryRun, setDryRun] = useState(false);

  if (isLoading) return <Loading label={`Compiling the “${workflow}” graph…`} />;
  if (error) return <ErrorNote error={error} />;
  if (!data) return null;

  const launch = () => {
    start.mutate(
      { project, workflows: [], workflow, dry_run: dryRun },
      { onSuccess: (run) => navigate({ to: "/run/$runId", params: { runId: String(run.id) } }) },
    );
  };

  const byId = new Map(data.nodes.map((node) => [node.id, node]));
  const recoveries = data.nodes.filter((node) => node.recover);
  // Background tasks come out of the waves entirely. A wave means "these run
  // together, and the next wave waits for them" — and nothing waits for these,
  // so drawing one inside a wave states a dependency the engine does not have.
  // They get their own section at the bottom instead, out of the flow they are
  // not part of.
  const background = data.nodes.filter((node) => node.persistent && !node.recover);
  const isBackground = new Set(background.map((node) => node.id));
  const planned = new Map((plan?.steps ?? []).map((step) => [step.name, step]));
  const caching = plan?.caching ?? false;

  return (
    <Card variant="outlined">
      <CardContent>
        <Stack direction="row" spacing={2} alignItems="center" flexWrap="wrap" useFlexGap>
          <Typography variant="h6" sx={{ fontFamily: monoFontStack }}>
            {data.workflow}
          </Typography>
          <Typography variant="body2" color="text.secondary" sx={{ flexGrow: 1 }}>
            {data.nodes.filter((n) => !n.recover && !n.persistent).length} step(s) across{" "}
            {data.units.length} sub-workspace(s), in {data.waves.length} wave(s)
            {background.length > 0 && ` · ${background.length} background`}
          </Typography>
          <FormControlLabel
            control={<Switch checked={dryRun} onChange={(e) => setDryRun(e.target.checked)} />}
            label="Dry run"
          />
          <Button
            variant="contained"
            startIcon={<PlayArrowIcon />}
            onClick={launch}
            disabled={start.isPending || data.empty}
          >
            Run it
          </Button>
        </Stack>

        {start.error && <ErrorNote error={start.error} />}

        {data.missing_tools.length > 0 && (
          <Alert severity="warning" sx={{ mt: 2 }}>
            <AlertTitle>
              {data.missing_tools.length} build tool(s) this machine doesn’t have
            </AlertTitle>
            <Stack spacing={0.5}>
              {data.missing_tools.map((tool) => (
                <Typography key={tool.tool} variant="body2">
                  <strong>{tool.tool}</strong> — needed by {tool.needed_by.join(", ")}
                  {tool.hint && (
                    <>
                      {" · "}
                      <code style={{ fontFamily: monoFontStack }}>{tool.hint}</code>
                    </>
                  )}
                </Typography>
              ))}
            </Stack>
          </Alert>
        )}

        <Stack spacing={2} sx={{ mt: 2 }}>
          {/* The graph's inputs, before its first wave: everything the steps
              read that isn't produced by another step. */}
          <EnvPanel report={data.env} title="environment — read before wave 1" />
          {caching && plan && <GraphInputsPanel plan={plan} />}

          {data.waves.map((wave, index) => {
            const steps = wave.filter((id) => !isBackground.has(id));
            // A wave that held nothing but background tasks isn't a wave.
            if (steps.length === 0) return null;
            return (
              <Box key={index}>
                <Typography variant="overline" color="text.secondary">
                  wave {index + 1} — runs in parallel
                </Typography>
                <Stack spacing={1} sx={{ mt: 0.5 }}>
                  {steps.map((id) => {
                    const node = byId.get(id);
                    return node ? (
                      <GraphNodeCard
                        key={id}
                        node={node}
                        env={envFor(data, id)}
                        planned={planned.get(id)}
                      />
                    ) : null;
                  })}
                </Stack>
              </Box>
            );
          })}

          {caching && plan && <GraphOutputsPanel plan={plan} />}

          {background.length > 0 && (
            <Box>
              <Tooltip title={BACKGROUND_TOOLTIP}>
                <Stack
                  direction="row"
                  spacing={0.5}
                  alignItems="center"
                  sx={{ width: "fit-content", cursor: "help" }}
                >
                  <BoltIcon fontSize="small" color="info" />
                  <Typography variant="overline" color="text.secondary">
                    background — started alongside the run, nothing waits for them
                  </Typography>
                </Stack>
              </Tooltip>
              <Stack spacing={1} sx={{ mt: 0.5 }}>
                {background.map((node) => (
                  <GraphNodeCard
                    key={node.id}
                    node={node}
                    env={envFor(data, node.id)}
                    planned={planned.get(node.id)}
                  />
                ))}
              </Stack>
            </Box>
          )}

          {recoveries.length > 0 && (
            <Box>
              <Typography variant="overline" color="text.secondary">
                recovery nodes — entered only when a step fails
              </Typography>
              <Stack spacing={1} sx={{ mt: 0.5 }}>
                {recoveries.map((node) => (
                  <GraphNodeCard key={node.id} node={node} env={envFor(data, node.id)} />
                ))}
              </Stack>
            </Box>
          )}
        </Stack>
      </CardContent>
    </Card>
  );
}

/** The variables one node depends on, with the values this machine resolves. */
function envFor(graph: WorkflowGraph, id: string): EnvVar[] {
  return graph.env.vars.filter((variable) => variable.steps.includes(id));
}

/**
 * One node. The sub-workspace it came from is the most prominent thing after
 * its name: in a graph drawn from six packages, "which package is this?" is the
 * first question anyone asks.
 */
function GraphNodeCard({
  node,
  env,
  planned,
}: {
  node: GraphNode;
  env: EnvVar[];
  planned?: PlannedStep;
}) {
  // What the node declares for itself is already a badge; this is what it
  // reads from the environment around it, resolved.
  const own = new Set(Object.keys(node.env));
  const reads = env.filter((variable) => !own.has(variable.key));

  return (
    <Box
      sx={{
        p: 1,
        borderLeft: 3,
        // Three kinds of node, three colours: the flow, the branch entered only
        // on failure, and the background task that is in neither.
        borderColor: node.recover
          ? "warning.main"
          : node.persistent
            ? "info.main"
            : "primary.main",
        bgcolor: "action.hover",
        borderRadius: 1,
      }}
    >
      <Stack direction="row" spacing={1} alignItems="center" flexWrap="wrap" useFlexGap>
        {node.workspace && (
          <Chip size="small" color="secondary" label={node.workspace} sx={{ fontWeight: 600 }} />
        )}
        <Typography variant="body2" sx={{ fontFamily: monoFontStack, fontWeight: 600 }}>
          {node.id}
        </Typography>
        <StepBadges step={node} />
        <Box sx={{ flexGrow: 1 }} />
        <Typography variant="caption" color="text.secondary">
          {node.owner ?? "unowned"}
        </Typography>
      </Stack>

      {node.description && (
        <Typography variant="caption" color="text.secondary" display="block">
          {node.description}
        </Typography>
      )}
      {node.needs.length > 0 && (
        <Typography variant="caption" color="text.secondary" display="block">
          after {node.needs.join(", ")}
        </Typography>
      )}
      {node.on_error && (
        <Typography variant="caption" color="warning.main" display="block">
          on failure → {node.on_error}
        </Typography>
      )}
      {node.cwd && (
        <Typography
          variant="caption"
          color="text.secondary"
          display="block"
          sx={{ fontFamily: monoFontStack }}
        >
          {node.cwd}
        </Typography>
      )}
      {reads.length > 0 && (
        <Stack
          direction="row"
          spacing={0.75}
          sx={{ mt: 0.5 }}
          flexWrap="wrap"
          useFlexGap
          alignItems="center"
        >
          <Typography variant="caption" color="text.secondary">
            reads
          </Typography>
          {reads.map((variable) => (
            <EnvVarChip key={variable.key} variable={variable} />
          ))}
        </Stack>
      )}

      {/* What the cache would do with this stage, and — on a miss — the diff
          that explains it. Absent entirely when caching is off, so a graph
          without a cache looks exactly as it always did. */}
      {planned && <StageCacheBadge step={planned} />}
    </Box>
  );
}
