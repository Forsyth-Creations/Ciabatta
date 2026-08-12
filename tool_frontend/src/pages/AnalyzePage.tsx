/**
 * The codebase dependency graph.
 *
 * A scan walks the whole tree, so it's an explicit action rather than something
 * that happens on page load. The result is the same `ciabatta-analyze.json` the
 * CLI writes, so running `ciabatta analyze` in a terminal updates this view too.
 */

import { useMemo, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Chip,
  FormControlLabel,
  Grid2 as Grid,
  LinearProgress,
  Stack,
  Switch,
  TextField,
  Typography,
} from "@mui/material";
import RefreshIcon from "@mui/icons-material/Refresh";
import {
  createColumnHelper,
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  useReactTable,
  type SortingState,
} from "@tanstack/react-table";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../api/client";
import { ErrorNote, Loading, PageHeader, RequireProject } from "../components/Page";
import { monoFontStack } from "../theme";

interface Node {
  id: string;
  label: string;
  category: "internal" | "external" | "publish" | "requirement" | string;
  version?: string | null;
  files?: string[];
  vulnerabilities?: { id: string; severity?: string }[];
}

interface Graph {
  scanned: boolean;
  nodes?: Node[];
  edges?: { from: string; to: string }[];
}

const CATEGORY_COLOR: Record<string, "primary" | "secondary" | "success" | "default"> = {
  internal: "primary",
  external: "secondary",
  publish: "success",
};

export function AnalyzePage() {
  return (
    <>
      <PageHeader
        title="Analyze"
        description="The dependency graph for this project — internal packages, external dependencies, and where artifacts get published."
      />
      <RequireProject>{(project) => <GraphView project={project} />}</RequireProject>
    </>
  );
}

function GraphView({ project }: { project: string }) {
  const queryClient = useQueryClient();
  const [checkVulns, setCheckVulns] = useState(false);
  const [filter, setFilter] = useState("");
  const [sorting, setSorting] = useState<SortingState>([]);

  const graphKey = ["analyze", "graph", project];

  const { data: graph, isLoading, error } = useQuery({
    queryKey: graphKey,
    queryFn: () => api.get<Graph>(`/api/analyze/graph?project=${encodeURIComponent(project)}`),
  });

  // While a scan runs, poll so the table refreshes on its own when it lands.
  const { data: status } = useQuery({
    queryKey: ["analyze", "status", project],
    queryFn: () => api.get<{ running: boolean }>(`/api/analyze/status?project=${encodeURIComponent(project)}`),
    refetchInterval: (query) => (query.state.data?.running ? 1_000 : false),
  });

  const scanning = status?.running ?? false;

  const scan = useMutation({
    mutationFn: () =>
      api.post<{ ok: boolean; error?: string }>("/api/analyze/scans", {
        project,
        check_vulns: checkVulns,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["analyze", "status", project] });
    },
  });

  // Refresh the graph once a running scan finishes.
  const wasScanning = useRunTransition(scanning, () => {
    queryClient.invalidateQueries({ queryKey: graphKey });
  });

  const nodes = useMemo(() => graph?.nodes ?? [], [graph]);
  const columns = useMemo(() => buildColumns(), []);

  const table = useReactTable({
    data: nodes,
    columns,
    state: { sorting, globalFilter: filter },
    onSortingChange: setSorting,
    onGlobalFilterChange: setFilter,
    getCoreRowModel: getCoreRowModel(),
    getSortedRowModel: getSortedRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
  });

  if (isLoading) return <Loading label="Loading the graph…" />;
  if (error) return <ErrorNote error={error} />;

  const counts = countByCategory(nodes);

  return (
    <>
      <Stack direction="row" spacing={2} alignItems="center" sx={{ mb: 2 }} flexWrap="wrap" useFlexGap>
        <Button
          variant="contained"
          startIcon={<RefreshIcon />}
          onClick={() => scan.mutate()}
          disabled={scanning || scan.isPending}
        >
          {graph?.scanned ? "Rescan" : "Run a scan"}
        </Button>

        <FormControlLabel
          control={
            <Switch
              size="small"
              checked={checkVulns}
              onChange={(e) => setCheckVulns(e.target.checked)}
              disabled={scanning}
            />
          }
          label="Check vulnerabilities (OSV)"
        />

        <Box sx={{ flexGrow: 1 }} />

        <TextField
          size="small"
          placeholder="Filter nodes…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          sx={{ minWidth: 240 }}
        />
      </Stack>

      {scanning && (
        <Box sx={{ mb: 2 }}>
          <LinearProgress />
          <Typography variant="caption" color="text.secondary">
            Scanning the tree{checkVulns && " and querying OSV"}…
          </Typography>
        </Box>
      )}

      {scan.data?.ok === false && <Alert severity="warning" sx={{ mb: 2 }}>{scan.data.error}</Alert>}
      {scan.error && <ErrorNote error={scan.error} />}

      {!graph?.scanned && !scanning && !wasScanning ? (
        <Alert severity="info">
          This project hasn't been analyzed yet. Run a scan to build the dependency graph.
        </Alert>
      ) : (
        <>
          <Grid container spacing={2} sx={{ mb: 3 }}>
            {(["internal", "external", "publish"] as const).map((category) => (
              <Grid key={category} size={{ xs: 6, sm: 3 }}>
                <Box sx={{ border: 1, borderColor: "divider", borderRadius: 1, p: 1.5 }}>
                  <Typography variant="h2">{counts[category] ?? 0}</Typography>
                  <Typography variant="caption" color="text.secondary">
                    {category}
                  </Typography>
                </Box>
              </Grid>
            ))}
            <Grid size={{ xs: 6, sm: 3 }}>
              <Box sx={{ border: 1, borderColor: "divider", borderRadius: 1, p: 1.5 }}>
                <Typography variant="h2">{graph?.edges?.length ?? 0}</Typography>
                <Typography variant="caption" color="text.secondary">
                  edges
                </Typography>
              </Box>
            </Grid>
          </Grid>

          <Box sx={{ overflowX: "auto", border: 1, borderColor: "divider", borderRadius: 1 }}>
            <Box component="table" sx={{ width: "100%", borderCollapse: "collapse", minWidth: 640 }}>
              <Box component="thead">
                {table.getHeaderGroups().map((group) => (
                  <Box component="tr" key={group.id}>
                    {group.headers.map((header) => (
                      <Box
                        component="th"
                        key={header.id}
                        onClick={header.column.getToggleSortingHandler()}
                        sx={{
                          textAlign: "left",
                          px: 1.5,
                          py: 1,
                          borderBottom: 1,
                          borderColor: "divider",
                          cursor: "pointer",
                          userSelect: "none",
                          fontSize: 13,
                          fontWeight: 600,
                          whiteSpace: "nowrap",
                        }}
                      >
                        {flexRender(header.column.columnDef.header, header.getContext())}
                        {{ asc: " ↑", desc: " ↓" }[header.column.getIsSorted() as string] ?? ""}
                      </Box>
                    ))}
                  </Box>
                ))}
              </Box>
              <Box component="tbody">
                {table.getRowModel().rows.map((row) => (
                  <Box
                    component="tr"
                    key={row.id}
                    sx={{ "&:hover": { bgcolor: "action.hover" } }}
                  >
                    {row.getVisibleCells().map((cell) => (
                      <Box
                        component="td"
                        key={cell.id}
                        sx={{ px: 1.5, py: 0.75, borderBottom: 1, borderColor: "divider", fontSize: 13 }}
                      >
                        {flexRender(cell.column.columnDef.cell, cell.getContext())}
                      </Box>
                    ))}
                  </Box>
                ))}
              </Box>
            </Box>
          </Box>

          <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 1.5 }}>
            {table.getRowModel().rows.length} of {nodes.length} nodes
          </Typography>
        </>
      )}
    </>
  );
}

