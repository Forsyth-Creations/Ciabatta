/**
 * A todo list, at whatever scope it's pointed at.
 *
 * Two views use this: the Todo page shows the selected project's list, and the
 * dashboard shows the global one. They are the same list with a different
 * filter, so they're the same component — the alternative was two copies of the
 * priority select and the inline editor, drifting apart a change at a time.
 *
 * A task moves between the two scopes with one button: **make global** on a
 * project's task, **move here** on a global one.
 */

import { useEffect, useRef, useState } from "react";
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
  Tooltip,
  Typography,
} from "@mui/material";
import AddIcon from "@mui/icons-material/Add";
import AutoAwesomeIcon from "@mui/icons-material/AutoAwesome";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutline";
import PublicIcon from "@mui/icons-material/Public";
import SouthEastIcon from "@mui/icons-material/SouthEast";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { api } from "../api/client";
import type { Priority, Todo } from "../api/types";
import { ErrorNote, Loading } from "./Page";

const PRIORITIES: Priority[] = ["high", "medium", "low"];

const PRIORITY_COLOR: Record<Priority, "error" | "warning" | "default"> = {
  high: "error",
  medium: "warning",
  low: "default",
};

/** The query key and `project` parameter for one scope. */
export function todoKey(projectId: string | null) {
  return ["todos", projectId ?? "global"] as const;
}

interface TodoListProps {
  /** The project whose list this is, or null for the global list. */
  projectId: string | null;
  /** Which tasks to show. */
  filter?: (todo: Todo) => boolean;
  /** Placeholder for the add box. */
  placeholder?: string;
  /** What to say when the list is empty. */
  emptyNote?: string;
  /**
   * Where "move here" would file a global task. Only shown on the global list,
   * and only when a project is actually selected — otherwise there's nowhere to
   * move it to.
   */
  moveTarget?: { id: string; name: string } | null;
  /** Hide the "ship to the assistant" button (it needs a checkout). */
  allowShip?: boolean;
  /** Cap the number of rows drawn, for the dashboard's summary view. */
  limit?: number;
  /** Rendered under the list when `limit` hid something. */
  footer?: (hidden: number) => React.ReactNode;
}

export function TodoList({
  projectId,
  filter,
  placeholder = "What needs doing?",
  emptyNote = "Nothing on this list.",
  moveTarget = null,
  allowShip = true,
  limit,
  footer,
}: TodoListProps) {
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState("");
  const queryKey = todoKey(projectId);

  const params = projectId ? `?project=${encodeURIComponent(projectId)}` : "";
  const { data: todos, isLoading, error } = useQuery({
    queryKey,
    queryFn: () => api.get<Todo[]>(`/api/todos${params}`),
  });

  // Every mutation returns the refreshed list for this scope, so the response
  // *is* the new cache value — no refetch round trip. It also means a task
  // promoted to global vanishes from the project view in the same reply.
  const add = useTodoMutation("/api/todos", projectId, queryKey, queryClient);
  const toggle = useTodoMutation("/api/todos/toggle", projectId, queryKey, queryClient);
  const remove = useTodoMutation("/api/todos/delete", projectId, queryKey, queryClient);
  const setPriority = useTodoMutation("/api/todos/priority", projectId, queryKey, queryClient);
  const edit = useTodoMutation("/api/todos/edit", projectId, queryKey, queryClient);
  const setScope = useTodoMutation("/api/todos/scope", projectId, queryKey, queryClient);

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

  const matching = (todos ?? []).filter((todo) => (filter ? filter(todo) : true));
  const visible = limit ? matching.slice(0, limit) : matching;
  const hidden = matching.length - visible.length;

  return (
    <>
      <Box component="form" onSubmit={submit} sx={{ mb: 2, maxWidth: 720 }}>
        <TextField
          fullWidth
          size="small"
          placeholder={placeholder}
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
      {edit.error && <ErrorNote error={edit.error} />}
      {setScope.error && <ErrorNote error={setScope.error} />}
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
          {emptyNote}
        </Typography>
      ) : (
        <Stack spacing={1} sx={{ maxWidth: 900 }}>
          {visible.map((todo) => (
            <Card key={todo.id} sx={{ px: 1.5, py: 1 }}>
              <Stack direction="row" alignItems="flex-start" spacing={1.5}>
                <Checkbox
                  checked={todo.done}
                  onChange={() => toggle.mutate({ id: todo.id })}
                  size="small"
                  sx={{ mt: -0.25 }}
                />

                <EditableText
                  todo={todo}
                  onSave={(text) => edit.mutate({ id: todo.id, text })}
                />

                <ScopeButton
                  todo={todo}
                  moveTarget={moveTarget}
                  disabled={setScope.isPending}
                  onMove={(target) =>
                    setScope.mutate({ id: todo.id, target: target ?? null })
                  }
                />

                {/* The select is the only priority control — a separate chip
                    showing the same word next to it was pure duplication. */}
                <Select
                  size="small"
                  value={todo.priority}
                  onChange={(e) =>
                    setPriority.mutate({ id: todo.id, priority: e.target.value as Priority })
                  }
                  sx={{ width: 132, flexShrink: 0 }}
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

                {allowShip && (
                  <Tooltip title="Hand this task to the AI assistant to complete in the background">
                    <span>
                      <IconButton
                        size="small"
                        disabled={ship.isPending}
                        onClick={() => ship.mutate(todo.id)}
                      >
                        <AutoAwesomeIcon fontSize="small" />
                      </IconButton>
                    </span>
                  </Tooltip>
                )}

                <IconButton
                  size="small"
                  sx={{ flexShrink: 0 }}
                  onClick={() => remove.mutate({ id: todo.id })}
                >
                  <DeleteOutlineIcon fontSize="small" />
                </IconButton>
              </Stack>
            </Card>
          ))}
        </Stack>
      )}

      {hidden > 0 && footer?.(hidden)}
    </>
  );
}

