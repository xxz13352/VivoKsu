import {
  TRACE_OUTPUT_MAX_BYTES,
  sha256HexV2,
  type CredentialRedactionCountV2,
  type TraceCommandV2,
  type TraceRunV2,
  type TraceUploadRequestV2,
} from "./trace-v2-contract";

export interface RedactedTraceUploadV2 {
  payload: TraceUploadRequestV2;
  run_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
  event_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
  chunk_redactions: ReadonlyMap<string, CredentialRedactionCountV2[]>;
}

type RedactionCounts = Map<string, number>;
type TraceTextLocation = "run" | "event" | "chunk";

interface CredentialMatcher {
  replace(text: string, location: TraceTextLocation): { text: string; counts: RedactionCounts };
}

const CREDENTIAL_KEY = "(?:(?:[A-Za-z0-9]+[_-])*(?:password|passwd|pwd|token|api[_-]?key|secret|signature|sig|secret[_-]?access[_-]?key))";
const COMPLETE_PRIVATE_KEY = /-----BEGIN ([A-Z0-9 ]*PRIVATE KEY)-----[\s\S]*?-----END \1-----/g;
const INCOMPLETE_PRIVATE_KEY = /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*$/g;
const URL_TEXT = /\bhttps?:\/\/[^\s"'<>]+/gi;

export function redactTraceUploadV2(
  payload: TraceUploadRequestV2,
  exactSecrets: readonly string[],
): RedactedTraceUploadV2 {
  const matcher = buildCredentialMatcher(exactSecrets.filter((value) => value.length >= 6));
  return mapTraceTextFields(payload, matcher);
}

function buildCredentialMatcher(exactSecrets: readonly string[]): CredentialMatcher {
  const secrets = [...new Set(exactSecrets)].sort((left, right) => right.length - left.length);

  return {
    replace(text: string, _location: TraceTextLocation) {
      let output = text;
      const counts: RedactionCounts = new Map();

      output = replacePattern(output, COMPLETE_PRIVATE_KEY, "private-key", counts);
      output = replacePattern(output, INCOMPLETE_PRIVATE_KEY, "high-risk", counts);
      output = redactAuthorization(output, counts);
      output = redactCookies(output, counts);
      output = output.replace(URL_TEXT, (urlText) => redactUrl(urlText, counts));
      output = redactCliAssignments(output, counts);
      output = redactAssignments(output, counts);

      for (const secret of secrets) {
        output = output.replace(new RegExp(escapeRegExp(secret), "g"), () => {
          addCount(counts, "exact");
          return safeCredentialMarker(secret);
        });
      }

      if ((counts.get("high-risk") ?? 0) > 0 && utf8Length(output) > utf8Length(text)) {
        output = marker("high-risk");
      }

      return { text: output, counts };
    },
  };
}

function redactAuthorization(text: string, counts: RedactionCounts): string {
  let output = text.replace(
    /\b(Authorization\s*:\s*Bearer\s+)([^\s,;\r\n]+)/gi,
    (match, prefix: string, value: string) => replaceCapturedValue(match, prefix, value, "bearer", counts),
  );
  output = output.replace(
    /\b(Authorization\s*:\s*)([^\r\n]+)/gi,
    (match, prefix: string, value: string) => {
          if (isRedacted(value)) return match;
      const scheme = /^(Basic|Digest)\s+/i.exec(value)?.[0] ?? "";
      const credential = value.slice(scheme.length);
      addCount(counts, "authorization");
      return `${prefix}${scheme}${safeCredentialMarker(credential)}`;
    },
  );
  return output.replace(
    /\b(Bearer\s+)(?!(?:authentication|authorization)\b)([A-Za-z0-9._~+/=-]{6,})/gi,
    (match, prefix: string, value: string) => replaceCapturedValue(match, prefix, value, "bearer", counts),
  );
}

function redactCookies(text: string, counts: RedactionCounts): string {
  return text.replace(
    /\b((?:Set-)?Cookie\s*:\s*)([^\r\n]*)/gi,
    (match, prefix: string, value: string) => {
      if (value.length === 0 || isRedacted(value)) return match;
      if (/^Set-Cookie/i.test(prefix)) {
        const pair = /^([^=;\s]+)(\s*=\s*)([^;]*)([\s\S]*)$/.exec(value);
        if (pair !== null) {
          addCount(counts, "cookie");
          return `${prefix}${pair[1]}${pair[2]}${safeCredentialMarker(pair[3])}${pair[4]}`;
        }
        addCount(counts, "high-risk");
        return `${prefix}${marker("high-risk")}`;
      }

      let parsedPairs = 0;
      const redacted = value.replace(/(^|;\s*)([^=;\s]+)(\s*=\s*)([^;]*)/g, (_pair, delimiter: string, name: string, equals: string, cookieValue: string) => {
        parsedPairs += 1;
        if (cookieValue.length === 0) return `${delimiter}${name}${equals}`;
        addCount(counts, "cookie");
        return `${delimiter}${name}${equals}${safeCredentialMarker(cookieValue)}`;
      });
      if (parsedPairs > 0) return `${prefix}${redacted}`;
      addCount(counts, "high-risk");
      return `${prefix}${marker("high-risk")}`;
    },
  );
}

function redactCliAssignments(text: string, counts: RedactionCounts): string {
  const pattern = new RegExp(
    `(\\-\\-${CREDENTIAL_KEY})(\\s*=\\s*|\\s+)(?:"([^"]*)"|'([^']*)'|([^\\s,;&]+))`,
    "gi",
  );
  return text.replace(pattern, (match, flag: string, separator: string, doubleQuoted?: string, singleQuoted?: string, bare?: string) => {
    const value = doubleQuoted ?? singleQuoted ?? bare ?? "";
    if (value.length === 0 || isRedacted(value)) return match;
    const kind = credentialKind(flag);
    addCount(counts, kind);
    return `${flag}${separator}${quotedMarker(value, doubleQuoted, singleQuoted)}`;
  });
}

function redactAssignments(text: string, counts: RedactionCounts): string {
  const pattern = new RegExp(
    `\\b(${CREDENTIAL_KEY})(\\s*[:=]\\s*)(?:"([^"]*)"|'([^']*)'|([^\\s,;&]+))`,
    "gi",
  );
  return text.replace(pattern, (match, key: string, separator: string, doubleQuoted?: string, singleQuoted?: string, bare?: string) => {
    const value = doubleQuoted ?? singleQuoted ?? bare ?? "";
    if (value.length === 0 || isRedacted(value)) return match;
    const kind = credentialKind(key);
    addCount(counts, kind);
    return `${key}${separator}${quotedMarker(value, doubleQuoted, singleQuoted)}`;
  });
}

function mapTraceTextFields(payload: TraceUploadRequestV2, matcher: CredentialMatcher): RedactedTraceUploadV2 {
  const runRedactions = new Map<string, CredentialRedactionCountV2[]>();
  const eventRedactions = new Map<string, CredentialRedactionCountV2[]>();
  const chunkRedactions = new Map<string, CredentialRedactionCountV2[]>();

  const runs = payload.runs.map((run) => {
    const counts: RedactionCounts = new Map();
    const redacted = mapRun(run, (text) => replaceAndAccumulate(matcher, text, "run", counts));
    const summarized = summarizeCounts(counts);
    if (summarized.length > 0) runRedactions.set(run.run_id, summarized);
    return redacted;
  });

  const events = payload.events.map((event) => {
    const counts: RedactionCounts = new Map();
    const redact = (text: string) => replaceAndAccumulate(matcher, text, "event", counts);
    const command = event.command === null ? null : mapCommand(event.command, redact, counts);
    const clientCounts = event.credential_redactions.map((redaction) => ({ ...redaction, kind: redact(redaction.kind) }));
    const redacted = {
      ...event,
      step_name: redact(event.step_name),
      partition_name: mapNullable(event.partition_name, redact),
      command,
      verification: mapNullable(event.verification, redact),
      device_state: mapNullable(event.device_state, redact),
      remedies: event.remedies.map(redact),
      error_class: mapNullable(event.error_class, redact),
      error_code: mapNullable(event.error_code, redact),
      error_message: mapNullable(event.error_message, redact),
      credential_redactions: [] as CredentialRedactionCountV2[],
    };
    const mergedCounts = mergeSummaries(clientCounts, summarizeCounts(counts));
    redacted.credential_redactions = mergedCounts.slice(0, 100);
    if (mergedCounts.length > 0) eventRedactions.set(event.event_id, mergedCounts);
    return redacted;
  });

  const output_chunks = payload.output_chunks.map((chunk) => ({ ...chunk }));
  const streamGroups = new Map<string, number[]>();
  payload.output_chunks.forEach((chunk, index) => {
    const key = `${chunk.event_id}\u0000${chunk.stream}`;
    const indexes = streamGroups.get(key) ?? [];
    indexes.push(index);
    streamGroups.set(key, indexes);
  });
  for (const indexes of streamGroups.values()) {
    indexes.sort((left, right) => payload.output_chunks[left].chunk_index - payload.output_chunks[right].chunk_index);
    const originalChunks = indexes.map((index) => payload.output_chunks[index]);
    const originalText = originalChunks.map((chunk) => chunk.text).join("");
    const result = matcher.replace(originalText, "chunk");
    if (result.text === originalText) continue;

    let redactedText = result.text;
    const counts = new Map(result.counts);
    if (utf8Length(redactedText) > originalChunks.length * TRACE_OUTPUT_MAX_BYTES) {
      redactedText = marker("high-risk");
      addCount(counts, "high-risk");
    }
    const parts = repartitionText(redactedText, originalChunks.map((chunk) => chunk.byte_count));
    indexes.forEach((payloadIndex, partIndex) => {
      const bytes = new TextEncoder().encode(parts[partIndex]);
      output_chunks[payloadIndex] = {
        ...output_chunks[payloadIndex],
        text: parts[partIndex],
        byte_count: bytes.byteLength,
        sha256: sha256HexV2(bytes),
      };
    });
    const remainingCounts = new Map(counts);
    originalChunks.forEach((chunk) => {
      const localCounts = matcher.replace(chunk.text, "chunk").counts;
      const attributed: RedactionCounts = new Map();
      for (const [kind, localCount] of localCounts) {
        const available = remainingCounts.get(kind) ?? 0;
        const count = Math.min(localCount, available);
        if (count === 0) continue;
        addCount(attributed, kind, count);
        remainingCounts.set(kind, available - count);
      }
      appendChunkCounts(chunkRedactions, chunk.chunk_id, attributed);
    });
    appendChunkCounts(chunkRedactions, originalChunks[0].chunk_id, remainingCounts);
  }

  return {
    payload: { ...payload, runs, events, output_chunks },
    run_redactions: runRedactions,
    event_redactions: eventRedactions,
    chunk_redactions: chunkRedactions,
  };
}

function mapRun(run: TraceRunV2, redact: (text: string) => string): TraceRunV2 {
  return {
    ...run,
    operation_kind: redact(run.operation_kind),
    title: redact(run.title),
    device_serial: mapNullable(run.device_serial, redact),
    source_paths: run.source_paths.map(redact),
    source_urls: run.source_urls.map(redact),
    client_version: redact(run.client_version),
    error_class: mapNullable(run.error_class, redact),
    error_code: mapNullable(run.error_code, redact),
    error_message: mapNullable(run.error_message, redact),
    trace_loss_reason: mapNullable(run.trace_loss_reason, redact),
  };
}

function mapCommand(command: TraceCommandV2, redact: (text: string) => string, counts: RedactionCounts): TraceCommandV2 {
  let pendingSecretKind: string | null = null;
  const argv = command.argv.map((argument) => {
    if (pendingSecretKind !== null) {
      const matched = redact(argument);
      if (matched !== argument) {
        pendingSecretKind = null;
        return matched;
      }
      if (isCredentialLikeDashValue(argument)) {
        const kind = pendingSecretKind;
        pendingSecretKind = null;
        addCount(counts, kind);
        return safeCredentialMarker(argument);
      }
      if (argument.startsWith("-")) {
        pendingSecretKind = cliFlagKind(argument);
        return argument;
      }
      const kind = pendingSecretKind;
      pendingSecretKind = null;
      addCount(counts, kind);
      return safeCredentialMarker(argument);
    }
    const redacted = redact(argument);
    pendingSecretKind = cliFlagKind(argument);
    return redacted;
  });
  return {
    program: redact(command.program),
    argv,
    display_command: redact(command.display_command),
    working_directory: mapNullable(command.working_directory, redact),
    paths: command.paths.map(redact),
    urls: command.urls.map(redact),
    serial: mapNullable(command.serial, redact),
  };
}

function redactUrl(urlText: string, counts: RedactionCounts): string {
  let output = urlText.replace(/^(https?:\/\/)([^/@]+)@/i, (_match, prefix: string, userinfo: string) => {
    addCount(counts, "url-userinfo");
    return `${prefix}${safeCredentialMarker(userinfo)}@`;
  });
  const pattern = new RegExp(`([?&])(${CREDENTIAL_KEY})(=)([^&#]*)`, "gi");
  output = output.replace(pattern, (match, delimiter: string, key: string, equals: string, value: string) => {
    if (value.length === 0 || isRedacted(value)) return match;
    const kind = credentialKind(key);
    addCount(counts, kind);
    return `${delimiter}${key}${equals}${safeCredentialMarker(value)}`;
  });
  return output;
}

function replacePattern(text: string, pattern: RegExp, kind: string, counts: RedactionCounts): string {
  return text.replace(pattern, () => {
    addCount(counts, kind);
    return marker(kind);
  });
}

function replaceCapturedValue(match: string, prefix: string, value: string, kind: string, counts: RedactionCounts): string {
  if (value.length === 0 || isRedacted(value)) return match;
  addCount(counts, kind);
  return `${prefix}${safeCredentialMarker(value)}`;
}

function replaceAndAccumulate(matcher: CredentialMatcher, text: string, location: TraceTextLocation, target: RedactionCounts): string {
  const result = matcher.replace(text, location);
  for (const [kind, count] of result.counts) addCount(target, kind, count);
  return result.text;
}

function mergeSummaries(left: readonly CredentialRedactionCountV2[], right: readonly CredentialRedactionCountV2[]): CredentialRedactionCountV2[] {
  const counts: RedactionCounts = new Map();
  for (const item of [...left, ...right]) addCount(counts, item.kind, item.count);
  return summarizeCounts(counts);
}

function summarizeCounts(counts: ReadonlyMap<string, number>): CredentialRedactionCountV2[] {
  return [...counts].map(([kind, count]) => ({ kind, count }));
}

function addCount(counts: RedactionCounts, kind: string, count = 1): void {
  counts.set(kind, Math.min(Number.MAX_SAFE_INTEGER, (counts.get(kind) ?? 0) + count));
}

function appendChunkCounts(
  target: Map<string, CredentialRedactionCountV2[]>,
  chunkId: string,
  counts: ReadonlyMap<string, number>,
): void {
  const summarized = summarizeCounts(counts).filter((item) => item.count > 0);
  if (summarized.length === 0) return;
  target.set(chunkId, mergeSummaries(target.get(chunkId) ?? [], summarized));
}

function marker(kind: string): string {
  return `[CREDENTIAL_REMOVED:${kind.replace(/-/g, "_").toUpperCase()}]`;
}

function quotedMarker(value: string, doubleQuoted: string | undefined, singleQuoted: string | undefined): string {
  const replacement = safeCredentialMarker(value);
  if (doubleQuoted !== undefined) return `"${replacement}"`;
  if (singleQuoted !== undefined) return `'${replacement}'`;
  return replacement;
}

function credentialKind(key: string): string {
  const normalized = key.replace(/^--/, "").toLowerCase().replace(/_/g, "-");
  if (normalized.endsWith("password") || normalized.endsWith("passwd") || normalized.endsWith("pwd")) return "password";
  if (normalized.endsWith("token")) return "token";
  if (normalized.endsWith("apikey") || normalized.endsWith("api-key")) return "api-key";
  if (normalized.endsWith("secret-access-key") || normalized.endsWith("secret")) return "secret";
  if (normalized.endsWith("signature") || normalized.endsWith("sig")) return "signature";
  return normalized;
}

function cliFlagKind(value: string): string | null {
  const match = new RegExp(`^--(${CREDENTIAL_KEY})$`, "i").exec(value);
  return match ? credentialKind(match[1]) : null;
}

function isCredentialLikeDashValue(value: string): boolean {
  if (!value.startsWith("-")) return false;
  const normalized = value.replace(/^-+/, "").toLowerCase();
  return /(?:^|[-_])(?:password|passwd|pwd|token|api[-_]?key|secret|signature|sig)(?:[-_]|$)/.test(normalized);
}

function mapNullable(value: string | null, redact: (text: string) => string): string | null {
  return value === null ? null : redact(value);
}

function safeCredentialMarker(originalValue: string): string {
  const replacement = "[REDACTED]";
  const originalBytes = utf8Length(originalValue);
  return originalBytes >= replacement.length ? replacement : "*".repeat(Math.max(1, originalBytes));
}

function isRedacted(value: string): boolean {
  return value.includes("[CREDENTIAL_REMOVED:") || value.includes("[REDACTED]") || /^\*+$/.test(value);
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function repartitionText(text: string, preferredByteCounts: readonly number[]): string[] {
  const parts: string[] = [];
  let remaining = text;
  for (let index = 0; index < preferredByteCounts.length - 1; index += 1) {
    const remainingSlots = preferredByteCounts.length - index - 1;
    const minimumHere = Math.max(0, utf8Length(remaining) - remainingSlots * TRACE_OUTPUT_MAX_BYTES);
    const limit = Math.min(TRACE_OUTPUT_MAX_BYTES, Math.max(preferredByteCounts[index], minimumHere + 3));
    const [part, rest] = takeUtf8Prefix(remaining, limit);
    parts.push(part);
    remaining = rest;
  }
  parts.push(remaining);
  return parts;
}

function takeUtf8Prefix(value: string, maximumBytes: number): [string, string] {
  let bytes = 0;
  let codeUnits = 0;
  for (const character of value) {
    const characterBytes = utf8Length(character);
    if (bytes + characterBytes > maximumBytes) break;
    bytes += characterBytes;
    codeUnits += character.length;
  }
  return [value.slice(0, codeUnits), value.slice(codeUnits)];
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
