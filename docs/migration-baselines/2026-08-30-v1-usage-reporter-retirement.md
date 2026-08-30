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

The Tauri `UsageLogReporter` is now a thin adapter over this durable bridge.
`OperationCoordinator` receives that adapter at application construction; there
is no second in-memory queue. The adapter binds an opaque SHA-256 account owner,
a stable fingerprint of the Rust lifecycle generation, and a zeroizing request
token as one credential. A failed account-A queue is never sent with account-B
credentials, while a later generation of account A may resume it. Login,
logout, session stop, and bounded process-exit closeout all use that binding.

The production compatibility file is
`%LOCALAPPDATA%/Nwflash/v1-usage-retirement.json` (with the existing app-data
fallback rules). Queue writes remain synchronous and durable before
`OperationCoordinator::record` returns. A canceled closeout leaves the entry on
disk for the next process and never treats an attempted HTTP call as success.

This adapter is not the Plan C producer. Raw `UsageLogEntry` values remain
confined to the retiring V1 `/api/usage/logs` bridge and are never converted to
V2 metadata, passed to the Plan C metadata spool, or accepted by a V2 wire
encoder. Protection-sealed V2 production remains a separate implementation and
gate.

## Retirement gates

Before deleting the legacy reporter, the adapter must provide focused tests for:

- failed middle batches retaining the failed batch and unattempted tail;
- account and login-generation ownership across logout/login transitions;
- shutdown deadline cancellation with durable recovery after restart;
- a real `OperationCoordinator` path proving that completed operations are
  immediately recoverable from the durable V1 bridge;
- negative checks that persisted ownership contains no raw username,
  lifecycle generation, or bearer token, and that raw V1 payloads never enter
  the metadata-only Plan C producer.

This document is a migration boundary, not a deployment authorization. The
bridge and its tests do not prove the full Plan C redaction, crash, retention,
ACK, or HTTP integration gates.
