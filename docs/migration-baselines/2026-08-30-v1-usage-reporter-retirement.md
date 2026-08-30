# V1 Usage Reporter Retirement Boundary

This checkpoint isolates the legacy `/api/usage/logs` reporter from Plan C's
metadata-only trace pipeline. The compatibility bridge stores only the legacy
`UsageLogEntry` shape and never converts raw V1 payloads into V2 trace metadata.

## Compatibility bridge

`nwflash_infrastructure::usage_reporter::LegacyUsageReporter` is a durable,
owner-scoped queue for the remaining V1 adapter. Each queued entry records an
opaque account owner and login generation. A later generation for the same
account may resume the queue; a different account cannot upload it. Successful
HTTP batches are removed only after an atomic local spool replacement. Failed
or canceled batches, including every unattempted tail, remain durable.

The bridge is intentionally not wired into the Tauri `UsageLogReporter` yet.
The existing Tauri reporter is still a best-effort in-memory `/api/usage/logs`
implementation and must not be treated as the Plan C producer. A future Tauri
adapter must construct protection-sealed, redacted metadata before calling the
Plan C spool/uploader; it must not pass raw `UsageLogEntry` values through a V2
wire encoder.

## Retirement gates

Before deleting the legacy reporter, the adapter must provide focused tests for:

- failed middle batches retaining the failed batch and unattempted tail;
- account and login-generation ownership across logout/login transitions;
- shutdown deadline cancellation with durable recovery after restart;
- a negative assertion that no raw V1 payload is accepted by the metadata-only
  Plan C producer.

This document is a migration boundary, not a deployment authorization. The
bridge and its tests do not prove the full Plan C redaction, crash, retention,
ACK, or HTTP integration gates.
