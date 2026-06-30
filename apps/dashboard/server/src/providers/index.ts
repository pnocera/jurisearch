/** Providers — one `DataProvider<T>` per on-box source (design §5.2). */
export {
  LogsProvider,
  type LogsQuery,
  logsCommand,
  parseJournalNdjson,
  serviceUnit,
} from "./logs.ts";
export { manifestPath, PackagesProvider, parseManifestText } from "./packages.ts";
export {
  lastRunByGroup,
  parseRunRecordText,
  RunsProvider,
  type RunsQuery,
  sortByStartedAtDesc,
} from "./runs.ts";
export { parseStatusStdout, StatusProvider, statusCommand } from "./status.ts";
export { parseTimersStdout, TimersProvider, timersCommand } from "./timers.ts";
export { type DataProvider, ProviderError } from "./types.ts";
