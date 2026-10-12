import {
  Alert,
  Card,
  LinearProgress,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
} from "@mui/material";

import { api, type Release } from "../api";
import { EmptyRow, Mono, Section, When } from "../components/ui";
import { human } from "../format";
import { useLoad } from "../useLoad";

export function Releases({ refreshKey }: { refreshKey: number }) {
  const { data, error, loading } = useLoad(
    () => api<{ releases: Release[] }>("/api/releases"),
    refreshKey,
  );
  if (error) return <Alert severity="error" sx={{ mt: 3 }}>{error}</Alert>;
  if (!data) return loading ? <LinearProgress sx={{ mt: 3 }} /> : null;
  const releases = data.releases ?? [];

  return (
    <Section
      title="Releases"
      sub={
        <>
          Versions published by a <Mono>kind: release-push</Mono> step or{" "}
          <Mono>ciabatta releases push</Mono>. Retention never evicts these.
        </>
      }
    >
      <TableContainer component={Card}>
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell>Project</TableCell>
              <TableCell>Release</TableCell>
              <TableCell>Version</TableCell>
              <TableCell>Published</TableCell>
              <TableCell>Commit</TableCell>
              <TableCell align="right">Files</TableCell>
              <TableCell align="right">Size</TableCell>
            </TableRow>
          </TableHead>
          <TableBody>
            {releases.length === 0 && (
              <EmptyRow colSpan={7}>
                No releases yet. Add a <Mono>kind: release-push</Mono> step to a workflow, or run{" "}
                <Mono>ciabatta releases push</Mono>.
              </EmptyRow>
            )}
            {releases.map((r) => (
              <TableRow key={`${r.project}/${r.name}/${r.version}`} hover>
                <TableCell>{r.project_name || r.project}</TableCell>
                <TableCell>
                  <Mono>{r.name}</Mono>
                </TableCell>
                <TableCell>
                  <Mono>
                    <b>{r.version}</b>
                  </Mono>
                </TableCell>
                <TableCell>
                  <When at={r.created_at} />
                </TableCell>
                <TableCell>
                  <Mono title={r.commit ?? ""}>{(r.commit ?? "").slice(0, 8) || "—"}</Mono>
                </TableCell>
                <TableCell align="right">{r.files}</TableCell>
                <TableCell align="right">{human(r.size)}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableContainer>
    </Section>
  );
}
