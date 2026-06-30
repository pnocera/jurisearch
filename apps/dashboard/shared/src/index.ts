/**
 * shared/ — the DRY contract imported by BOTH server and web: producer-JSON DTOs + runtime
 * validators, the snake→camel mapping/derivation helpers, and the outcome-first exit-class logic.
 * One source of truth for the wire and the UI (design §4/§9).
 */
export const DASHBOARD_NAME = "Juridia — Update Server";
export type DashboardName = typeof DASHBOARD_NAME;

export * from "./dto.ts";
export * from "./exit-class.ts";
export * from "./mapping.ts";
export * from "./validate.ts";
