/**
 * Picks which checkout every per-project page is looking at.
 *
 * Projects register themselves: every CLI command posts its working directory
 * to `/api/projects` before opening the browser, so this list fills in as you
 * use ciabatta in more repos. Which also means it fills up with repos you've
 * long since stopped working on — hence the remove button.
 */

import { useState } from "react";
import {
  Box,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  Button,
  FormControl,
  IconButton,
  ListItemText,
  MenuItem,
  Select,
  Tooltip,
  Typography,
} from "@mui/material";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api } from "../api/client";
import { queryKeys } from "../api/queries";
import type { Project } from "../api/types";
import { useProjectContext } from "../state/project";

export function ProjectSwitcher() {
  const { projects, projectId, setProjectId, isLoading } = useProjectContext();
  const queryClient = useQueryClient();

  // Confirmed rather than immediate: the row is a few pixels from the one you
  // click to *switch* projects, and the two mistakes are not equally cheap.
  const [pendingRemoval, setPendingRemoval] = useState<Project | null>(null);

  const forget = useMutation({
    mutationFn: (id: string) => api.delete<{ ok: boolean }>(`/api/projects/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.projects });
      setPendingRemoval(null);
    },
  });

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
    <>
      <FormControl size="small" sx={{ minWidth: 200 }}>
        <Select
          value={projectId ?? ""}
          onChange={(event) => setProjectId(event.target.value)}
          displayEmpty
          // Checkout paths are long and arbitrary. Without a ceiling the menu
          // grows past the right edge of the window, taking the remove button
          // with it — so the one control this menu exists to offer becomes
          // unreachable on exactly the machines with the deepest paths.
          MenuProps={{ slotProps: { paper: { sx: { maxWidth: 520 } } } }}
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
            <MenuItem key={project.id} value={project.id} sx={{ pr: 1 }}>
              <ListItemText
                primary={project.name}
                secondary={project.path}
                slotProps={{
                  secondary: {
                    // The tail of a path identifies it; the head is usually
                    // /home/someone/git/… and the same for every row.
                    sx: {
                      direction: "rtl",
                      textAlign: "left",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    },
                  },
                }}
                sx={{ mr: 1, minWidth: 0, flexGrow: 1 }}
              />
              <Tooltip title="Remove from this list">
                <IconButton
                  size="small"
                  edge="end"
                  sx={{ flexShrink: 0 }}
                  onClick={(event) => {
                    // Without this the click also selects the row, so you'd
                    // switch to the project you're trying to get rid of.
                    event.stopPropagation();
                    setPendingRemoval(project);
                  }}
                >
                  <DeleteOutlineIcon fontSize="small" />
                </IconButton>
              </Tooltip>
            </MenuItem>
          ))}
        </Select>
      </FormControl>

      <Dialog open={pendingRemoval !== null} onClose={() => setPendingRemoval(null)}>
        <DialogTitle>Remove {pendingRemoval?.name}?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            This only takes it out of the switcher. Nothing on disk is touched — not the
            checkout, not its <code>.ciabatta/</code> directory, not its cache. Running any
            ciabatta command in {pendingRemoval?.path} will add it back.
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setPendingRemoval(null)}>Cancel</Button>
          <Button
            color="error"
            disabled={forget.isPending}
            onClick={() => pendingRemoval && forget.mutate(pendingRemoval.id)}
          >
            Remove
          </Button>
        </DialogActions>
      </Dialog>
    </>
  );
}
