/**
 * Which checkout the app is looking at.
 *
 * One daemon serves many repos, and most state (`ai`, `analyze`,
 * `run`) is per-repo — so nearly every API call carries a `project` id. The
 * exception is todo, which is deliberately global and personal.
 *
 * The choice persists in localStorage, and defaults to whichever project the
 * daemon lists first (the one a CLI command most recently registered).
 */

import { createContext, useContext, useEffect, useMemo, useState } from "react";
import type { ReactNode } from "react";

import { useProjects } from "../api/queries";
import type { Project } from "../api/types";

const STORAGE_KEY = "ciabatta-project";

interface ProjectContextValue {
  projects: Project[];
  project: Project | undefined;
  projectId: string | undefined;
  setProjectId: (id: string) => void;
  isLoading: boolean;
}

const ProjectContext = createContext<ProjectContextValue | undefined>(undefined);

export function ProjectProvider({ children }: { children: ReactNode }) {
  const { data: projects, isLoading } = useProjects();
  const [selected, setSelected] = useState<string | undefined>(
    () => localStorage.getItem(STORAGE_KEY) ?? undefined,
  );

  // Settle on a valid selection once the list arrives: keep the stored choice
  // if it still exists, otherwise fall back to the first project.
  useEffect(() => {
    if (!projects?.length) return;
    const stillExists = projects.some((p) => p.id === selected);
    if (!stillExists) {
      setSelected(projects[0].id);
    }
  }, [projects, selected]);

  const setProjectId = (id: string) => {
    localStorage.setItem(STORAGE_KEY, id);
    setSelected(id);
  };

  const value = useMemo<ProjectContextValue>(
    () => ({
      projects: projects ?? [],
      project: projects?.find((p) => p.id === selected),
      projectId: projects?.some((p) => p.id === selected) ? selected : undefined,
      setProjectId,
      isLoading,
    }),
    [projects, selected, isLoading],
  );

  return <ProjectContext.Provider value={value}>{children}</ProjectContext.Provider>;
}

export function useProjectContext(): ProjectContextValue {
  const context = useContext(ProjectContext);
  if (!context) {
    throw new Error("useProjectContext must be used inside a ProjectProvider");
  }
  return context;
}

/**
 * The selected project id, for pages that require one.
 *
 * Returns `undefined` while the list is loading or when no project is
 * registered yet; pages use that to show the "no project" prompt rather than
 * firing a query that would 400.
 */
export function useProjectId(): string | undefined {
  return useProjectContext().projectId;
}
