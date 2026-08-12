/**
 * Entry point: providers, then the router.
 *
 * The app shell (nav rail, project switcher, health chip) is the router's root
 * route rather than a wrapper around `RouterProvider` — it uses `Link` and
 * `useRouterState`, which need to be inside the router's context.
 */

import CssBaseline from "@mui/material/CssBaseline";
import { ThemeProvider } from "@mui/material/styles";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { router } from "./router";
import { ColorModeProvider } from "./state/colorMode";
import { ProjectProvider } from "./state/project";
import { buildTheme } from "./theme";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // These are local tools reading local state; refetching on focus is
      // cheap and keeps a long-lived tab honest.
      refetchOnWindowFocus: true,
      retry: 1,
      staleTime: 2_000,
    },
  },
});

function App() {
  return (
    <ColorModeProvider>
      {(mode) => (
        <ThemeProvider theme={buildTheme(mode)}>
          <CssBaseline />
          <QueryClientProvider client={queryClient}>
            <ProjectProvider>
              <RouterProvider router={router} />
            </ProjectProvider>
          </QueryClientProvider>
        </ThemeProvider>
      )}
    </ColorModeProvider>
  );
}

const container = document.getElementById("root");
if (!container) throw new Error("#root is missing from index.html");

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
