/**
 * The selected project's README, rendered.
 *
 * Rendered the way GitHub renders it. Raw HTML in the README — a centred
 * logo, a `<details>` block, badges — is parsed and then sanitised with
 * GitHub's own allow-list (`rehype-sanitize`'s default schema): what GitHub
 * would show is shown, and `<script>`, event handlers and the rest are
 * removed. That matters here more than on GitHub: this page holds the
 * daemon's token. react-markdown builds React elements, never an HTML string.
 *
 * Images the README links relatively (`docs/arch.png`) are fetched from the
 * daemon, which serves images from inside the project and nothing else.
 * Relative links to other files have nowhere to go in this app, so they're
 * shown as what they are rather than as links that would 404.
 */

import { Box, Link as MuiLink, Typography } from "@mui/material";
import { alpha } from "@mui/material/styles";
import { useQuery } from "@tanstack/react-query";
import ReactMarkdown, { type Components } from "react-markdown";
import rehypeRaw from "rehype-raw";
import rehypeSanitize from "rehype-sanitize";
import remarkGfm from "remark-gfm";

import { ApiError, api, streamUrl } from "../api/client";
import { ErrorNote, Loading } from "../components/Page";
import { useProjectContext } from "../state/project";
import { monoFontStack } from "../theme";

interface Readme {
  file: string;
  text: string;
}

/** Whether `url` points somewhere of its own, rather than into the repo. */
const isAbsolute = (url: string) => /^[a-z][a-z0-9+.-]*:/i.test(url) || url.startsWith("//");

export function ReadmePage() {
  const { project, projectId } = useProjectContext();

  if (!projectId) {
    return (
      <Typography color="text.secondary">
        Pick a project in the switcher above to see its README.
      </Typography>
    );
  }
  return (
    <Box sx={{ maxWidth: 960 }}>
      <ReadmeView projectId={projectId} projectName={project?.name} />
    </Box>
  );
}

/** The project's README, as one fetch shared by every view of it. */
export function useReadme(projectId: string | undefined) {
  return useQuery({
    queryKey: ["project", projectId, "readme"] as const,
    queryFn: () => api.get<Readme>(`/api/projects/${projectId}/readme`),
    enabled: projectId !== undefined,
    // A missing README is an answer, not a failure to retry.
    retry: (count, error) => !(error instanceof ApiError && error.status === 404) && count < 2,
  });
}

/** Whether a README query failed only because there isn't one. */
export const noReadme = (error: unknown) => error instanceof ApiError && error.status === 404;

/**
 * A project's README, rendered — on its own tab, and on the project's
 * dashboard. Says so plainly when there isn't one.
 */
export function ReadmeView({
  projectId,
  projectName,
}: {
  projectId: string;
  projectName?: string;
}) {
  const readme = useReadme(projectId);

  if (readme.isLoading) return <Loading label="Reading the README…" />;
  if (noReadme(readme.error)) {
    return (
      <Typography color="text.secondary">
        {projectName ?? "This project"} has no <code>README.md</code> at its root.
      </Typography>
    );
  }
  if (readme.error) return <ErrorNote error={readme.error} />;
  if (!readme.data) return null;

  const components: Components = {
    a: ({ href, children }) => {
      if (!href || href.startsWith("#")) return <a href={href}>{children}</a>;
      if (isAbsolute(href)) {
        return (
          <MuiLink href={href} target="_blank" rel="noreferrer noopener">
            {children}
          </MuiLink>
        );
      }
      return (
        <Box component="span" title={`${href} — a file in the repository`} sx={{ textDecoration: "underline dotted" }}>
          {children}
        </Box>
      );
    },
    // Width and height kept: a README sizes its logo with them.
    img: ({ src, alt, width, height, title }) => {
      if (!src) return null;
      const url = isAbsolute(src)
        ? src
        : streamUrl(`/api/projects/${projectId}/readme/asset`, { path: src.replace(/^\.\//, "") });
      return (
        <img src={url} alt={alt ?? ""} width={width} height={height} title={title} loading="lazy" />
      );
    },
  };

  return (
    <Box>
      <Typography variant="caption" color="text.secondary" sx={{ fontFamily: monoFontStack }}>
        {projectName}/{readme.data.file}
      </Typography>
      <Box
        sx={(theme) => ({
          mt: 1,
          lineHeight: 1.65,
          fontSize: 15,
          "& h1, & h2": {
            pb: 0.5,
            borderBottom: 1,
            borderColor: "divider",
            mt: 3,
            mb: 1.5,
          },
          "& h1": { ...theme.typography.h1, textTransform: "none" },
          "& h2": { ...theme.typography.h2, textTransform: "none" },
          "& h3": { ...theme.typography.h3, textTransform: "none", mt: 2.5, mb: 1 },
          "& h4, & h5, & h6": { ...theme.typography.h4, textTransform: "none", mt: 2, mb: 1 },
          "& > :first-child": { mt: 0 },
          "& p, & ul, & ol, & table, & pre, & blockquote": { my: 1.25 },
          "& li + li": { mt: 0.5 },
          // GitHub-style task lists: the checkbox is the bullet.
          "& li.task-list-item": { listStyle: "none" },
          "& li.task-list-item input": { mr: 1, ml: -2.5 },
          "& code": {
            fontFamily: monoFontStack,
            fontSize: "0.88em",
            px: 0.5,
            py: 0.125,
            borderRadius: 0.5,
            bgcolor: alpha(theme.palette.text.primary, 0.08),
          },
          "& pre": {
            p: 1.5,
            overflowX: "auto",
            borderRadius: 1,
            border: 1,
            borderColor: "divider",
            bgcolor: "background.default",
          },
          "& pre code": { p: 0, bgcolor: "transparent", fontSize: 13 },
          "& blockquote": {
            mx: 0,
            pl: 2,
            borderLeft: 3,
            borderColor: "divider",
            color: "text.secondary",
          },
          "& table": { borderCollapse: "collapse", display: "block", overflowX: "auto" },
          "& th, & td": { border: 1, borderColor: "divider", px: 1.25, py: 0.5, textAlign: "left" },
          "& th": { bgcolor: alpha(theme.palette.text.primary, 0.05) },
          "& img": { maxWidth: "100%" },
          "& hr": { border: 0, borderTop: 1, borderColor: "divider", my: 3 },
          "& a": { color: "primary.main" },
        })}
      >
        <ReactMarkdown
          remarkPlugins={[remarkGfm]}
          // Raw HTML parsed first, then sanitised: the order is the safety.
          rehypePlugins={[rehypeRaw, rehypeSanitize]}
          components={components}
        >
          {readme.data.text}
        </ReactMarkdown>
      </Box>
    </Box>
  );
}
