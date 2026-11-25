import { useEffect, useState } from "react";
import {
  Alert,
  Box,
  Button,
  Card,
  CardContent,
  Chip,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  LinearProgress,
  MenuItem,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  TextField,
  Typography,
} from "@mui/material";

import { api, type Project, type SecretGroup } from "../api";
import { EmptyRow, Mono, Prose, Section } from "../components/ui";
import { useLoad } from "../useLoad";

const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;
const GROUP = /^[A-Za-z0-9_-]+$/;

function secretsPath(project: string, group?: string, name?: string) {
  let path = `/api/projects/${encodeURIComponent(project)}/secrets`;
  if (group) path += `/${encodeURIComponent(group)}`;
  if (name) path += `/${encodeURIComponent(name)}`;
  return path;
}

export function Secrets({ refreshKey }: { refreshKey: number }) {
  const projects = useLoad(() => api<Project[]>("/api/projects"), refreshKey);
  const [project, setProject] = useState<string>("");

  useEffect(() => {
    if (!project && projects.data && projects.data.length > 0)
      setProject(projects.data[0].id);
  }, [project, projects.data]);

  if (projects.error)
    return (
      <Alert severity="error" sx={{ mt: 3 }}>
        {projects.error}
      </Alert>
    );
  if (!projects.data)
    return projects.loading ? <LinearProgress sx={{ mt: 3 }} /> : null;

  const sub =
    'Values a workflow needs that no checkout should hold, kept here and nowhere else, in groups such as `dev` and `prod`. A value can be set or replaced here but never read back. In a terminal, `eval "$(ciabatta secrets use dev)"` puts a group into the environment, and a job offloaded from it runs with the same group.';

  return (
    <Section title="Secrets" sub={<Prose text={sub} />}>
      {projects.data.length === 0 ? (
        <Alert severity="info">
          No projects yet. A workspace registers itself the first time it uses
          this cache.
        </Alert>
      ) : (
        <>
          <TextField
            select
            size="small"
            label="Project"
            value={project}
            onChange={(e) => setProject(e.target.value)}
            sx={{ mb: 2, minWidth: 260 }}
          >
            {projects.data.map((p) => (
              <MenuItem key={p.id} value={p.id}>
                {p.name}{" "}
                <Typography
                  component="span"
                  color="text.secondary"
                  sx={{ ml: 1, fontSize: "0.8em" }}
                >
                  {p.id.slice(0, 8)}
                </Typography>
              </MenuItem>
            ))}
          </TextField>
          {project && (
            <ProjectSecrets
              key={project}
              project={project}
              refreshKey={refreshKey}
            />
          )}
        </>
      )}
    </Section>
  );
}

