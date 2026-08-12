/**
 * The AI assistant: the architecture mind map, pending tag proposals, and
 * background jobs.
 *
 * The map polls with `?after=<seq>` and gets `{changed:false}` back while the
 * brain hasn't moved, so watching a burn-in live costs one small request a
 * second rather than the whole graph.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  Divider,
  LinearProgress,
  Stack,
  Tab,
  Tabs,
  Typography,
} from "@mui/material";
import CheckIcon from "@mui/icons-material/Check";
import CloseIcon from "@mui/icons-material/Close";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useTheme } from "@mui/material/styles";
import type { Theme } from "@mui/material/styles";
import type { Node } from "@xyflow/react";

import { api } from "../api/client";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { GraphCanvas } from "../components/GraphCanvas";
import { radialLayout, toEdges } from "../components/layout";
import { monoFontStack } from "../theme";

interface BrainNode {
  id: string;
  label: string;
  kind: "architecture" | "file";
  description?: string;
  knowledge?: number;
  tags?: string[];
  provisional?: boolean;
}

interface BrainEdge {
  from: string;
  to: string;
  score?: number;
  provisional?: boolean;
}

interface BrainGraph {
  changed: boolean;
  seq?: number;
  activity?: string | null;
  nodes?: BrainNode[];
  edges?: BrainEdge[];
  pending?: { file: string; tags: string[] }[];
  confidence?: number;
}

interface Job {
  id: number;
  prompt: string;
  source: string;
  status: string;
  created_at: string;
  error?: string | null;
  changed_files?: string[];
}

export function AiPage() {
  const [tab, setTab] = useState(0);

  return (
    <>
      <PageHeader
        title="AI"
        description="The architecture mind map the assistant builds as it learns this codebase, plus the tasks running in the background."
      />
      <Tabs value={tab} onChange={(_, next) => setTab(next)} sx={{ mb: 2 }}>
        <Tab label="Mind map" />
        <Tab label="Jobs" />
      </Tabs>
      <RequireProject>
        {(project) => (tab === 0 ? <MindMap project={project} /> : <Jobs project={project} />)}
      </RequireProject>
    </>
  );
}

function MindMap({ project }: { project: string }) {
  const theme = useTheme();
  const queryClient = useQueryClient();
  const [seq, setSeq] = useState(0);
  const [graph, setGraph] = useState<BrainGraph | null>(null);
  const [selected, setSelected] = useState<BrainNode | null>(null);

  const { isLoading, error } = useQuery({
    queryKey: ["ai", "graph", project, seq],
    queryFn: async () => {
      const next = await api.get<BrainGraph>(
        `/api/ai/graph?project=${encodeURIComponent(project)}&after=${seq}`,
      );
      // Unchanged responses still carry `activity`, so merge rather than
      // replace — that's what keeps the status line live during a burn-in.
      if (next.changed) {
        setGraph(next);
        if (typeof next.seq === "number") setSeq(next.seq);
      } else {
        setGraph((previous) => (previous ? { ...previous, activity: next.activity } : previous));
      }
      return next;
    },
    refetchInterval: 1_000,
  });

  const confirm = useMutation({
    mutationFn: (body: { file: string; accept: boolean }) =>
      api.post("/api/ai/confirm", { project, ...body }),
    onSuccess: () => setSeq(0),
  });

  const confirmAll = useMutation({
    mutationFn: (accept: boolean) => api.post("/api/ai/confirm-all", { project, accept }),
    onSuccess: () => setSeq(0),
  });

  const prune = useMutation({
    mutationFn: (body: { kind: string; id: string; tag?: string }) =>
      api.post("/api/ai/prune", { project, tag: "", ...body }),
    onSuccess: () => {
      setSeq(0);
      setSelected(null);
      queryClient.invalidateQueries({ queryKey: ["ai", "graph", project] });
    },
  });

  const { nodes, edges } = useMemo(() => buildFlowGraph(graph, theme), [graph, theme]);

  if (isLoading && !graph) return <Loading label="Loading the mind map…" />;
  if (error) return <ErrorNote error={error} />;

  const nodeCount = graph?.nodes?.length ?? 0;
  const architectures = graph?.nodes?.filter((n) => n.kind === "architecture") ?? [];
  const pending = graph?.pending ?? [];

  if (nodeCount === 0) {
    return (
      <Alert severity="info">
        The map is empty. Run <code style={{ fontFamily: monoFontStack }}>ciabatta ai burn-in</code>{" "}
        to have the assistant traverse the codebase and build it, or just start asking questions —
        it learns as it goes.
      </Alert>
    );
  }

  return (
    <>
      <Stack direction="row" spacing={1.5} alignItems="center" sx={{ mb: 1.5 }} flexWrap="wrap" useFlexGap>
        <Chip size="small" variant="outlined" label={`${architectures.length} architectures`} />
        <Chip
          size="small"
          variant="outlined"
          label={`${nodeCount - architectures.length} files`}
        />
        {typeof graph?.confidence === "number" && (
          <Chip
            size="small"
            variant="outlined"
            label={`confidence ${Math.round(graph.confidence)}`}
          />
        )}
        <Box sx={{ flexGrow: 1 }} />
        {pending.length > 0 && (
          <>
            <Typography variant="body2" color="text.secondary">
              {pending.length} pending
            </Typography>
            <Button size="small" startIcon={<CheckIcon />} onClick={() => confirmAll.mutate(true)}>
              Accept all
            </Button>
            <Button
              size="small"
              color="error"
              startIcon={<CloseIcon />}
              onClick={() => confirmAll.mutate(false)}
            >
              Reject all
            </Button>
          </>
        )}
      </Stack>

      {graph?.activity && (
        <Box sx={{ mb: 1.5 }}>
          <LinearProgress />
          <Typography variant="caption" color="text.secondary">
            {graph.activity}
          </Typography>
        </Box>
      )}

      <GraphCanvas
        nodes={nodes}
        edges={edges}
        height={560}
        onNodeClick={(_, node) => {
          const found = graph?.nodes?.find((n) => n.id === node.id) ?? null;
          setSelected(found);
        }}
        nodeColor={(node) =>
          node.data?.kind === "architecture"
            ? theme.palette.primary.main
            : theme.palette.text.disabled
        }
      />

      {selected && (
        <Box sx={{ mt: 2, p: 2, border: 1, borderColor: "divider", borderRadius: 1 }}>
          <Stack direction="row" alignItems="center" spacing={1.5}>
            <Chip
              size="small"
              color={selected.kind === "architecture" ? "primary" : "default"}
              label={selected.kind}
            />
            <Typography sx={{ fontFamily: monoFontStack, flexGrow: 1, wordBreak: "break-all" }}>
              {selected.label}
            </Typography>
            <Button
              size="small"
              color="error"
              onClick={() =>
                prune.mutate({
                  kind: selected.kind === "architecture" ? "architecture" : "file",
                  id: selected.label,
                })
              }
            >
              Forget
            </Button>
          </Stack>
          {selected.description && (
            <Typography variant="body2" color="text.secondary" sx={{ mt: 1 }}>
              {selected.description}
            </Typography>
          )}
          {selected.tags && selected.tags.length > 0 && (
            <Stack direction="row" spacing={0.5} sx={{ mt: 1 }} flexWrap="wrap" useFlexGap>
              {selected.tags.map((tag) => (
                <Chip key={tag} size="small" variant="outlined" label={tag} />
              ))}
            </Stack>
          )}
        </Box>
      )}

      {pending.length > 0 && (
        <>
          <Divider sx={{ my: 3 }} />
          <Typography variant="h3" sx={{ mb: 1.5 }}>
            Pending tag proposals
          </Typography>
          <Stack spacing={1}>
            {pending.map((proposal) => (
              <Stack key={proposal.file} direction="row" alignItems="center" spacing={1.5}>
                <Typography sx={{ fontFamily: monoFontStack, fontSize: 13, flexGrow: 1 }}>
                  {proposal.file}
                </Typography>
                {proposal.tags.map((tag) => (
                  <Chip key={tag} size="small" variant="outlined" label={tag} />
                ))}
                <Button
                  size="small"
                  onClick={() => confirm.mutate({ file: proposal.file, accept: true })}
                >
                  Accept
                </Button>
                <Button
                  size="small"
                  color="error"
                  onClick={() => confirm.mutate({ file: proposal.file, accept: false })}
                >
                  Reject
                </Button>
              </Stack>
            ))}
          </Stack>
        </>
      )}
    </>
  );
}

/** Turn the brain's node/edge lists into a positioned react-flow graph. */
function buildFlowGraph(graph: BrainGraph | null, theme: Theme) {
  if (!graph?.nodes) return { nodes: [] as Node[], edges: [] };

  const architectures = graph.nodes.filter((n) => n.kind === "architecture");
  const files = graph.nodes.filter((n) => n.kind === "file");
  const brainEdges = graph.edges ?? [];

  // A file's cluster is whichever architecture links to it first.
  const ownerOf = new Map<string, string>();
  for (const edge of brainEdges) {
    if (!ownerOf.has(edge.to)) ownerOf.set(edge.to, edge.from);
  }

  const positioned = radialLayout(
    architectures.map((node) => ({
      id: node.id,
      data: { label: node.label, kind: node.kind },
    })),
    files.map((node) => ({
      id: node.id,
      hub: ownerOf.get(node.id) ?? null,
      // Files are shown by basename: full paths make the map unreadable at any
      // zoom that fits more than a handful of nodes.
      data: { label: basename(node.label), kind: node.kind },
    })),
  );

  const byId = new Map(graph.nodes.map((n) => [n.id, n]));
  const nodes: Node[] = positioned.map((node) => {
    const source = byId.get(node.id);
    const isArch = source?.kind === "architecture";
    return {
      ...node,
      style: {
        background: isArch ? theme.palette.primary.main : theme.palette.background.paper,
        color: isArch ? theme.palette.primary.contrastText : theme.palette.text.primary,
        border: `1px solid ${isArch ? theme.palette.primary.main : theme.palette.divider}`,
        borderRadius: 8,
        fontSize: isArch ? 13 : 11,
        fontWeight: isArch ? 600 : 400,
        padding: isArch ? "8px 14px" : "4px 8px",
        // Provisional (unconfirmed) nodes read as ghosts.
        opacity: source?.provisional ? 0.5 : 1,
        borderStyle: source?.provisional ? "dashed" : "solid",
      },
    };
  });

  return {
    nodes,
    edges: toEdges(brainEdges, { dashed: (pair) => Boolean(byId.get(pair.to)?.provisional) }),
  };
}

