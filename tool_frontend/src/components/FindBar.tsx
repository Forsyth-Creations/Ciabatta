/**
 * Find in a log, the way a browser's find bar does it: every match is
 * highlighted where it sits, and Enter steps through them. The log isn't
 * filtered down to the matching lines — the line before an error is usually
 * the one that explains it, so a match is shown in its context.
 *
 * The bar only reports and steps; what counts as a match, and how a log
 * scrolls to one, are the log's business.
 */

import { IconButton, InputAdornment, Stack, TextField, Tooltip, Typography } from "@mui/material";
import CloseIcon from "@mui/icons-material/Close";
import KeyboardArrowDownIcon from "@mui/icons-material/KeyboardArrowDown";
import KeyboardArrowUpIcon from "@mui/icons-material/KeyboardArrowUp";
import SearchIcon from "@mui/icons-material/Search";

/** Step to the next match (1) or the previous one (-1), wrapping round. */
export function stepMatch(current: number, count: number, by: 1 | -1): number {
  if (count === 0) return 0;
  return (current + by + count) % count;
}

export function FindBar({
  value,
  onChange,
  count,
  current,
  onStep,
  placeholder = "Find in log…",
  width = 220,
  note,
}: {
  value: string;
  onChange: (value: string) => void;
  /** How many matches there are. */
  count: number;
  /** Which of them is the current one, from 0. */
  current: number;
  onStep: (by: 1 | -1) => void;
  placeholder?: string;
  width?: number | string;
  /** Said after the count, e.g. that the matches were capped. */
  note?: string;
}) {
  const searching = value.trim().length > 0;
  return (
    <TextField
      size="small"
      value={value}
      placeholder={placeholder}
      onChange={(event) => onChange(event.target.value)}
      onKeyDown={(event) => {
        if (event.key === "Enter") {
          event.preventDefault();
          onStep(event.shiftKey ? -1 : 1);
        } else if (event.key === "Escape" && searching) {
          event.stopPropagation();
          onChange("");
        }
      }}
      // Shrinks with a narrow pane rather than pushing its toolbar wider.
      sx={{
        flex: `0 1 ${typeof width === "number" ? `${width}px` : width}`,
        minWidth: 150,
        "& .MuiInputBase-root": { height: 30, fontSize: 13, pr: 0.5 },
      }}
      slotProps={{
        htmlInput: { "aria-label": placeholder },
        input: {
          startAdornment: (
            <InputAdornment position="start">
              <SearchIcon sx={{ fontSize: 16 }} />
            </InputAdornment>
          ),
          endAdornment: searching && (
            <InputAdornment position="end">
              <Stack direction="row" alignItems="center">
                <Tooltip title={note ?? ""}>
                  <Typography
                    variant="caption"
                    color={count === 0 ? "error" : "text.secondary"}
                    sx={{ mr: 0.5, whiteSpace: "nowrap" }}
                  >
                    {count === 0
                      ? "no matches"
                      : `${current + 1}/${count.toLocaleString()}${note ? "+" : ""}`}
                  </Typography>
                </Tooltip>
                <Tooltip title="Previous match (Shift+Enter)">
                  <span>
                    <IconButton
                      size="small"
                      disabled={count === 0}
                      onClick={() => onStep(-1)}
                      aria-label="Previous match"
                      sx={{ p: 0.25 }}
                    >
                      <KeyboardArrowUpIcon sx={{ fontSize: 18 }} />
                    </IconButton>
                  </span>
                </Tooltip>
                <Tooltip title="Next match (Enter)">
                  <span>
                    <IconButton
                      size="small"
                      disabled={count === 0}
                      onClick={() => onStep(1)}
                      aria-label="Next match"
                      sx={{ p: 0.25 }}
                    >
                      <KeyboardArrowDownIcon sx={{ fontSize: 18 }} />
                    </IconButton>
                  </span>
                </Tooltip>
                <Tooltip title="Clear (Esc)">
                  <IconButton
                    size="small"
                    onClick={() => onChange("")}
                    aria-label="Clear the search"
                    sx={{ p: 0.25 }}
                  >
                    <CloseIcon sx={{ fontSize: 16 }} />
                  </IconButton>
                </Tooltip>
              </Stack>
            </InputAdornment>
          ),
        },
      }}
    />
  );
}
