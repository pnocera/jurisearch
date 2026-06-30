/**
 * shared/ — the DRY contract imported by BOTH server and web.
 *
 * M0 carries only the brand constant; the real DTOs + validators (StatusDTO, RunRecordDTO,
 * PackageDTO, the ExitClass table, etc.) land in M1.
 */
export const DASHBOARD_NAME = "Juridia — Update Server";
export type DashboardName = typeof DASHBOARD_NAME;