function ProjectSecrets({
  project,
  refreshKey,
}: {
  project: string;
  refreshKey: number;
}) {
  const { data, error, loading, reload } = useLoad(
    () => api<{ groups: SecretGroup[] }>(secretsPath(project)),
    refreshKey,
  );
  const [group, setGroup] = useState("dev");
  const [name, setName] = useState("");
  const [value, setValue] = useState("");
  const [notice, setNotice] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [removing, setRemoving] = useState<{
    group: string;
    name: string;
  } | null>(null);

  if (error)
    return (
      <Alert severity="error" sx={{ mt: 1 }}>
        {error}
      </Alert>
    );
  if (!data) return loading ? <LinearProgress sx={{ mt: 1 }} /> : null;

  const groupOk = GROUP.test(group);
  const nameOk = NAME.test(name);
  const rows = data.groups.flatMap((g) =>
    g.secrets.map((s) => ({ group: g.name, ...s })),
  );
  const exists = rows.some((r) => r.group === group && r.name === name);

  return (
    <>
      {actionError && (
        <Alert
          severity="error"
          sx={{ mb: 2 }}
          onClose={() => setActionError(null)}
        >
          {actionError}
        </Alert>
      )}
      {notice && (
        <Alert
          severity="success"
          sx={{ mb: 2 }}
          onClose={() => setNotice(null)}
        >
          {notice}
        </Alert>
      )}

      <Card sx={{ mb: 2 }}>
        <CardContent>
          <Box
            component="form"
            autoComplete="off"
            sx={{
              display: "flex",
              flexWrap: "wrap",
              gap: 2,
              alignItems: "flex-start",
            }}
            onSubmit={async (event) => {
              event.preventDefault();
              try {
                await api(secretsPath(project, group, name), {
                  method: "PUT",
                  body: JSON.stringify({ value }),
                });
                setNotice(
                  `${exists ? "Replaced" : "Set"} ${name} in '${group}'.`,
                );
                setActionError(null);
                setName("");
                setValue("");
                reload();
              } catch (e) {
                setActionError((e as Error).message);
              }
            }}
          >
            <TextField
              size="small"
              label="Group"
              required
              value={group}
              error={group !== "" && !groupOk}
              helperText={
                group !== "" && !groupOk
                  ? "letters, digits, - and _"
                  : "e.g. dev or prod"
              }
              onChange={(e) => setGroup(e.target.value.trim())}
              sx={{ width: 140 }}
            />
            <TextField
              size="small"
              label="Name"
              required
              value={name}
              error={name !== "" && !nameOk}
              helperText={
                name !== "" && !nameOk
                  ? "a variable name: letters, digits and _"
                  : exists
                    ? "exists — saving replaces it"
                    : "the variable it becomes"
              }
              onChange={(e) => setName(e.target.value.trim())}
              slotProps={{ htmlInput: { spellCheck: false } }}
              sx={{ width: 280 }}
            />
            <TextField
              size="small"
              label="Value"
              type="password"
              required
              value={value}
              autoComplete="new-password"
              helperText="never shown again"
              onChange={(e) => setValue(e.target.value)}
              sx={{ flex: "1 1 220px", minWidth: 200 }}
            />
            <Button
              type="submit"
              variant="contained"
              disabled={!groupOk || !nameOk || value === ""}
              sx={{ mt: 0.25 }}
            >
              {exists ? "Replace" : "Set"}
            </Button>
          </Box>
        </CardContent>
      </Card>

      <TableContainer component={Card}>
        <Table size="small">
          <TableHead>
            <TableRow>
              <TableCell>Group</TableCell>
              <TableCell>Name</TableCell>
              <TableCell>Set</TableCell>
              <TableCell />
            </TableRow>
          </TableHead>
          <TableBody>
            {rows.length === 0 && (
              <EmptyRow colSpan={4}>No secrets for this project yet.</EmptyRow>
            )}
            {rows.map((r) => (
              <TableRow key={`${r.group}/${r.name}`} hover>
                <TableCell>
                  <Chip size="small" variant="outlined" label={r.group} />
                </TableCell>
                <TableCell>
                  <Mono>{r.name}</Mono>
                </TableCell>
                <TableCell sx={{ color: "text.secondary" }}>
                  {new Date(r.updated_at).toLocaleString()}
                  {r.updated_by ? ` by ${r.updated_by}` : ""}
                </TableCell>
                <TableCell align="right" sx={{ whiteSpace: "nowrap" }}>
                  <Button
                    size="small"
                    onClick={() => {
                      setGroup(r.group);
                      setName(r.name);
                      setValue("");
                    }}
                  >
                    Replace
                  </Button>
                  <Button
                    size="small"
                    color="error"
                    onClick={() =>
                      setRemoving({ group: r.group, name: r.name })
                    }
                  >
                    Remove
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </TableContainer>

      <Dialog open={removing !== null} onClose={() => setRemoving(null)}>
        <DialogTitle>
          Remove {removing?.name} from '{removing?.group}'?
        </DialogTitle>
        <DialogContent>
          <DialogContentText>
            Runs that use this group stop getting it. There is no undo: the
            value isn't kept anywhere else.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setRemoving(null)}>Cancel</Button>
          <Button
            color="error"
            variant="contained"
            onClick={async () => {
              const target = removing!;
              setRemoving(null);
              try {
                await api(secretsPath(project, target.group, target.name), {
                  method: "DELETE",
                });
                setNotice(`Removed ${target.name} from '${target.group}'.`);
                setActionError(null);
                reload();
              } catch (e) {
                setActionError((e as Error).message);
              }
            }}
          >
            Remove
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}
