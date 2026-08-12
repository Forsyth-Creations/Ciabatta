/**
 * Picks which checkout the per-project pages are looking at.
 *
 * Projects register themselves: every CLI command posts its working directory
 * to `/api/projects` before opening the browser, so this list fills in as you
 * use ciabatta in more repos.
 */

import {
  Box,
  FormControl,
  ListItemText,
  MenuItem,
  Select,
  Tooltip,
  Typography,
} from "@mui/material";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";

import { useProjectContext } from "../state/project";

export function ProjectSwitcher() {
  const { projects, projectId, setProjectId, isLoading } = useProjectContext();

  if (isLoading) return null;

  if (projects.length === 0) {
    return (
      <Tooltip title="Run a ciabatta command in a project directory to register it">
        <Typography variant="body2" color="text.secondary">
          No projects yet
        </Typography>
      </Tooltip>
    );
  }

  return (
    <FormControl size="small" sx={{ minWidth: 200 }}>
      <Select
        value={projectId ?? ""}
        onChange={(event) => setProjectId(event.target.value)}
        displayEmpty
        renderValue={(value) => {
          const project = projects.find((p) => p.id === value);
          return (
            <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
              <FolderOpenIcon fontSize="small" />
              <span>{project?.name ?? "Select a project"}</span>
            </Box>
          );
        }}
      >
        {projects.map((project) => (
          <MenuItem key={project.id} value={project.id}>
            <ListItemText primary={project.name} secondary={project.path} />
          </MenuItem>
        ))}
      </Select>
    </FormControl>
  );
}
