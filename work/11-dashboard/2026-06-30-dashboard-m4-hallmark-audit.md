# Hallmark audit — Update-Server dashboard SPA (M4)

Anti-AI-slop design audit of `apps/dashboard/web` (Vue 3 + shadcn-vue). Graded as a **modern-minimal /
utilitarian ops dashboard** (restraint is correct — no marketing hero). Bar: does it read as a *deliberately
designed instrument* or a *stock shadcn scaffold*?

## Findings (as audited, pre-fix)

### 🔴 Critical
1. **System-font default — no type pairing** (`main.css` body stack). The #1 generated-look tell.
2. **Structural sameness across all four pages** — every page is `<h2>+RefreshBar` → DegradedPanel → card/stack;
   no per-page structural identity.

### 🟠 Major
3. **Stock shadcn slate + indigo palette** — no brand presence (`main.css` `--primary` indigo).
4. **N1a nav + lucide icon-as-logo** — the canonical AI dashboard shell (`App.vue`).
5. **Flat type hierarchy** — brand / titles / heads all ~`font-semibold`.

### 🟡 Minor
6. Glassy `backdrop-blur` sticky header. 7. Uniform 0.6rem radius + `border-l-4` card voice.
8. Nav may crowd at 320px (no mobile treatment).

**Count: 2 critical · 3 major · 3 minor.**

## Resolution (focused pass, user direction = "keep neutral")

| # | Decision |
|---|---|
| 1 | **Fixed** — Space Grotesk (display/body) + JetBrains Mono (data) via `@fontsource`, fully offline (woff2 bundled into `web/dist/assets`, embedded in the binary, served by M3's AssetSource — no network on CT 111). |
| 2 | **Deferred** by choice — per-page structural redesign is a follow-up `hallmark redesign` (do it once live). |
| 3 | **Fixed (neutral)** — `--primary` → near-foreground ink (chroma ~0); the `--rag-*` status colours are now the ONLY chroma on the page. |
| 4 | **Fixed** — typographic "Juridia / Update Server" wordmark (from the single `DASHBOARD_NAME` contract); lucide icon-logo dropped. |
| 5 | **Fixed** — display title tier + `tabular-nums` on all counts/sequences. |
| 6 | **Fixed** — solid `bg-background` + hairline `border-b`; underline-tab nav (ink active-state) instead of the pill. |
| 7 | **Deferred** with #2 (taste; uniform radius). |
| 8 | **Fixed** — flex-wrap two-row masthead + `overflow-x-auto` nav; no horizontal scroll at 320–375px. |

Post-pass: **0 critical · 0 major · 1 minor** (radius uniformity, deferred with the structural work). The R/A/G
mapping stays single-sourced in `lib/severity.ts`; no per-component colour hard-coding. Reads as an instrument.
