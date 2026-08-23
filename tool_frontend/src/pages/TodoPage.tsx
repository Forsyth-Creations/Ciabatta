/**
 * The task list for the selected project.
 *
 * Scoped like every other page: the switcher at the top decides which list
 * you're looking at, so tasks written in one repo don't clutter another. What
 * isn't about any one repo belongs on the global list instead — the globe
 * button moves a task there, and it turns up on the dashboard.
 */

import { Alert, Stack, ToggleButton, ToggleButtonGroup, Typography } from "@mui/material";
import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { PageHeader, RequireProject } from "../components/Page";
import { TodoList } from "../components/TodoList";
import { useProjectContext } from "../state/project";

type Filter = "all" | "open" | "done";

export function TodoPage() {
  return <RequireProject>{(projectId) => <Todos projectId={projectId} />}</RequireProject>;
}

function Todos({ projectId }: { projectId: string }) {
  const { project } = useProjectContext();
  const [filter, setFilter] = useState<Filter>("all");

  return (
    <>
      <PageHeader
        title="Todo"
        description={`Tasks for ${project?.name ?? "this project"}. Use the switcher above to see another project's list.`}
        actions={
          <ToggleButtonGroup
            size="small"
            exclusive
            value={filter}
            onChange={(_, next: Filter | null) => next && setFilter(next)}
          >
            <ToggleButton value="all">All</ToggleButton>
            <ToggleButton value="open">Open</ToggleButton>
            <ToggleButton value="done">Done</ToggleButton>
          </ToggleButtonGroup>
        }
      />

      <TodoList
        projectId={projectId}
        filter={(todo) =>
          filter === "all" ? true : filter === "done" ? todo.done : !todo.done
        }
        emptyNote={
          filter === "all" ? "Nothing on this project's list." : `No ${filter} tasks.`
        }
      />

      <Alert severity="info" variant="outlined" sx={{ mt: 4, maxWidth: 900 }}>
        <Stack spacing={0.5}>
          <Typography variant="body2">
            Something that isn&apos;t about this repo? The globe button makes it{" "}
            <strong>global</strong> — it leaves this list and appears on the{" "}
            <Link to="/">dashboard</Link>, where it stays whichever project you switch to.
          </Typography>
          <Typography variant="caption" color="text.secondary">
            From a terminal: <code>ciabatta todo --global &quot;…&quot;</code>
          </Typography>
        </Stack>
      </Alert>
    </>
  );
}
