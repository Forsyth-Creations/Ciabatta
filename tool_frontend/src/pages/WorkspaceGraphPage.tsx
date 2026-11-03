/**
 * Every workflow and step in the workspace, as one graph — for finding what
 * dangles.
 *
 * `ciabatta self graph` opens this, and so does the Workspace page's "Whole
 * graph" button. It's the run page's graph editor over every workflow at
 * once: a workflow nothing connects to, and a step its workflow leaves
 * unordered, are marked, and connecting one (or making another workflow wait
 * for it) clears the mark straight away. Nothing is written until Save.
 */

import { Button } from "@mui/material";
import ArrowBackIcon from "@mui/icons-material/ArrowBack";
import { Link as RouterLink } from "@tanstack/react-router";

import { GraphEditor } from "../components/GraphEditor";
import { PageHeader, RequireProject } from "../components/Page";

const NO_STATUS: Record<string, string> = {};

export function WorkspaceGraphPage() {
  return (
    <>
      <PageHeader
        title="Whole graph"
        description="Every workflow in the workspace, in context with one another. Workflows nothing connects to, and steps nothing orders, are marked in amber — connect them here, or remove them if they're no longer used."
        actions={
          <Button
            component={RouterLink}
            to="/workspace"
            size="small"
            startIcon={<ArrowBackIcon />}
          >
            Workspace
          </Button>
        }
      />
      <RequireProject>
        {(project) => (
          <GraphEditor
            key={project}
            project={project}
            units={["*"]}
            status={NO_STATUS}
            title="Every workflow"
            height="calc(100vh - 300px)"
            showDangling
          />
        )}
      </RequireProject>
    </>
  );
}