/**
 * Move a task between its project and the global list.
 *
 * One button, because there are only ever two places a task can be and it is
 * always in exactly one of them.
 */
function ScopeButton({
  todo,
  moveTarget,
  disabled,
  onMove,
}: {
  todo: Todo;
  moveTarget: { id: string; name: string } | null;
  disabled: boolean;
  onMove: (target: string | null) => void;
}) {
  if (todo.project === null) {
    // A global task can be filed under a project — but only if there's one
    // selected to file it under.
    if (!moveTarget) {
      return (
        <Tooltip title="On the global list. Select a project above to move it there.">
          <PublicIcon fontSize="small" sx={{ color: "text.disabled", mt: 0.75, flexShrink: 0 }} />
        </Tooltip>
      );
    }
    return (
      <Tooltip title={`Move to ${moveTarget.name}`}>
        <span>
          <IconButton
            size="small"
            disabled={disabled}
            sx={{ flexShrink: 0 }}
            onClick={() => onMove(moveTarget.id)}
          >
            <SouthEastIcon fontSize="small" />
          </IconButton>
        </span>
      </Tooltip>
    );
  }

  return (
    <Tooltip title="Make global — move it off this project's list and onto the dashboard">
      <span>
        <IconButton
          size="small"
          disabled={disabled}
          sx={{ flexShrink: 0 }}
          onClick={() => onMove(null)}
        >
          <PublicIcon fontSize="small" />
        </IconButton>
      </span>
    </Tooltip>
  );
}

/**
 * A task's text, editable where it sits.
 *
 * Multi-line: a task is often a paragraph, and a single-line box that scrolls
 * sideways makes anything longer than a sentence unreadable while you're
 * editing it. Which means Enter has to insert a newline, so saving moves to
 * Cmd/Ctrl+Enter — and to clicking away, because after typing that means "keep
 * it" far more often than it means the opposite. Escape abandons.
 */
function EditableText({ todo, onSave }: { todo: Todo; onSave: (text: string) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(todo.text);
  const input = useRef<HTMLTextAreaElement>(null);

  // A change from elsewhere (another tab, the CLI) shouldn't be clobbered by a
  // stale draft the next time this opens.
  useEffect(() => {
    if (!editing) setDraft(todo.text);
  }, [todo.text, editing]);

  useEffect(() => {
    if (editing) input.current?.select();
  }, [editing]);

  const commit = () => {
    const text = draft.trim();
    setEditing(false);
    // An empty edit is a mis-key, not a request to delete: the bin does that.
    if (!text || text === todo.text) {
      setDraft(todo.text);
      return;
    }
    onSave(text);
  };

  if (editing) {
    return (
      <Box sx={{ flexGrow: 1, minWidth: 0 }}>
        <TextField
          inputRef={input}
          size="small"
          multiline
          minRows={2}
          maxRows={16}
          autoFocus
          fullWidth
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={commit}
          onKeyDown={(event) => {
            if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
              event.preventDefault();
              commit();
            } else if (event.key === "Escape") {
              event.preventDefault();
              setDraft(todo.text);
              setEditing(false);
            }
          }}
        />
        <Typography variant="caption" color="text.secondary">
          Enter for a new line · ⌘/Ctrl+Enter to save · Esc to cancel
        </Typography>
      </Box>
    );
  }

  return (
    <Tooltip title="Click to edit" enterDelay={600}>
      <Typography
        onClick={() => setEditing(true)}
        sx={{
          flexGrow: 1,
          minWidth: 0,
          cursor: "text",
          // Multi-line text was typed with its line breaks meaning something,
          // so show them rather than collapsing the task into one run-on line.
          whiteSpace: "pre-wrap",
          overflowWrap: "anywhere",
          textDecoration: todo.done ? "line-through" : "none",
          color: todo.done ? "text.disabled" : "text.primary",
          borderRadius: 0.5,
          px: 0.5,
          py: 0.5,
          "&:hover": { bgcolor: "action.hover" },
        }}
      >
        {todo.text}
      </Typography>
    </Tooltip>
  );
}

/**
 * A mutation that replaces the cached list with the server's reply.
 *
 * The scope goes on every request so the reply is filtered the same way the
 * list is — otherwise an edit would hand back a different list than the one on
 * screen.
 */
function useTodoMutation(
  path: string,
  projectId: string | null,
  queryKey: readonly unknown[],
  queryClient: ReturnType<typeof useQueryClient>,
) {
  return useMutation({
    mutationFn: (body: Record<string, unknown>) =>
      api.post<Todo[]>(path, { ...body, project: projectId }),
    onSuccess: (next) => {
      queryClient.setQueryData(queryKey, next);
      // A move changes two lists; whichever one isn't on screen has to be
      // refetched before it's shown again.
      queryClient.invalidateQueries({ queryKey: ["todos"] });
    },
  });
}