function basename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

function Jobs({ project }: { project: string }) {
  const { data, isLoading, error } = useQuery({
    queryKey: ["ai", "jobs", project],
    queryFn: () => api.get<{ jobs: Job[] }>(`/api/ai/jobs?project=${encodeURIComponent(project)}`),
    refetchInterval: 2_000,
  });

  if (isLoading) return <Loading label="Loading jobs…" />;
  if (error) return <ErrorNote error={error} />;

  const jobs = data?.jobs ?? [];
  if (jobs.length === 0) {
    return (
      <Alert severity="info">
        No background tasks. Ship one with{" "}
        <code style={{ fontFamily: monoFontStack }}>ciabatta ai ship "…"</code>, or from the Todo
        page.
      </Alert>
    );
  }

  return (
    <Stack spacing={1}>
      {jobs.map((job) => (
        <Box key={job.id} sx={{ p: 1.5, border: 1, borderColor: "divider", borderRadius: 1 }}>
          <Stack direction="row" alignItems="center" spacing={1.5}>
            <Chip size="small" color={jobStatusColor(job.status)} label={job.status} />
            <Typography sx={{ flexGrow: 1 }}>{job.prompt}</Typography>
            <Typography variant="caption" color="text.secondary">
              #{job.id} · {job.source}
            </Typography>
          </Stack>
          {job.error && (
            <Typography variant="body2" color="error" sx={{ mt: 1 }}>
              {job.error}
            </Typography>
          )}
          {job.changed_files && job.changed_files.length > 0 && (
            <Typography
              variant="caption"
              color="text.secondary"
              sx={{ display: "block", mt: 1, fontFamily: monoFontStack }}
            >
              {job.changed_files.join(", ")}
            </Typography>
          )}
        </Box>
      ))}
    </Stack>
  );
}

function jobStatusColor(status: string): "success" | "error" | "warning" | "default" {
  switch (status) {
    case "done":
    case "succeeded":
      return "success";
    case "failed":
      return "error";
    case "running":
      return "warning";
    default:
      return "default";
  }
}
