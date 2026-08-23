import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import type { TraceUploadRequestV2 } from "../src/trace-v2-contract";
import { validateTraceUploadV2 } from "../src/trace-v2-contract";
import { redactTraceUploadV2 } from "../src/trace-v2-redaction";

const canonical = JSON.parse(readFileSync(
  new URL("../contracts/trace-v2/upload.success.json", import.meta.url),
  "utf8",
)) as TraceUploadRequestV2;

function copyCanonical(): TraceUploadRequestV2 {
  return JSON.parse(JSON.stringify(canonical)) as TraceUploadRequestV2;
}

function setChunkText(payload: TraceUploadRequestV2, text: string): void {
  const bytes = Buffer.from(text, "utf8");
  payload.output_chunks[0] = {
    ...payload.output_chunks[0],
    text,
    byte_count: bytes.byteLength,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

function replaceChunkText(payload: TraceUploadRequestV2, index: number, text: string): void {
  const bytes = Buffer.from(text, "utf8");
  payload.output_chunks[index] = {
    ...payload.output_chunks[index],
    text,
    byte_count: bytes.byteLength,
    sha256: createHash("sha256").update(bytes).digest("hex"),
  };
}

describe("trace v2 credential boundary", () => {
  it("removes credentials but preserves operational fields", () => {
    const bearer = "bearer-sentinel-314159";
    const cookie = "cookie-sentinel-271828";
    const exact = "exact-sentinel-141421";
    const serial = "9A7F23BC10D4";
    const path = "C:\\Users\\Public\\Nwflash\\super.img";
    const source = copyCanonical();

    source.runs[0].device_serial = serial;
    source.runs[0].source_paths = [path];
    source.runs[0].source_urls = [
      "https://firmware.example/rom.zip?channel=stable&token=query-sentinel-161803",
    ];
    source.runs[0].error_message = "flash failed password=password-sentinel-173205";

    const event = source.events[1];
    event.command = {
      program: "C:\\Nwflash\\platform-tools\\fastboot.exe",
      argv: ["flash", "super", path, "--api-key", "cli-sentinel-223607", "--label", "nightly"],
      display_command: "uploader --secret=assignment-sentinel-244949",
      working_directory: "C:\\Nwflash\\platform-tools",
      paths: [path],
      urls: [
        "https://operator:userinfo-sentinel-264575@firmware.example/rom.zip?signature=signature-sentinel-282842&channel=stable",
      ],
      serial,
    };
    event.verification = `authenticated with ${exact}`;
    event.remedies = ["Set-Cookie: session=set-cookie-sentinel-316227; Path=/; Secure"];
    event.credential_redactions = [{ kind: "client", count: 2 }];

    setChunkText(source, [
      `Authorization: Bearer ${bearer}`,
      `Cookie: session=${cookie}`,
      `device ${serial}`,
      `flashing ${path}`,
      "",
    ].join("\n"));

    const result = redactTraceUploadV2(source, [bearer, exact, "tiny"]);
    const stored = JSON.stringify(result.payload);

    for (const secret of [
      bearer,
      cookie,
      exact,
      "query-sentinel-161803",
      "password-sentinel-173205",
      "cli-sentinel-223607",
      "assignment-sentinel-244949",
      "userinfo-sentinel-264575",
      "signature-sentinel-282842",
      "set-cookie-sentinel-316227",
    ]) {
      expect(stored).not.toContain(secret);
    }
    expect(stored).toContain(serial);
    expect(stored).toContain("C:\\\\Users\\\\Public\\\\Nwflash\\\\super.img");
    expect(result.payload.events[0].verification).toBe("Bearer authentication accepted");
    expect(stored).toContain("https://firmware.example/rom.zip?channel=stable&token=[REDACTED]");
    expect(stored).toContain("&channel=stable");
    expect(result.payload.events[1].command?.argv).toEqual([
      "flash", "super", path, "--api-key", "[REDACTED]", "--label", "nightly",
    ]);
    expect(result.run_redactions.get(source.runs[0].run_id)).toEqual([
      { kind: "token", count: 1 },
      { kind: "password", count: 1 },
    ]);
    expect(result.event_redactions.get(event.event_id)).toEqual(expect.arrayContaining([
      { kind: "client", count: 2 },
      { kind: "api-key", count: 1 },
      { kind: "secret", count: 1 },
      { kind: "url-userinfo", count: 1 },
      { kind: "signature", count: 1 },
      { kind: "exact", count: 1 },
      { kind: "cookie", count: 1 },
    ]));
    expect(result.payload.events[1].credential_redactions).toEqual(
      result.event_redactions.get(event.event_id),
    );
    expect(result.chunk_redactions.get(source.output_chunks[0].chunk_id)).toEqual([
      { kind: "bearer", count: 1 },
      { kind: "cookie", count: 1 },
    ]);

    const storedChunk = result.payload.output_chunks[0];
    expect(storedChunk.text).toBe(
      "Authorization: Bearer [REDACTED]\n"
      + "Cookie: session=[REDACTED]\n"
      + `device ${serial}\n`
      + `flashing ${path}\n`,
    );
    expect(storedChunk.byte_count).toBe(123);
    expect(storedChunk.sha256).toBe("c811bd196547c3560e5db8c43567c4b205cc3990245d96d8f04707200e0a67fd");
    expect(validateTraceUploadV2(result.payload)).toEqual(result.payload);
    expect(source.output_chunks[0].text).toContain(bearer);
  });

  it("removes complete private keys and fails closed on unterminated private-key material", () => {
    const source = copyCanonical();
    const privateKey = "-----BEGIN PRIVATE KEY-----\nprivate-sentinel-331662\n-----END PRIVATE KEY-----";
    source.runs[0].error_message = privateKey;
    source.events[1].error_message = "-----BEGIN OPENSSH PRIVATE KEY-----\nunterminated-sentinel-346410";

    const result = redactTraceUploadV2(source, []);

    expect(JSON.stringify(result.payload)).not.toContain("private-sentinel-331662");
    expect(JSON.stringify(result.payload)).not.toContain("unterminated-sentinel-346410");
    expect(result.payload.runs[0].error_message).toBe("[CREDENTIAL_REMOVED:PRIVATE_KEY]");
    expect(result.payload.events[1].error_message).toBe("[CREDENTIAL_REMOVED:HIGH_RISK]");
    expect(result.run_redactions.get(source.runs[0].run_id)).toEqual([{ kind: "private-key", count: 1 }]);
    expect(result.event_redactions.get(source.events[1].event_id)).toEqual([{ kind: "high-risk", count: 1 }]);
  });

  it("detects credentials split across adjacent chunks in one stream", () => {
    const source = copyCanonical();
    source.events[1].stdout_chunks = 2;
    source.events[1].stderr_chunks = 0;
    source.output_chunks[1] = {
      ...source.output_chunks[1],
      stream: "stdout",
      chunk_index: 1,
    };
    replaceChunkText(source, 0, "progress token-part-");
    replaceChunkText(source, 1, "exact-424242 token=late-sentinel-387298 OKAY\n");

    const result = redactTraceUploadV2(source, ["token-part-exact-424242"]);
    const storedText = result.payload.output_chunks
      .sort((left, right) => left.chunk_index - right.chunk_index)
      .map((chunk) => chunk.text)
      .join("");

    expect(storedText).not.toContain("token-part-exact-424242");
    expect(result.chunk_redactions.get(source.output_chunks[0].chunk_id)).toEqual([{ kind: "exact", count: 1 }]);
    expect(result.chunk_redactions.get(source.output_chunks[1].chunk_id)).toEqual([{ kind: "token", count: 1 }]);
    expect(validateTraceUploadV2(result.payload)).toEqual(result.payload);
  });

  it("recognizes prefixed secret names without consuming a following option as a value", () => {
    const source = copyCanonical();
    const command = source.events[1].command!;
    command.argv = [
      "upload",
      "--client-secret",
      "client-sentinel-360555",
      "--token",
      "--label",
      "nightly",
      "--token",
      "-secret-value",
    ];
    command.display_command = "upload aws_secret_access_key=aws-sentinel-374165";

    const result = redactTraceUploadV2(source, []);
    const redactedCommand = result.payload.events[1].command!;

    expect(JSON.stringify(redactedCommand)).not.toContain("client-sentinel-360555");
    expect(JSON.stringify(redactedCommand)).not.toContain("aws-sentinel-374165");
    expect(redactedCommand.argv).toEqual([
      "upload",
      "--client-secret",
      "[REDACTED]",
      "--token",
      "--label",
      "nightly",
      "--token",
      "[REDACTED]",
    ]);
    expect(result.event_redactions.get(source.events[1].event_id)).toEqual([
      { kind: "secret", count: 2 },
      { kind: "token", count: 1 },
    ]);
  });

  it("keeps a maximum-size chunk valid when a short credential value is replaced", () => {
    const source = copyCanonical();
    setChunkText(source, `${"a".repeat(32_761)} pwd=x`);

    const result = redactTraceUploadV2(source, []);
    const chunk = result.payload.output_chunks[0];

    expect(chunk.text).not.toContain("pwd=x");
    expect(chunk.byte_count).toBeLessThanOrEqual(32_768);
    expect(validateTraceUploadV2(result.payload)).toEqual(result.payload);
  });

  it("redacts a credential-like dash value but preserves a safe option boundary", () => {
    const sensitive = copyCanonical();
    sensitive.events[1].command!.argv = ["--token", "--secret-value", "positional"];
    const safeBoundary = copyCanonical();
    safeBoundary.events[1].command!.argv = ["--token", "--label"];

    expect(redactTraceUploadV2(sensitive, []).payload.events[1].command!.argv).toEqual([
      "--token",
      "[REDACTED]",
      "positional",
    ]);
    expect(redactTraceUploadV2(safeBoundary, []).payload.events[1].command!.argv).toEqual([
      "--token",
      "--label",
    ]);
  });

  it("redacts an exact registered secret before treating it as an option boundary", () => {
    const source = copyCanonical();
    source.events[1].command!.argv = ["--token", "--label"];
    const result = redactTraceUploadV2(source, ["--label"]);

    expect(result.payload.events[1].command!.argv).toEqual([
      "--token",
      "*******",
    ]);
    expect(result.event_redactions.get(source.events[1].event_id)).toEqual([{ kind: "exact", count: 1 }]);
  });

  it("keeps merged counts and high-risk field replacement within the frozen contract", () => {
    const source = copyCanonical();
    source.events[1].credential_redactions = [
      { kind: "token", count: Number.MAX_SAFE_INTEGER },
      ...Array.from({ length: 99 }, (_, index) => ({ kind: `client-${index}`, count: 1 })),
    ];
    source.events[1].error_message = "password=server-sentinel-400000";
    const begin = "-----BEGIN PRIVATE KEY-----";
    source.runs[0].error_message = `${"a".repeat(16_384 - begin.length)}${begin}`;

    const result = redactTraceUploadV2(source, []);

    expect(result.payload.events[1].credential_redactions).toHaveLength(100);
    expect(result.payload.events[1].credential_redactions[0]).toEqual({ kind: "token", count: Number.MAX_SAFE_INTEGER });
    expect(result.payload.runs[0].error_message).toBe("[CREDENTIAL_REMOVED:HIGH_RISK]");
    expect(validateTraceUploadV2(result.payload)).toEqual(result.payload);
  });
});