function buildColumns() {
  const column = createColumnHelper<Node>();
  return [
    column.accessor("label", {
      header: "Name",
      cell: (info) => (
        <Box component="span" sx={{ fontFamily: monoFontStack }}>
          {info.getValue()}
        </Box>
      ),
    }),
    column.accessor("category", {
      header: "Category",
      cell: (info) => (
        <Chip
          size="small"
          variant="outlined"
          color={CATEGORY_COLOR[info.getValue()] ?? "default"}
          label={info.getValue()}
        />
      ),
    }),
    column.accessor("version", {
      header: "Version",
      cell: (info) => info.getValue() ?? "—",
    }),
    column.accessor((row) => row.files?.length ?? 0, {
      id: "files",
      header: "Files",
    }),
    column.accessor((row) => row.vulnerabilities?.length ?? 0, {
      id: "vulns",
      header: "Vulns",
      cell: (info) => {
        const count = info.getValue();
        return count > 0 ? <Chip size="small" color="error" label={count} /> : "—";
      },
    }),
  ];
}

function countByCategory(nodes: Node[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const node of nodes) {
    counts[node.category] = (counts[node.category] ?? 0) + 1;
  }
  return counts;
}

/**
 * Fire `onFinish` when `running` goes true → false, and report whether a run
 * has been seen this session. Used to refresh the graph exactly once when a
 * scan completes, rather than polling the (potentially large) graph itself.
 */
function useRunTransition(running: boolean, onFinish: () => void): boolean {
  const [everRan, setEverRan] = useState(false);
  const [previous, setPrevious] = useState(running);

  if (running !== previous) {
    setPrevious(running);
    if (running) {
      setEverRan(true);
    } else if (everRan) {
      onFinish();
    }
  }

  return everRan;
}
