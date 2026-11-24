import type { EngineErrorCode, PaletteId, PresetId } from "./generated/schema.js";

/** Errors raised by this package itself rather than the engine. */
export type ClientErrorCode =
  /** The render was cancelled through its AbortSignal. */
  | "ABORTED"
  /** The input could not be read (unsupported type, failed fetch, bad dimensions). */
  | "INVALID_INPUT"
  /** The requested output needs an API this runtime lacks (for example ImageBitmap in Node). */
  | "UNSUPPORTED"
  /** A worker failed to start or crashed. */
  | "WORKER_ERROR";

/** Every error code: the engine's (identical to the CLI's --json codes) plus the client ones. */
export type DithErrorCode = EngineErrorCode | ClientErrorCode;

/** The type of validValues for codes that carry them. */
interface ValidValuesByCode {
  UNKNOWN_PRESET: PresetId[];
  UNKNOWN_PALETTE: PaletteId[];
}

/** The validValues of an error with code C (undefined for codes without a list). */
export type ValidValuesOf<C extends DithErrorCode> = C extends keyof ValidValuesByCode
  ? ValidValuesByCode[C]
  : undefined;

/** A dith error. Narrow {@link AnyDithError} on code to get the exact shape. */
export class DithError<C extends DithErrorCode = DithErrorCode> extends Error {
  override readonly name = "DithError";
  /** Machine-readable code, matching the CLI's --json error codes. */
  readonly code: C;
  /** Recovery hint. */
  readonly suggestion: string;
  /** Valid values when the error concerns an unknown preset or palette. */
  readonly validValues: ValidValuesOf<C>;

  constructor(code: C, message: string, suggestion = "", validValues?: ValidValuesOf<C>) {
    super(message);
    this.code = code;
    this.suggestion = suggestion;
    this.validValues = validValues as ValidValuesOf<C>;
  }
}

/** Discriminated union of every DithError; switch on code to narrow. */
export type AnyDithError = { [C in DithErrorCode]: DithError<C> }[DithErrorCode];

/** Type guard for errors thrown by this package. */
export function isDithError(error: unknown): error is AnyDithError {
  return error instanceof DithError;
}

/** The plain shape errors cross worker boundaries in. */
export interface ErrorPayload {
  code: string;
  message: string;
  suggestion?: string;
  validValues?: string[];
}

export function toPayload(error: unknown): ErrorPayload {
  if (error instanceof DithError) {
    return { code: error.code, message: error.message, suggestion: error.suggestion, validValues: error.validValues };
  }
  const message = error instanceof Error ? error.message : String(error);
  try {
    const envelope = JSON.parse(message) as {
      code?: string;
      message?: string;
      suggestion?: string;
      valid_values?: string[];
    };
    if (typeof envelope.code === "string") {
      return {
        code: envelope.code,
        message: envelope.message ?? message,
        suggestion: envelope.suggestion,
        validValues: envelope.valid_values,
      };
    }
  } catch {
    // Not an engine envelope.
  }
  return { code: "WORKER_ERROR", message };
}

export function fromPayload(payload: ErrorPayload): AnyDithError {
  return new DithError(
    payload.code as DithErrorCode,
    payload.message,
    payload.suggestion,
    payload.validValues as never,
  ) as AnyDithError;
}

export function abortError(): DithError<"ABORTED"> {
  return new DithError("ABORTED", "the render was aborted", "Ignore it: a newer render replaced this one.");
}
