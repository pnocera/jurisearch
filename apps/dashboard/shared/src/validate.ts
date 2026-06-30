/**
 * shared/ — a tiny, dependency-light, Zod-style runtime validator toolkit (design §9: prefer
 * dependency-light validators so the `bun build --compile` bundle stays lean and the contract is
 * the single source of truth for BOTH server and web).
 *
 * The combinators below let each DTO declare its fields ONCE; the snake_case source key is derived
 * from the camelCase output key via `camelToSnake` (one mapping mechanism), with an explicit
 * `from(src, …)` escape hatch for the few producer fields whose name is a rename rather than a
 * case change. Validators read ONLY declared fields, so unknown/extra producer fields are ignored
 * (forward-compatible, per design §4) instead of rejected.
 */

import { camelToSnake } from "./mapping.ts";

/** A localized validation failure carrying the JSON path so a degraded panel can be diagnosed. */
export class ValidationError extends Error {
  constructor(
    public readonly path: string,
    public readonly detail: string,
  ) {
    super(`${path}: ${detail}`);
    this.name = "ValidationError";
  }
}

/** Result of a non-throwing parse; a failure degrades one panel rather than crashing the server. */
export type Result<T> = { ok: true; value: T } | { ok: false; error: string };

/** A reader validates+maps a single already-extracted value at `path` (throws on mismatch). */
export type Reader<T> = (value: unknown, path: string) => T;

/** The output type a reader produces. */
export type Parsed<R> = R extends Reader<infer T> ? T : never;

function typeName(value: unknown): string {
  if (value === null) return "null";
  if (Array.isArray(value)) return "array";
  return typeof value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Assert a value is a plain object and return it as a record (throws otherwise). */
export function asRecord(value: unknown, path: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new ValidationError(path, `expected object, got ${typeName(value)}`);
  }
  return value;
}

export const str: Reader<string> = (value, path) => {
  if (typeof value !== "string") {
    throw new ValidationError(path, `expected string, got ${typeName(value)}`);
  }
  return value;
};

export const num: Reader<number> = (value, path) => {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new ValidationError(path, `expected finite number, got ${typeName(value)}`);
  }
  return value;
};

export const bool: Reader<boolean> = (value, path) => {
  if (typeof value !== "boolean") {
    throw new ValidationError(path, `expected boolean, got ${typeName(value)}`);
  }
  return value;
};

/** A passthrough reader for fields we carry verbatim (forward-compat, e.g. a package signature). */
export const unknownValue: Reader<unknown> = (value) => value;

/** Make a reader nullable: `null`/`undefined`/absent → `null`, else delegate. */
export function optional<T>(reader: Reader<T>): Reader<T | null> {
  return (value, path) => (value === null || value === undefined ? null : reader(value, path));
}

/** Provide a default when a field is `null`/`undefined`/absent (e.g. an omitted `row_counts`). */
export function withDefault<T>(reader: Reader<T>, fallback: T): Reader<T> {
  return (value, path) => (value === null || value === undefined ? fallback : reader(value, path));
}

export const optStr = optional(str);
export const optNum = optional(num);

/** A reader for a homogeneous array. */
export function arrayOf<T>(item: Reader<T>): Reader<T[]> {
  return (value, path) => {
    if (!Array.isArray(value)) {
      throw new ValidationError(path, `expected array, got ${typeName(value)}`);
    }
    return value.map((element, index) => item(element, `${path}[${index}]`));
  };
}

/** A reader for a string-keyed map whose KEYS are preserved verbatim (e.g. `row_counts`). */
export function mapOf<T>(valueReader: Reader<T>): Reader<Record<string, T>> {
  return (value, path) => {
    const record = asRecord(value, path);
    const out: Record<string, T> = {};
    for (const key of Object.keys(record)) {
      out[key] = valueReader(record[key], `${path}.${key}`);
    }
    return out;
  };
}

/** A reader constraining a string to a closed vocabulary (e.g. an enum). */
export function enumOf<const T extends readonly string[]>(...values: T): Reader<T[number]> {
  return (value, path) => {
    const s = str(value, path);
    if (!values.includes(s as T[number])) {
      throw new ValidationError(path, `expected one of ${values.join("|")}, got "${s}"`);
    }
    return s as T[number];
  };
}

/** A field whose producer source key is a RENAME (not a case change) of the DTO key. */
export type FieldSpec<T> = { src: string; read: Reader<T> };

/** A schema entry: a bare reader (source key = `camelToSnake(camelKey)`) or an explicit rename. */
export type Spec<T> = Reader<T> | FieldSpec<T>;

/** Declare an explicit source key for a field (e.g. `manifestGeneratedAt` ← `generated_at`). */
export function from<T>(src: string, read: Reader<T>): FieldSpec<T> {
  return { src, read };
}

export type Schema = Record<string, Spec<unknown>>;

type SpecType<S> = S extends Reader<infer T> ? T : S extends FieldSpec<infer T> ? T : never;

/** The DTO type inferred from a schema — fields are declared exactly once, here. */
export type Infer<S extends Schema> = { [K in keyof S]: SpecType<S[K]> };

/**
 * Build a reader for an object DTO from a camelCase schema. Each field is read from its
 * snake_case source key (`camelToSnake(camelKey)`, or the explicit `from()` override). Unknown
 * producer fields are ignored (forward-compatible).
 */
export function object<S extends Schema>(schema: S): Reader<Infer<S>> {
  const entries = Object.keys(schema).map((camelKey) => {
    const spec = schema[camelKey] as Spec<unknown>;
    const isReader = typeof spec === "function";
    return {
      camelKey,
      srcKey: isReader ? camelToSnake(camelKey) : spec.src,
      read: isReader ? spec : spec.read,
    };
  });
  return (value, path) => {
    const record = asRecord(value, path);
    const out: Record<string, unknown> = {};
    for (const { camelKey, srcKey, read } of entries) {
      out[camelKey] = read(record[srcKey], `${path}.${srcKey}`);
    }
    return out as Infer<S>;
  };
}

/** Run a reader without throwing: a `ValidationError` becomes `{ ok: false }`. */
export function safeParse<T>(reader: Reader<T>, input: unknown): Result<T> {
  try {
    return { ok: true, value: reader(input, "$") };
  } catch (error) {
    if (error instanceof ValidationError) {
      return { ok: false, error: error.message };
    }
    throw error;
  }
}
