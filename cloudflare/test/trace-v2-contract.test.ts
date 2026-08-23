import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import {
  decodeTraceCursorV2,
  encodeTraceCursorV2,
  readTraceUploadV2,
  validateTraceUploadV2,
} from "../src/trace-v2-contract";

const valid = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/upload.success.json", import.meta.url),
  "utf8",
));
const failed = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/upload.failed.json", import.meta.url),
  "utf8",
));
const adminFailed = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/admin-run-failed.json", import.meta.url),
  "utf8",
));
const schema = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/usage-trace-v2.schema.json", import.meta.url),
  "utf8",
));

function copy(): Record<string, any> {
  return JSON.parse(JSON.stringify(valid));
}

describe("trace v2 contract", () => {
  it("accepts the canonical success fixture", () => {
    expect(validateTraceUploadV2(valid)).toEqual(valid);
  });

  it("freezes failure traces with persisted skipped remainder instead of evidence loss", () => {
    expect(validateTraceUploadV2(failed)).toEqual(failed);
    expect(failed.runs[0]).toMatchObject({ trace_complete: true, trace_loss_reason: null, final_sequence: 3 });
    expect(failed.events.map((event: { status: string }) => event.status)).toEqual(["failed", "skipped", "failed"]);
    expect(adminFailed.run).toMatchObject({ trace_complete: true, trace_loss_reason: null });
    expect(adminFailed.events.map((event: { status: string }) => event.status)).toEqual(["failed", "skipped", "failed"]);
  });

  it("reads a canonical upload body through the bounded request reader", async () => {
    const request = new Request("https://api.nwflash.cc.cd/api/usage/traces/v2", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(valid),
    });
    await expect(readTraceUploadV2(request)).resolves.toEqual(valid);
  });

  it("rejects unknown fields", () => {
    expect(() => validateTraceUploadV2({ ...valid, token: "must-not-exist" }))
      .toThrow(/unknown field: token/);
  });

  it("round-trips the opaque keyset cursor", () => {
    const value = { v: 1 as const, started_at_ms: 1_787_500_000_123, run_id: valid.runs[0].run_id };
    expect(decodeTraceCursorV2(encodeTraceCursorV2(value))).toEqual(value);
  });

  it("rejects non-lowercase UUIDv7 IDs and closed-enum escapes", () => {
    const uppercase = copy();
    uppercase.runs[0].run_id = uppercase.runs[0].run_id.toUpperCase();
    expect(() => validateTraceUploadV2(uppercase)).toThrow(/UUIDv7/);

    const invalidEnum = copy();
    invalidEnum.events[0].status = "completed";
    expect(() => validateTraceUploadV2(invalidEnum)).toThrow(/events\[0\]\.status/);
  });

  it("rejects unsafe, negative, and inconsistent numeric values", () => {
    const unsafe = copy();
    unsafe.runs[0].started_at_ms = Number.MAX_SAFE_INTEGER + 1;
    expect(() => validateTraceUploadV2(unsafe)).toThrow(/safe integer/);

    const negative = copy();
    negative.events[0].stdout_chunks = -1;
    expect(() => validateTraceUploadV2(negative)).toThrow(/events\[0\]\.stdout_chunks/);

    const inconsistent = copy();
    inconsistent.runs[0].duration_ms += 1;
    expect(() => validateTraceUploadV2(inconsistent)).toThrow(/duration_ms/);
  });

  it("rejects sequence zero even when it is declared as the final event", () => {
    const zero = copy();
    zero.events[0].sequence = 0;
    zero.runs[0].final_sequence = 0;
    expect(() => validateTraceUploadV2(zero)).toThrow(/sequence/);
  });

  it("rejects a persisted event whose sequence exceeds its declared final sequence", () => {
    const beyondFinal = copy();
    beyondFinal.events[2].sequence = 4;
    expect(() => validateTraceUploadV2(beyondFinal)).toThrow(/final_sequence/);
  });

  it("rejects duplicate identities and parent relationships that do not exist", () => {
    const duplicateRun = copy();
    duplicateRun.runs.push({ ...duplicateRun.runs[0] });
    expect(() => validateTraceUploadV2(duplicateRun)).toThrow(/duplicate run_id/);

    const duplicateSequence = copy();
    duplicateSequence.events.push({ ...duplicateSequence.events[0], event_id: "019d9c40-7b3c-7000-8000-000000000099" });
    expect(() => validateTraceUploadV2(duplicateSequence)).toThrow(/duplicate \(run_id, sequence\)/);

    const missingParent = copy();
    missingParent.events[0].run_id = "019d9c40-7b3c-7000-8000-000000000088";
    expect(() => validateTraceUploadV2(missingParent)).toThrow(/unknown run_id/);
  });

  it("rejects duplicate output chunk tuples independently of chunk IDs", () => {
    const duplicateTuple = copy();
    duplicateTuple.output_chunks.push({
      ...duplicateTuple.output_chunks[0],
      chunk_id: "019d9c40-7b3c-7000-8000-000000000099",
    });
    expect(() => validateTraceUploadV2(duplicateTuple)).toThrow(/duplicate \(event_id, stream, chunk_index\)/);
  });

  it("checks UTF-8 chunk byte counts and SHA-256 values", () => {
    const byteMismatch = copy();
    byteMismatch.output_chunks[0].byte_count += 1;
    expect(() => validateTraceUploadV2(byteMismatch)).toThrow(/byte_count/);

    const hashMismatch = copy();
    hashMismatch.output_chunks[0].sha256 = "0".repeat(64);
    expect(() => validateTraceUploadV2(hashMismatch)).toThrow(/sha256/);

    const oversized = copy();
    oversized.output_chunks[0].text = "你".repeat(10_923);
    oversized.output_chunks[0].byte_count = 32_769;
    expect(() => validateTraceUploadV2(oversized)).toThrow(/UTF-8 byte limit/);
  });

  it("rejects cursor fields outside the opaque V1 shape", () => {
    const encoded = btoa(JSON.stringify({ v: 1, started_at_ms: 1, run_id: valid.runs[0].run_id, extra: true }))
      .replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/g, "");
    expect(() => decodeTraceCursorV2(encoded)).toThrow(/unknown field: extra/);
  });

  it("publishes UTF-8 byte-limit metadata that matches executable multibyte rejection", () => {
    expect(schema.$defs.shortText["x-maxUtf8Bytes"]).toBe(1_024);
    expect(schema.$defs.text["x-maxUtf8Bytes"]).toBe(16_384);
    expect(schema.$defs.nullableShortText["x-maxUtf8Bytes"]).toBe(1_024);
    expect(schema.$defs.nullableText["x-maxUtf8Bytes"]).toBe(16_384);
    expect(schema.$defs.outputChunk.properties.text["x-maxUtf8Bytes"]).toBe(32_768);
    expect(schema.$defs.outputChunk.properties.sha256["x-maxUtf8Bytes"]).toBe(64);

    const multibyte = copy();
    multibyte.events[0].step_name = "你".repeat(342);
    expect(() => validateTraceUploadV2(multibyte)).toThrow(/UTF-8 byte limit of 1024/);
  });
});
