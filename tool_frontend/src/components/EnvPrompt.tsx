/**
 * The dialog that asks for the variables a run can't start without.
 *
 * Shared by the launcher and by "run again", because the daemon refuses both
 * the same way: a 422 listing what it couldn't resolve, rather than a run that
 * would abort at its own `REQUIRED_ENV` gate a second later.
 */

import { useState } from "react";
import {
  Button,
  Dialog,
  DialogActions,
  DialogContent,
  DialogContentText,
  DialogTitle,
  Stack,
  TextField,
} from "@mui/material";

import { monoFontStack } from "../theme";

/**
 * Ask for the variables a run can't start without.
 *
 * The daemon already looked in its own environment and in whatever `.env` files
 * the workflow sources, so anything listed here genuinely has nowhere else to come
 * from. Values are used for this launch only — nothing is written to disk.
 */
export function EnvPrompt({
  variables,
  initial,
  pending,
  onCancel,
  onSubmit,
}: {
  variables: string[];
  initial: Record<string, string>;
  pending: boolean;
  onCancel: () => void;
  onSubmit: (values: Record<string, string>) => void;
}) {
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(variables.map((name) => [name, initial[name] ?? ""])),
  );

  // Blank is the state the daemon already rejected, so requiring a value here
  // saves a round trip that could only come back with the same question.
  const complete = variables.every((name) => values[name]?.trim());

  return (
    <Dialog open fullWidth maxWidth="sm" onClose={onCancel}>
      <DialogTitle>This run needs a few variables</DialogTitle>
      <DialogContent>
        <DialogContentText sx={{ mb: 2 }}>
          {variables.length === 1
            ? "One variable the run requires isn't set. Give it a value to continue."
            : `${variables.length} variables the run requires aren't set. Give them values to continue.`}
        </DialogContentText>
        <Stack spacing={2}>
          {variables.map((name) => (
            <TextField
              key={name}
              label={name}
              value={values[name] ?? ""}
              onChange={(e) => setValues((prev) => ({ ...prev, [name]: e.target.value }))}
              size="small"
              fullWidth
              autoFocus={name === variables[0]}
              slotProps={{ input: { sx: { fontFamily: monoFontStack } } }}
            />
          ))}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onCancel}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!complete || pending}
          onClick={() => onSubmit(values)}
        >
          Run
        </Button>
      </DialogActions>
    </Dialog>
  );
}
