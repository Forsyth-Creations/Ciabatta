/**
 * The landing page: where you pick which project to look at.
 *
 * Reached from the logo, not from the nav rail. Everything in the rail is
 * about the selected project; this page is the one step above that — every
 * checkout the daemon knows, to choose between — plus the global todo list,
 * which belongs to none of them.
 */

import {
  Box,
  Button,
  Card,
  CardActionArea,
  CardContent,
  Chip,
  Divider,
  Grid2 as Grid,
  Paper,
  Stack,
  Typography,
} from "@mui/material";
import FolderIcon from "@mui/icons-material/Folder";
import PublicIcon from "@mui/icons-material/Public";
import MenuBookIcon from "@mui/icons-material/MenuBook";
import { Link, useNavigate } from "@tanstack/react-router";

import { PageHeader } from "../components/Page";
import { TodoList } from "../components/TodoList";
import { useHealth } from "../api/queries";
import { useActiveRuns } from "../api/run";
import { useWatchSessions } from "../api/watch";
import { useProjectContext } from "../state/project";
import { monoFontStack } from "../theme";

export function LandingPage() {
  const { data: health } = useHealth();
  const { projects, projectId, setProjectId, isLoading } = useProjectContext();
  const navigate = useNavigate();
  const sessions = useWatchSessions();
  const running = useActiveRuns();

  const open = (id: string) => {
    setProjectId(id);
    navigate({ to: "/dashboard" });
  };

  return (
    <Box>
      <PageHeader
        title="ciabatta"
        description="Pick a project to work in. Everything in the menu on the left — runs, watch sessions, the cache, todos — is about the project you pick here."
      />

      <Typography variant="h3" sx={{ mb: 1.5 }}>
        Projects
      </Typography>
      {!isLoading && projects.length === 0 && (
        <Typography variant="body2" color="text.secondary" sx={{ maxWidth: "70ch" }}>
          No projects yet. Run any ciabatta command inside a checkout — <code>ciabatta list</code>{" "}
          will do — and it appears here.
        </Typography>
      )}
      <Grid container spacing={2}>
        {projects.map((project) => {
          const watching = (sessions.data ?? []).filter(
            (s) => s.project === project.id && s.running,
          ).length;
          return (
            <Grid key={project.id} size={{ xs: 12, sm: 6, lg: 4 }}>
              <Card
                variant="outlined"
                sx={{
                  height: "100%",
                  borderColor: project.id === projectId ? "primary.main" : undefined,
                }}
              >
                <CardActionArea onClick={() => open(project.id)} sx={{ height: "100%" }}>
                  <CardContent>
                    <Stack direction="row" spacing={1.5} alignItems="center" sx={{ mb: 0.5 }}>
                      <FolderIcon color="primary" />
                      <Typography variant="h3" noWrap sx={{ flexGrow: 1, minWidth: 0 }}>
                        {project.name}
                      </Typography>
                      {project.id === projectId && (
                        <Chip size="small" variant="outlined" color="primary" label="selected" />
                      )}
                    </Stack>
                    <Typography
                      variant="caption"
                      color="text.secondary"
                      noWrap
                      title={project.path}
                      sx={{ display: "block", fontFamily: monoFontStack }}
                    >
                      {project.path}
                    </Typography>
                    {watching > 0 && (
                      <Typography variant="caption" color="success.main" sx={{ display: "block", mt: 1 }}>
                        {watching} watch session{watching === 1 ? "" : "s"} running
                      </Typography>
                    )}
                  </CardContent>
                </CardActionArea>
              </Card>
            </Grid>
          );
        })}
      </Grid>
      {running > 0 && (
        <Typography variant="caption" color="warning.main" sx={{ display: "block", mt: 1.5 }}>
          {running} run{running === 1 ? "" : "s"} in progress across your projects.
        </Typography>
      )}

      <GlobalTodos />

      <Stack direction="row" spacing={1} alignItems="center" sx={{ mt: 3 }}>
        <Typography variant="body2" color="text.secondary">
          New here?
        </Typography>
        <Button component={Link} to="/docs" size="small" startIcon={<MenuBookIcon />}>
          Read the docs
        </Button>
      </Stack>

      {health && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 4 }}>
          daemon v{health.version} · pid {health.pid} · started{" "}
          {new Date(health.started_at).toLocaleString()}
        </Typography>
      )}
    </Box>
  );
}

/**
 * The global todo list: the things that aren't about any one repo.
 *
 * It lives on the dashboard rather than behind the project switcher because
 * that's the whole point of it — a task you deliberately detached from a
 * project shouldn't need you to pick a project to find it again.
 */
function GlobalTodos() {
  const { project } = useProjectContext();

  return (
    <Paper variant="outlined" sx={{ p: 2, mt: 4 }}>
      <Stack direction="row" spacing={1} alignItems="center" sx={{ mb: 0.5 }}>
        <PublicIcon fontSize="small" color="primary" />
        <Typography variant="h3">Global todo</Typography>
      </Stack>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2, maxWidth: "70ch" }}>
        Tasks that belong to no project, so they stay put whichever one you switch to. Move
        something here from a project&apos;s list with its globe button, or add it straight
        from a terminal with <code>ciabatta todo --global</code>.
      </Typography>

      <Divider sx={{ mb: 2 }} />

      <TodoList
        projectId={null}
        placeholder="Something that isn't about any one repo…"
        emptyNote="Nothing global yet."
        // A global task can be filed under whichever project is selected; with
        // none selected there is nowhere to move it to, so the button isn't shown.
        moveTarget={project ? { id: project.id, name: project.name } : null}
        // Shipping needs a checkout for the agent to edit, and a global task by
        // definition names none.
        allowShip={false}
      />
    </Paper>
  );
}
