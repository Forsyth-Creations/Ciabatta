/**
 * The personal task list.
 *
 * Unlike every other page, this one is not project-scoped — the list lives in
 * `~/.ciabatta/todos.json` and follows you across checkouts. The one exception
 * is "ship to AI", which has to name a project because the assistant edits
 * files.
 */

import { useState } from "react";
import {
  Box,
  Button,
  Card,
  Checkbox,
  Chip,
  IconButton,
  InputAdornment,
  MenuItem,
  Select,
  Stack,
  TextField,
  ToggleButton,
  ToggleButtonGroup,
  Tooltip,
  Typography,
} from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import AutoAwesomeIcon from "@mui/icons-material/AutoAwesome";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../api/client";
import { queryKeys } from "../api/queries";
import type { Priority, Todo } from "../api/types";
import { ErrorNote, Loading, PageHeader } from "../components/Page";
import { useProjectId } from "../state/project";

type Filter = "all" | "open" | "done";

const PRIORITIES: Priority[] = ["high", "medium", "low"];

const PRIORITY_COLOR: Record<Priority, "error" | "warning" | "default"> = {
  high: "error",
  medium: "warning",
  low: "default",
};

export function TodoPage() {
  const queryClient = useQueryClient();
  const projectId = useProjectId();
  const [draft, setDraft] = useState("");
  const [filter, setFilter] = useState<Filter>("all");

  const { data: todos, isLoading, error } = useQuery({
    queryKey: queryKeys.todos,
    queryFn: () => api.get<Todo[]>("/api/todos"),
  });

  // Every mutation returns the full refreshed list, so the response *is* the
  // new cache value — no refetch round trip.
  const add = useTodoMutation(queryClient, "/api/todos");
  const toggle = useTodoMutation(queryClient, "/api/todos/toggle");
  const remove = useTodoMutation(queryClient, "/api/todos/delete");
  const setPriority = useTodoMutation(queryClient, "/api/todos/priority");

  const ship = useMutation({
    mutationFn: (id: number) =>
      api.post<{ job: number }>("/api/todos/ship", { id, project: projectId }),
  });

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    add.mutate({ text });
    setDraft("");
  };

  const visible = (todos ?? []).filter((todo) =>
    filter === "all" ? true : filter === "done" ? todo.done : !todo.done,
  );
  const openCount = (todos ?? []).filter((t) => !t.done).length;

  return (
    <>
      <PageHeader
        title="Todo"
        description="Your personal task list, stored in ~/.ciabatta/todos.json and shared across every project."
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

      <Box component="form" onSubmit={submit} sx={{ mb: 3, maxWidth: 720 }}>
        <TextField
          fullWidth
          size="small"
          placeholder="What needs doing?"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          slotProps={{
            input: {
              endAdornment: (
                <InputAdornment position="end">
                  <Button type="submit" startIcon={<AddIcon />} disabled={!draft.trim()}>
                    Add
                  </Button>
                </InputAdornment>
              ),
            },
          }}
        />
      </Box>

      {error && <ErrorNote error={error} />}
      {ship.error && <ErrorNote error={ship.error} />}
      {ship.isSuccess && (
        <Typography variant="body2" color="success.main" sx={{ mb: 2 }}>
          Shipped to the assistant as job #{ship.data.job}. Watch it on the AI page.
        </Typography>
      )}

      {isLoading ? (
        <Loading label="Loading tasks…" />
      ) : visible.length === 0 ? (
        <Typography variant="body2" color="text.secondary">
          {filter === "all" ? "Nothing on the list." : `No ${filter} tasks.`}
        </Typography>
      ) : (
        <Stack spacing={1} sx={{ maxWidth: 900 }}>
          {visible.map((todo) => (
            <Card key={todo.id} sx={{ px: 1.5, py: 1 }}>
              <Stack direction="row" alignItems="center" spacing={1.5}>
                <Checkbox
                  checked={todo.done}
                  onChange={() => toggle.mutate({ id: todo.id })}
                  size="small"
                />

                <Typography
                  sx={{
                    flexGrow: 1,
                    minWidth: 0,
                    textDecoration: todo.done ? "line-through" : "none",
                    color: todo.done ? "text.disabled" : "text.primary",
                  }}
                >
                  {todo.text}
                </Typography>

                {/* The select is the only priority control — a separate chip
                    showing the same word next to it was pure duplication. */}
                <Select
                  size="small"
                  value={todo.priority}
                  onChange={(e) =>
                    setPriority.mutate({ id: todo.id, priority: e.target.value as Priority })
                  }
                  sx={{ width: 132 }}
                  renderValue={(value) => (
                    <Chip
                      size="small"
                      variant="outlined"
                      color={PRIORITY_COLOR[value as Priority]}
                      label={value}
                    />
                  )}
                >
                  {PRIORITIES.map((p) => (
                    <MenuItem key={p} value={p}>
                      {p}
                    </MenuItem>
                  ))}
                </Select>

                <Tooltip
                  title={
                    projectId
                      ? "Hand this task to the AI assistant to complete in the background"
                      : "Register a project first — the assistant needs a checkout to work in"
                  }
                >
                  {/* span so the tooltip still shows while the button is disabled */}
                  <span>
                    <IconButton
                      size="small"
                      disabled={!projectId || ship.isPending}
                      onClick={() => ship.mutate(todo.id)}
                    >
                      <AutoAwesomeIcon fontSize="small" />
                    </IconButton>
                  </span>
                </Tooltip>

                <IconButton size="small" onClick={() => remove.mutate({ id: todo.id })}>
                  <DeleteOutlineIcon fontSize="small" />
                </IconButton>
              </Stack>
            </Card>
          ))}
        </Stack>
      )}

      {todos && todos.length > 0 && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 3 }}>
          {openCount} open · {todos.length - openCount} done
        </Typography>
      )}
    </>
  );
}

/** A mutation that replaces the cached list with the server's reply. */
function useTodoMutation(queryClient: ReturnType<typeof useQueryClient>, path: string) {
  return useMutation({
    mutationFn: (body: Record<string, unknown>) => api.post<Todo[]>(path, body),
    onSuccess: (todos) => queryClient.setQueryData(queryKeys.todos, todos),
  });
}
