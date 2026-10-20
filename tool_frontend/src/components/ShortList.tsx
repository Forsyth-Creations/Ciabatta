/**
 * A list of paths or globs, cut to the first few.
 *
 * A Rust target's inputs are every crate its `cache.cargo` closure reaches, and
 * its undeclared reads are every file rustc opened — written out in full, one
 * line of the inspector became a paragraph nobody could read. Text is the wrong
 * way to read hundreds of paths anyway: the file tree under the step
 * ([`CacheKeyFilesView`]) shows them by directory, with sizes and hashes. So
 * text shows five, and "see more" takes you to the tree — or, where there is no
 * tree on the page, opens the rest in place.
 */

import { useState } from "react";
import { Box, Link } from "@mui/material";

import { monoFontStack } from "../theme";

/** How many items are written out before the rest go behind "see more". */
export const SHORT_LIST_MAX = 5;

/** The id the inspector's file tree carries, for "see more" to scroll to. */
export const CACHE_FILES_ANCHOR = "cache-key-files";

/**
 * Scroll the file tree into view and flash it, so the eye lands where the
 * click sent it. False when the page has no tree to go to.
 */
export function revealCacheFiles(): boolean {
  const tree = document.getElementById(CACHE_FILES_ANCHOR);
  if (!tree) return false;
  tree.scrollIntoView({ behavior: "smooth", block: "start" });
  tree.animate?.(
    [{ boxShadow: "0 0 0 2px rgba(255, 167, 38, 0.9)" }, { boxShadow: "0 0 0 2px transparent" }],
    { duration: 1400, easing: "ease-out" },
  );
  return true;
}

/** "… and 12 more — see files": to the tree when there is one, else open in place. */
export function SeeMore({ hidden, onExpand }: { hidden: number; onExpand: () => void }) {
  return (
    <Link
      component="button"
      type="button"
      variant="caption"
      onClick={(event) => {
        // Inside a node's popover or a clickable row, a click here is this
        // link's alone.
        event.stopPropagation();
        if (!revealCacheFiles()) onExpand();
      }}
      sx={{ verticalAlign: "baseline", fontFamily: "inherit" }}
    >
      … and {hidden} more — see files
    </Link>
  );
}

/** `items` joined with commas, at most [`SHORT_LIST_MAX`] of them. */
export function ShortList({ items, mono = false }: { items: string[]; mono?: boolean }) {
  const [open, setOpen] = useState(false);
  const shown = open ? items : items.slice(0, SHORT_LIST_MAX);
  const hidden = items.length - shown.length;
  return (
    <Box component="span" sx={{ fontFamily: mono ? monoFontStack : undefined }}>
      {shown.join(", ")}
      {hidden > 0 && (
        <>
          {" "}
          <SeeMore hidden={hidden} onExpand={() => setOpen(true)} />
        </>
      )}
    </Box>
  );
}
