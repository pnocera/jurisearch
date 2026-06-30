/**
 * web/ — the ONE mapping from the shared `Severity` (outcome-first, derived in `shared/severityOf`)
 * to the dashboard's R/A/G presentation (design §6.3: R/A/G is NEVER hard-coded per component). Every
 * badge, card border and dot resolves its colour through this module, so the palette and the
 * severity→colour policy live in exactly one place. Colours reference the `--rag-*` Tailwind tokens
 * (`assets/main.css`), so light/dark theming is automatic.
 */

import type { OverviewFreshnessDTO, Severity } from "@jurisearch-dashboard/shared";

/** The three-light status plus a neutral/in-progress state. */
export type Rag = "green" | "amber" | "red" | "neutral";

/** Map a backend `Severity` to an R/A/G light. Transient failures are amber; hard failures red. */
export function ragOf(severity: Severity): Rag {
  switch (severity) {
    case "ok":
      return "green";
    case "neutral":
      return "neutral";
    case "transient":
      return "amber";
    default:
      // data / unprovisioned / config / permanent — all hard failures.
      return "red";
  }
}

/**
 * Map a syslog priority (0..7) to an R/A/G light — the ONE place log severity becomes colour, so
 * `LogViewer` never names a `rag-*` class itself. 0..3 (emerg/alert/crit/err) → red; 4 (warning) →
 * amber; the rest (notice/info/debug) and unknown → neutral.
 */
export function priorityRag(priority: number | null): Rag {
  if (priority === null) {
    return "neutral";
  }
  if (priority <= 3) {
    return "red";
  }
  if (priority === 4) {
    return "amber";
  }
  return "neutral";
}

/** A short human label for a backend severity (tooltip/aria text). */
export function severityLabel(severity: Severity): string {
  switch (severity) {
    case "ok":
      return "Healthy";
    case "neutral":
      return "In progress";
    case "transient":
      return "Transient failure";
    case "data":
      return "Data error";
    case "unprovisioned":
      return "Unprovisioned";
    case "config":
      return "Misconfigured";
    case "permanent":
      return "Failed";
    default:
      return severity;
  }
}

/** The presentational classes for an R/A/G light — referenced by every status surface. */
export interface RagPresentation {
  label: string;
  /** Badge pill classes (background tint + text + border). */
  badgeClass: string;
  /** A small status dot's fill. */
  dotClass: string;
  /** A card's left accent / border emphasis. */
  accentClass: string;
  /** Plain coloured text (e.g. an error line). */
  textClass: string;
}

export const RAG_PRESENTATION: Readonly<Record<Rag, RagPresentation>> = {
  green: {
    label: "Healthy",
    badgeClass: "bg-rag-green/15 text-rag-green border-rag-green/30",
    dotClass: "bg-rag-green",
    accentClass: "border-l-rag-green",
    textClass: "text-rag-green",
  },
  amber: {
    label: "Attention",
    badgeClass: "bg-rag-amber/15 text-rag-amber border-rag-amber/30",
    dotClass: "bg-rag-amber",
    accentClass: "border-l-rag-amber",
    textClass: "text-rag-amber",
  },
  red: {
    label: "Failed",
    badgeClass: "bg-rag-red/15 text-rag-red border-rag-red/30",
    dotClass: "bg-rag-red",
    accentClass: "border-l-rag-red",
    textClass: "text-rag-red",
  },
  neutral: {
    label: "In progress",
    badgeClass: "bg-rag-neutral/15 text-rag-neutral border-rag-neutral/30",
    dotClass: "bg-rag-neutral",
    accentClass: "border-l-rag-neutral",
    textClass: "text-rag-neutral",
  },
};

/** Presentation for a backend severity in one hop (the common case for a badge). */
export function presentationOf(severity: Severity): RagPresentation {
  return RAG_PRESENTATION[ragOf(severity)];
}

/** The corpus-level `overall` health (`StatusDTO.overall`) → R/A/G — the one mapping for the header. */
export function overallRag(overall: "current" | "stale" | "broken"): Rag {
  switch (overall) {
    case "current":
      return "green";
    case "stale":
      return "amber";
    default:
      return "red";
  }
}

/**
 * A GROUP's overall R/A/G = its run severity OVERLAID with freshness (design §6.3 "severityOf +
 * freshness"). A healthy run that is nonetheless stale-by-age or has a rebaseline pending is
 * demoted green→amber so the card flags the lag; a failing/in-progress run keeps its own light.
 */
export function groupRag(severity: Severity, freshness: OverviewFreshnessDTO): Rag {
  const base = ragOf(severity);
  if (base === "green" && (freshness.staleByAge || freshness.rebaselinePending)) {
    return "amber";
  }
  return base;
}
