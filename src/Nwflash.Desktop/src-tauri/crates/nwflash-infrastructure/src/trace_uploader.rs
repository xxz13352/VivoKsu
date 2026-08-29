//! Metadata-only state machine for protection-sealed trace upload attempts.

use crate::trace_spool::{
    AttemptPauseReason, InflightAttemptHandle, OwnerPauseReason, ProtectionClientVersionHash,
    ProtectionSealedUploadId, ResealReason, TraceItemKey, TraceOwnerGeneration, TraceSpoolEntity,
    TraceSpoolError, TraceSpoolStore, TRACE_SPOOL_MAX_CHUNKS, TRACE_SPOOL_MAX_EVENTS,
    TRACE_SPOOL_MAX_RUNS, TRACE_SPOOL_MAX_WIRE_BYTES,
};
use std::collections::HashSet;

const MAX_ACK_ITEMS: usize = 320;
const MAX_ACK_ID_BYTES: usize = 256;

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
const RETRY_BASE_MS: u64 = 1_000;
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
const RETRY_MAX_MS: u64 = 5 * 60 * 1_000;
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
const SERVER_UNACKED_DELAY_MS: u64 = 1_000;
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
const MAX_INJECTED_JITTER_MS: u64 = 1_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceAcceptedAck {
    pub runs: Vec<String>,
    pub events: Vec<String>,
    pub output_chunks: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum TraceRejectedCode {
    Invalid,
    MissingParent,
    SequenceConflict,
    IncompleteTrace,
    CredentialRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceRejectedAck {
    pub entity: TraceSpoolEntity,
    pub id: Option<String>,
    pub code: TraceRejectedCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceUploadAck {
    pub ok: bool,
    pub accepted: TraceAcceptedAck,
    pub rejected: Vec<TraceRejectedAck>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct AckValidationError;

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct ValidatedTraceAck {
    accepted: Vec<TraceItemKey>,
    rejected: Vec<TraceItemKey>,
    unacknowledged: Vec<TraceItemKey>,
    credential_rejected: Vec<TraceItemKey>,
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
impl ValidatedTraceAck {
    pub(crate) fn accepted(&self) -> &[TraceItemKey] {
        &self.accepted
    }

    pub(crate) fn rejected(&self) -> &[TraceItemKey] {
        &self.rejected
    }

    pub(crate) fn unacknowledged(&self) -> &[TraceItemKey] {
        &self.unacknowledged
    }

    pub(crate) fn credential_rejected(&self) -> &[TraceItemKey] {
        &self.credential_rejected
    }
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
fn validate_success_ack(
    dispatched: &[TraceItemKey],
    ack: &TraceUploadAck,
) -> Result<ValidatedTraceAck, AckValidationError> {
    if !ack.ok {
        return Err(AckValidationError);
    }
    let dispatched_set = dispatched.iter().cloned().collect::<HashSet<_>>();
    if dispatched_set.len() != dispatched.len() {
        return Err(AckValidationError);
    }
    let ack_item_count = ack
        .accepted
        .runs
        .len()
        .saturating_add(ack.accepted.events.len())
        .saturating_add(ack.accepted.output_chunks.len())
        .saturating_add(ack.rejected.len());
    if ack_item_count > MAX_ACK_ITEMS {
        return Err(AckValidationError);
    }
    if ack
        .accepted
        .runs
        .iter()
        .chain(&ack.accepted.events)
        .chain(&ack.accepted.output_chunks)
        .any(|id| !valid_ack_id(id))
        || ack
            .rejected
            .iter()
            .any(|item| item.id.as_deref().is_none_or(|id| !valid_ack_id(id)))
    {
        return Err(AckValidationError);
    }

    let mut accepted = Vec::new();
    for (entity, ids) in [
        (TraceSpoolEntity::Run, ack.accepted.runs.as_slice()),
        (TraceSpoolEntity::Event, ack.accepted.events.as_slice()),
        (
            TraceSpoolEntity::OutputChunk,
            ack.accepted.output_chunks.as_slice(),
        ),
    ] {
        accepted.extend(ids.iter().map(|id| TraceItemKey::new(entity, id.clone())));
    }

    let mut seen = HashSet::new();
    if accepted
        .iter()
        .any(|key| !dispatched_set.contains(key) || !seen.insert(key.clone()))
    {
        return Err(AckValidationError);
    }

    let mut rejected = Vec::new();
    let mut credential_rejected = Vec::new();
    for item in &ack.rejected {
        let Some(id) = item.id.as_ref() else {
            return Err(AckValidationError);
        };
        let key = TraceItemKey::new(item.entity, id.clone());
        if !dispatched_set.contains(&key) || !seen.insert(key.clone()) {
            return Err(AckValidationError);
        }
        if item.code == TraceRejectedCode::CredentialRejected {
            if item.entity != TraceSpoolEntity::OutputChunk {
                return Err(AckValidationError);
            }
            credential_rejected.push(key.clone());
        }
        rejected.push(key);
    }

    let unacknowledged = dispatched
        .iter()
        .filter(|key| !seen.contains(*key))
        .cloned()
        .collect();
    Ok(ValidatedTraceAck {
        accepted,
        rejected,
        unacknowledged,
        credential_rejected,
    })
}

fn valid_ack_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ACK_ID_BYTES && !id.chars().any(char::is_control)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum UploadConnectivity {
    Offline,
    Online,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum TraceApiErrorCode {
    BodyTooLarge,
    Invalid,
    Unauthorized,
    Forbidden,
    OwnershipConflict,
    Incomplete,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceUploadErrorEnvelope {
    pub ok: bool,
    pub code: TraceApiErrorCode,
    pub details: Vec<TraceRejectedAck>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum TraceHttpEnvelope {
    Success(TraceUploadAck),
    Error(TraceUploadErrorEnvelope),
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceHttpResponse {
    pub status: u16,
    pub envelope: Option<TraceHttpEnvelope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum TraceHttpOutcome {
    TransportFailure,
    Response(TraceHttpResponse),
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct DispatchInstruction {
    inflight_handle: InflightAttemptHandle,
    captured_owner: TraceOwnerGeneration,
    protection_sealed_upload_id: ProtectionSealedUploadId,
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
impl DispatchInstruction {
    pub(crate) fn inflight_handle(&self) -> &InflightAttemptHandle {
        &self.inflight_handle
    }

    pub(crate) fn captured_owner(&self) -> &TraceOwnerGeneration {
        &self.captured_owner
    }

    pub(crate) fn protection_sealed_upload_id(&self) -> &ProtectionSealedUploadId {
        &self.protection_sealed_upload_id
    }
}

impl std::fmt::Debug for DispatchInstruction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DispatchInstruction")
            .field("captured_owner", &self.captured_owner)
            .field("item_count", &self.inflight_handle.items().len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum DispatchTickOutcome {
    Offline,
    Idle,
    Dispatch(Box<DispatchInstruction>),
    RemediationRequired(Box<RemediationInstruction>),
    LocalContractFailure,
}

impl PartialEq for DispatchTickOutcome {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Offline, Self::Offline)
                | (Self::Idle, Self::Idle)
                | (Self::LocalContractFailure, Self::LocalContractFailure)
        )
    }
}

impl Eq for DispatchTickOutcome {}

#[derive(Clone)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct RemediationInstruction {
    inflight_handle: InflightAttemptHandle,
    owner: TraceOwnerGeneration,
    items: Vec<TraceItemKey>,
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
impl RemediationInstruction {
    pub(crate) fn inflight_handle(&self) -> &InflightAttemptHandle {
        &self.inflight_handle
    }

    pub(crate) fn owner(&self) -> &TraceOwnerGeneration {
        &self.owner
    }

    pub(crate) fn items(&self) -> &[TraceItemKey] {
        &self.items
    }
}

impl std::fmt::Debug for RemediationInstruction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemediationInstruction")
            .field("owner", &self.owner)
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl PartialEq for RemediationInstruction {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner
            && self.items == other.items
            && self.inflight_handle.attempt_id() == other.inflight_handle.attempt_id()
    }
}

impl Eq for RemediationInstruction {}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum UploadTickOutcome {
    Uploaded { accepted: usize, stale: usize },
    Backoff { retry_at_ms: u64 },
    Unauthorized,
    Forbidden,
    ManualIntervention { status: u16 },
    UpdateRequired,
    RemediationRequired(Box<RemediationInstruction>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) enum TraceUploaderError {
    #[error("trace upload spool transition failed")]
    Spool,
}

impl From<TraceSpoolError> for TraceUploaderError {
    fn from(_: TraceSpoolError) -> Self {
        Self::Spool
    }
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
pub(crate) struct TraceUploader {
    jitter_ms: u64,
}

#[allow(dead_code, reason = "Wave 3 concrete emitter integration seam")]
impl TraceUploader {
    pub(crate) fn new(jitter_ms: u64) -> Self {
        Self {
            jitter_ms: jitter_ms.min(MAX_INJECTED_JITTER_MS),
        }
    }

    pub(crate) fn next_dispatch(
        &self,
        store: &TraceSpoolStore,
        owner: &TraceOwnerGeneration,
        current_client_version_hash: &ProtectionClientVersionHash,
        connectivity: UploadConnectivity,
        now_ms: u64,
    ) -> Result<DispatchTickOutcome, TraceUploaderError> {
        store.expire(owner, now_ms)?;
        if connectivity == UploadConnectivity::Offline {
            return Ok(DispatchTickOutcome::Offline);
        }
        if let Some(remediation) = store
            .due_remediations(owner, current_client_version_hash)?
            .into_iter()
            .next()
        {
            return Ok(DispatchTickOutcome::RemediationRequired(Box::new(
                RemediationInstruction {
                    inflight_handle: remediation.handle().clone(),
                    owner: remediation.handle().owner().clone(),
                    items: remediation.affected().to_vec(),
                },
            )));
        }
        let Some(due) = store
            .due_attempts(owner, now_ms)?
            .into_iter()
            .find(|attempt| attempt.client_version_hash() == current_client_version_hash)
        else {
            return Ok(DispatchTickOutcome::Idle);
        };
        let handle = store.begin_dispatch(due.attempt_id(), owner)?;
        if handle.wire_bytes() > TRACE_SPOOL_MAX_WIRE_BYTES
            || handle.run_count() > TRACE_SPOOL_MAX_RUNS
            || handle.event_count() > TRACE_SPOOL_MAX_EVENTS
            || handle.chunk_count() > TRACE_SPOOL_MAX_CHUNKS
        {
            store.pause_attempt(&handle, AttemptPauseReason::LocalContract)?;
            return Ok(DispatchTickOutcome::LocalContractFailure);
        }
        Ok(DispatchTickOutcome::Dispatch(Box::new(
            DispatchInstruction {
                captured_owner: handle.owner().clone(),
                protection_sealed_upload_id: handle.sealed_upload_id().clone(),
                inflight_handle: handle,
            },
        )))
    }

    pub(crate) fn apply_http_outcome(
        &self,
        store: &TraceSpoolStore,
        handle: &InflightAttemptHandle,
        outcome: TraceHttpOutcome,
        now_ms: u64,
    ) -> Result<UploadTickOutcome, TraceUploaderError> {
        match outcome {
            TraceHttpOutcome::TransportFailure => self.retryable(store, handle, now_ms),
            TraceHttpOutcome::Response(response) => match response.status {
                200 => self.apply_success(store, handle, response.envelope, now_ms),
                401 => {
                    store.pause_owner(handle, OwnerPauseReason::Unauthorized)?;
                    Ok(UploadTickOutcome::Unauthorized)
                }
                403 => {
                    store.pause_owner(handle, OwnerPauseReason::Forbidden)?;
                    Ok(UploadTickOutcome::Forbidden)
                }
                422 => self.apply_incomplete(store, handle, response.envelope, now_ms),
                426 => {
                    store.pause_client_version_for_update(handle)?;
                    Ok(UploadTickOutcome::UpdateRequired)
                }
                429 => self.retryable(store, handle, now_ms),
                500..=599 => self.retryable(store, handle, now_ms),
                status => self.manual_intervention(store, handle, status),
            },
        }
    }

    fn apply_success(
        &self,
        store: &TraceSpoolStore,
        handle: &InflightAttemptHandle,
        envelope: Option<TraceHttpEnvelope>,
        now_ms: u64,
    ) -> Result<UploadTickOutcome, TraceUploaderError> {
        let Some(TraceHttpEnvelope::Success(ack)) = envelope else {
            return self.manual_intervention(store, handle, 200);
        };
        let dispatched = handle
            .items()
            .iter()
            .map(|item| item.key().clone())
            .collect::<Vec<_>>();
        let Ok(validated) = validate_success_ack(&dispatched, &ack) else {
            return self.manual_intervention(store, handle, 200);
        };

        let accepted = store.apply_accepted_cas(handle, validated.accepted())?;
        let remediation = if validated.credential_rejected().is_empty() {
            Vec::new()
        } else {
            store.mark_needs_remediation(handle, validated.credential_rejected())?
        };
        store.retire_attempt_and_mark_reseal_cas(
            handle,
            now_ms.saturating_add(SERVER_UNACKED_DELAY_MS),
            ResealReason::ServerUnacked,
        )?;

        if !remediation.is_empty() {
            return Ok(UploadTickOutcome::RemediationRequired(Box::new(
                RemediationInstruction {
                    inflight_handle: handle.clone(),
                    owner: handle.owner().clone(),
                    items: remediation,
                },
            )));
        }
        Ok(UploadTickOutcome::Uploaded {
            accepted: accepted.matched_items(),
            stale: accepted.stale_items(),
        })
    }

    fn apply_incomplete(
        &self,
        store: &TraceSpoolStore,
        handle: &InflightAttemptHandle,
        envelope: Option<TraceHttpEnvelope>,
        now_ms: u64,
    ) -> Result<UploadTickOutcome, TraceUploaderError> {
        let Some(TraceHttpEnvelope::Error(error)) = envelope else {
            return self.manual_intervention(store, handle, 422);
        };
        if error.ok || error.code != TraceApiErrorCode::Incomplete {
            return self.manual_intervention(store, handle, 422);
        }
        let dispatched = handle
            .items()
            .iter()
            .map(|item| item.key().clone())
            .collect::<Vec<_>>();
        let details_ack = TraceUploadAck {
            ok: true,
            accepted: TraceAcceptedAck::default(),
            rejected: error.details,
        };
        let Ok(validated) = validate_success_ack(&dispatched, &details_ack) else {
            return self.manual_intervention(store, handle, 422);
        };
        let remediation = if validated.credential_rejected().is_empty() {
            Vec::new()
        } else {
            store.mark_needs_remediation(handle, validated.credential_rejected())?
        };
        store.retire_attempt_and_mark_reseal_cas(
            handle,
            now_ms.saturating_add(SERVER_UNACKED_DELAY_MS),
            ResealReason::ServerUnacked,
        )?;
        if remediation.is_empty() {
            Ok(UploadTickOutcome::Backoff {
                retry_at_ms: now_ms.saturating_add(SERVER_UNACKED_DELAY_MS),
            })
        } else {
            Ok(UploadTickOutcome::RemediationRequired(Box::new(
                RemediationInstruction {
                    inflight_handle: handle.clone(),
                    owner: handle.owner().clone(),
                    items: remediation,
                },
            )))
        }
    }

    fn retryable(
        &self,
        store: &TraceSpoolStore,
        handle: &InflightAttemptHandle,
        now_ms: u64,
    ) -> Result<UploadTickOutcome, TraceUploaderError> {
        let shift = handle.attempt_count().min(63);
        let exponential = RETRY_BASE_MS
            .checked_shl(shift)
            .unwrap_or(u64::MAX)
            .min(RETRY_MAX_MS);
        let retry_at_ms = now_ms
            .saturating_add(exponential)
            .saturating_add(self.jitter_ms);
        store.retire_attempt_and_mark_reseal_cas(handle, retry_at_ms, ResealReason::Retryable)?;
        Ok(UploadTickOutcome::Backoff { retry_at_ms })
    }

    fn manual_intervention(
        &self,
        store: &TraceSpoolStore,
        handle: &InflightAttemptHandle,
        status: u16,
    ) -> Result<UploadTickOutcome, TraceUploaderError> {
        store.pause_attempt(handle, AttemptPauseReason::ManualIntervention)?;
        Ok(UploadTickOutcome::ManualIntervention { status })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_spool::{
        ProtectionClientVersionHash, ProtectionSealedUploadId, SealedAttemptId,
        SealedAttemptManifest, SealedItemRevision, TraceItemKey, TraceOwnerGeneration,
        TraceSpoolEntity, TraceSpoolStore,
    };
    use tempfile::TempDir;

    fn dispatched() -> Vec<TraceItemKey> {
        vec![
            TraceItemKey::new(TraceSpoolEntity::Run, "run"),
            TraceItemKey::new(TraceSpoolEntity::Event, "event"),
            TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk"),
        ]
    }

    fn empty_accepted() -> TraceAcceptedAck {
        TraceAcceptedAck {
            runs: vec![],
            events: vec![],
            output_chunks: vec![],
        }
    }

    fn owner(seed: u8, generation: u64) -> TraceOwnerGeneration {
        TraceOwnerGeneration::from_canonical_username_hash([seed; 32], generation)
    }

    fn client_version(seed: u8) -> ProtectionClientVersionHash {
        ProtectionClientVersionHash::from_digest([seed; 32])
    }

    fn sealed_item(
        entity: TraceSpoolEntity,
        id: &str,
        parent: Option<TraceItemKey>,
        revision: u64,
    ) -> SealedItemRevision {
        SealedItemRevision::new(TraceItemKey::new(entity, id), "trace", parent, revision, 0)
    }

    fn manifest_with_version(
        seed: u8,
        version_seed: u8,
        owner: TraceOwnerGeneration,
        items: Vec<SealedItemRevision>,
    ) -> SealedAttemptManifest {
        let runs = items
            .iter()
            .filter(|item| item.key().entity() == TraceSpoolEntity::Run)
            .count() as u16;
        let events = items
            .iter()
            .filter(|item| item.key().entity() == TraceSpoolEntity::Event)
            .count() as u16;
        let chunks = items
            .iter()
            .filter(|item| item.key().entity() == TraceSpoolEntity::OutputChunk)
            .count() as u16;
        SealedAttemptManifest::new(
            SealedAttemptId::from_digest([seed; 32]),
            owner,
            ProtectionSealedUploadId::from_digest([seed.wrapping_add(0x40); 32]),
            client_version(version_seed),
            512,
            runs,
            events,
            chunks,
            items,
        )
    }

    fn manifest(
        seed: u8,
        owner: TraceOwnerGeneration,
        items: Vec<SealedItemRevision>,
    ) -> SealedAttemptManifest {
        manifest_with_version(seed, 0x55, owner, items)
    }

    fn success(ack: TraceUploadAck) -> TraceHttpOutcome {
        TraceHttpOutcome::Response(TraceHttpResponse {
            status: 200,
            envelope: Some(TraceHttpEnvelope::Success(ack)),
        })
    }

    fn dispatch(
        uploader: &TraceUploader,
        store: &TraceSpoolStore,
        owner: &TraceOwnerGeneration,
        now_ms: u64,
    ) -> DispatchInstruction {
        match uploader
            .next_dispatch(
                store,
                owner,
                &client_version(0x55),
                UploadConnectivity::Online,
                now_ms,
            )
            .unwrap()
        {
            DispatchTickOutcome::Dispatch(instruction) => *instruction,
            unexpected => panic!("expected dispatch, got {unexpected:?}"),
        }
    }

    #[test]
    fn strict_ack_accepts_exact_mixed_membership_only() {
        let ack = TraceUploadAck {
            ok: true,
            accepted: TraceAcceptedAck {
                runs: vec!["run".to_owned()],
                events: vec![],
                output_chunks: vec![],
            },
            rejected: vec![TraceRejectedAck {
                entity: TraceSpoolEntity::Event,
                id: Some("event".to_owned()),
                code: TraceRejectedCode::Invalid,
            }],
        };

        let validated = validate_success_ack(&dispatched(), &ack).unwrap();

        assert_eq!(
            validated.accepted(),
            &[TraceItemKey::new(TraceSpoolEntity::Run, "run")]
        );
        assert_eq!(validated.rejected().len(), 1);
        assert_eq!(
            validated.unacknowledged(),
            &[TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk")]
        );
    }

    #[test]
    fn malicious_unknown_duplicate_overlap_cross_entity_or_null_ack_fails_closed() {
        let cases = [
            TraceUploadAck {
                ok: true,
                accepted: TraceAcceptedAck {
                    runs: vec!["unknown".into()],
                    ..empty_accepted()
                },
                rejected: vec![],
            },
            TraceUploadAck {
                ok: true,
                accepted: TraceAcceptedAck {
                    runs: vec!["run".into(), "run".into()],
                    ..empty_accepted()
                },
                rejected: vec![],
            },
            TraceUploadAck {
                ok: true,
                accepted: TraceAcceptedAck {
                    runs: vec!["run".into()],
                    ..empty_accepted()
                },
                rejected: vec![TraceRejectedAck {
                    entity: TraceSpoolEntity::Run,
                    id: Some("run".into()),
                    code: TraceRejectedCode::Invalid,
                }],
            },
            TraceUploadAck {
                ok: true,
                accepted: TraceAcceptedAck {
                    events: vec!["run".into()],
                    ..empty_accepted()
                },
                rejected: vec![],
            },
            TraceUploadAck {
                ok: true,
                accepted: empty_accepted(),
                rejected: vec![TraceRejectedAck {
                    entity: TraceSpoolEntity::Event,
                    id: None,
                    code: TraceRejectedCode::Invalid,
                }],
            },
            TraceUploadAck {
                ok: false,
                accepted: empty_accepted(),
                rejected: vec![],
            },
        ];

        for ack in cases {
            assert_eq!(
                validate_success_ack(&dispatched(), &ack),
                Err(AckValidationError)
            );
        }
    }

    #[test]
    fn duplicate_rejected_ids_fail_closed_even_when_codes_differ() {
        let ack = TraceUploadAck {
            ok: true,
            accepted: empty_accepted(),
            rejected: vec![
                TraceRejectedAck {
                    entity: TraceSpoolEntity::OutputChunk,
                    id: Some("chunk".into()),
                    code: TraceRejectedCode::Invalid,
                },
                TraceRejectedAck {
                    entity: TraceSpoolEntity::OutputChunk,
                    id: Some("chunk".into()),
                    code: TraceRejectedCode::CredentialRejected,
                },
            ],
        };

        assert_eq!(
            validate_success_ack(&dispatched(), &ack),
            Err(AckValidationError)
        );
    }

    #[test]
    fn credential_rejections_are_returned_as_stable_metadata_instructions() {
        let ack = TraceUploadAck {
            ok: true,
            accepted: empty_accepted(),
            rejected: vec![TraceRejectedAck {
                entity: TraceSpoolEntity::OutputChunk,
                id: Some("chunk".into()),
                code: TraceRejectedCode::CredentialRejected,
            }],
        };

        let validated = validate_success_ack(&dispatched(), &ack).unwrap();

        assert_eq!(
            validated.credential_rejected(),
            &[TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk")]
        );
    }

    #[test]
    fn credential_rejection_for_a_non_chunk_is_protocol_invalid() {
        let ack = TraceUploadAck {
            ok: true,
            accepted: empty_accepted(),
            rejected: vec![TraceRejectedAck {
                entity: TraceSpoolEntity::Run,
                id: Some("run".into()),
                code: TraceRejectedCode::CredentialRejected,
            }],
        };

        assert_eq!(
            validate_success_ack(&dispatched(), &ack),
            Err(AckValidationError)
        );
    }

    #[test]
    fn offline_returns_before_claiming_a_pending_attempt() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);

        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Offline,
                    0
                )
                .unwrap(),
            DispatchTickOutcome::Offline
        );
        assert_eq!(store.due_attempts(&current, 0).unwrap().len(), 1);
    }

    #[test]
    fn offline_still_applies_the_seven_day_metadata_retention_cap() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "expired");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "expired", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);

        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Offline,
                    crate::trace_spool::TRACE_SPOOL_RETENTION_MS,
                )
                .unwrap(),
            DispatchTickOutcome::Offline
        );
        assert_eq!(store.current_revision_for_test(&current, &key), None);
    }

    #[test]
    fn inflight_attempt_is_not_recovered_or_redispatched_in_the_same_process() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);

        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    0
                )
                .unwrap(),
            DispatchTickOutcome::Idle
        );
        assert!(store
            .due_reseal_items(&current, 0, &client_version(0x55))
            .unwrap()
            .is_empty());
        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    success(TraceUploadAck {
                        ok: true,
                        accepted: TraceAcceptedAck {
                            runs: vec!["run".into()],
                            ..empty_accepted()
                        },
                        rejected: vec![],
                    }),
                    0,
                )
                .unwrap(),
            UploadTickOutcome::Uploaded {
                accepted: 1,
                stale: 0
            }
        );
    }

    #[test]
    fn retryable_failure_retires_the_seal_and_requires_reseal_with_backoff() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);

        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    TraceHttpOutcome::TransportFailure,
                    0,
                )
                .unwrap(),
            UploadTickOutcome::Backoff { retry_at_ms: 1_000 }
        );
        assert!(store.due_attempts(&current, 1_000).unwrap().is_empty());
        assert!(store
            .due_reseal_items(&current, 999, &client_version(0x55))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .due_reseal_items(&current, 1_000, &client_version(0x55))
                .unwrap()[0]
                .attempt_count(),
            1
        );
    }

    #[test]
    fn retry_dispatch_requires_fresh_attempt_and_opaque_seal_with_stable_revision() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let first = dispatch(&uploader, &store, &current, 0);
        let first_attempt = first.inflight_handle().attempt_id().clone();
        let first_seal = first.protection_sealed_upload_id().clone();
        uploader
            .apply_http_outcome(
                &store,
                first.inflight_handle(),
                TraceHttpOutcome::TransportFailure,
                0,
            )
            .unwrap();

        store
            .register_resealed_attempt(manifest(
                2,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let second = dispatch(&uploader, &store, &current, 1_000);

        assert_ne!(second.inflight_handle().attempt_id(), &first_attempt);
        assert_ne!(second.protection_sealed_upload_id(), &first_seal);
        assert_eq!(second.captured_owner(), &current);
        assert_eq!(second.inflight_handle().items()[0].key().item_id(), "run");
        assert_eq!(second.inflight_handle().items()[0].revision(), 1);
    }

    #[test]
    fn unauthorized_pauses_only_the_captured_owner_generation() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let old = owner(1, 1);
        let new = owner(1, 2);
        store
            .register_sealed_attempt(manifest(
                1,
                old.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "old", None, 1)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(manifest(
                2,
                new.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "new", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &old, 0);

        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    TraceHttpOutcome::Response(TraceHttpResponse {
                        status: 401,
                        envelope: None
                    }),
                    0,
                )
                .unwrap(),
            UploadTickOutcome::Unauthorized
        );
        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &old,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Idle
        );
        assert!(matches!(
            uploader
                .next_dispatch(
                    &store,
                    &new,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Dispatch(_)
        ));
    }

    #[test]
    fn old_generation_handle_cannot_delete_same_id_in_new_generation() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let old = owner(1, 1);
        let new = owner(1, 2);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "same-id");
        store
            .register_sealed_attempt(manifest(
                1,
                old.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "same-id", None, 1)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(manifest(
                2,
                new.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "same-id", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &old, 0);

        uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: TraceAcceptedAck {
                        runs: vec!["same-id".into()],
                        ..empty_accepted()
                    },
                    rejected: vec![],
                }),
                0,
            )
            .unwrap();

        assert_eq!(store.current_revision_for_test(&old, &key), None);
        assert_eq!(store.current_revision_for_test(&new, &key), Some(1));
    }

    #[test]
    fn upgrade_required_retires_and_pauses_without_reseal() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let same_version_other_owner = owner(2, 9);
        let newer_version_owner = owner(3, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(manifest(
                2,
                same_version_other_owner.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "same-version", None, 1)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(manifest_with_version(
                3,
                0x66,
                newer_version_owner.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "new-version", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);

        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    TraceHttpOutcome::Response(TraceHttpResponse {
                        status: 426,
                        envelope: None
                    }),
                    0,
                )
                .unwrap(),
            UploadTickOutcome::UpdateRequired
        );
        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Idle
        );
        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &same_version_other_owner,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Idle
        );
        assert!(matches!(
            uploader
                .next_dispatch(
                    &store,
                    &newer_version_owner,
                    &client_version(0x66),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Dispatch(_)
        ));
        assert!(store
            .due_reseal_items(&current, 1, &client_version(0x55))
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .due_reseal_items(&current, 1, &client_version(0x66))
                .unwrap()
                .len(),
            1
        );
        store
            .register_resealed_attempt(manifest_with_version(
                4,
                0x66,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        assert!(matches!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x66),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Dispatch(_)
        ));
    }

    #[test]
    fn accepted_parent_does_not_delete_child() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let parent = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        let child = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![
                    sealed_item(TraceSpoolEntity::Run, "run", None, 1),
                    sealed_item(TraceSpoolEntity::Event, "event", Some(parent.clone()), 1),
                ],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        let outcome = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: TraceAcceptedAck {
                        runs: vec!["run".into()],
                        ..empty_accepted()
                    },
                    rejected: vec![],
                }),
                0,
            )
            .unwrap();

        assert!(matches!(
            outcome,
            UploadTickOutcome::Uploaded {
                accepted: 1,
                stale: 0
            }
        ));
        assert_eq!(store.current_revision_for_test(&current, &parent), None);
        assert_eq!(store.current_revision_for_test(&current, &child), Some(1));
    }

    #[test]
    fn old_revision_ack_and_unacked_schedule_cannot_touch_revision_two() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        store
            .register_sealed_attempt(manifest(
                2,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 2)],
            ))
            .unwrap();

        let stale = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: TraceAcceptedAck {
                        runs: vec!["run".into()],
                        ..empty_accepted()
                    },
                    rejected: vec![],
                }),
                1,
            )
            .unwrap();
        assert_eq!(
            stale,
            UploadTickOutcome::Uploaded {
                accepted: 0,
                stale: 1
            }
        );
        assert_eq!(store.current_revision_for_test(&current, &key), Some(2));
    }

    #[test]
    fn old_rejected_or_omitted_revision_cannot_overwrite_revision_two_state() {
        for rejected in [false, true] {
            let root = TempDir::new().unwrap();
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            let current = owner(if rejected { 1 } else { 2 }, 1);
            let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
            store
                .register_sealed_attempt(manifest(
                    1,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
                ))
                .unwrap();
            let uploader = TraceUploader::new(0);
            let instruction = dispatch(&uploader, &store, &current, 0);
            store
                .register_sealed_attempt(manifest(
                    2,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 2)],
                ))
                .unwrap();

            let rejected_items = if rejected {
                vec![TraceRejectedAck {
                    entity: TraceSpoolEntity::Run,
                    id: Some("run".into()),
                    code: TraceRejectedCode::SequenceConflict,
                }]
            } else {
                vec![]
            };
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    success(TraceUploadAck {
                        ok: true,
                        accepted: empty_accepted(),
                        rejected: rejected_items,
                    }),
                    0,
                )
                .unwrap();

            assert_eq!(store.current_revision_for_test(&current, &key), Some(2));
            assert_eq!(
                store.due_attempts(&current, 0).unwrap()[0].items()[0].revision(),
                2
            );
            assert!(store
                .due_reseal_items(&current, 1_000, &client_version(0x55))
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn stale_credential_rejection_retires_the_old_attempt_without_remediation() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let chunk = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        let event = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(
                    TraceSpoolEntity::OutputChunk,
                    "chunk",
                    Some(event.clone()),
                    1,
                )],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        store
            .register_sealed_attempt(manifest(
                2,
                current.clone(),
                vec![sealed_item(
                    TraceSpoolEntity::OutputChunk,
                    "chunk",
                    Some(event),
                    2,
                )],
            ))
            .unwrap();

        let outcome = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: empty_accepted(),
                    rejected: vec![TraceRejectedAck {
                        entity: TraceSpoolEntity::OutputChunk,
                        id: Some("chunk".into()),
                        code: TraceRejectedCode::CredentialRejected,
                    }],
                }),
                1,
            )
            .unwrap();

        assert_eq!(
            outcome,
            UploadTickOutcome::Uploaded {
                accepted: 0,
                stale: 0
            }
        );
        assert_eq!(store.current_revision_for_test(&current, &chunk), Some(2));
        assert!(store
            .due_remediations(&current, &client_version(0x55))
            .unwrap()
            .is_empty());
        assert_eq!(
            store.due_attempts(&current, 1).unwrap()[0].items()[0].revision(),
            2
        );
    }

    #[test]
    fn credential_rejection_returns_only_metadata_remediation_instruction() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        let parent = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(
                    TraceSpoolEntity::OutputChunk,
                    "chunk",
                    Some(parent),
                    1,
                )],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        let outcome = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: empty_accepted(),
                    rejected: vec![TraceRejectedAck {
                        entity: TraceSpoolEntity::OutputChunk,
                        id: Some("chunk".into()),
                        code: TraceRejectedCode::CredentialRejected,
                    }],
                }),
                0,
            )
            .unwrap();

        let UploadTickOutcome::RemediationRequired(remediation) = outcome else {
            panic!("expected remediation instruction");
        };
        assert_eq!(remediation.owner(), &current);
        assert_eq!(remediation.items(), std::slice::from_ref(&key));
        assert!(store.due_attempts(&current, 1_000).unwrap().is_empty());
    }

    #[test]
    fn remediation_instruction_replays_after_reopen_until_rev_two_is_registered() {
        let root = TempDir::new().unwrap();
        let current = owner(1, 1);
        let event = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        let chunk = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        let uploader = TraceUploader::new(0);
        {
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            store
                .register_sealed_attempt(manifest(
                    1,
                    current.clone(),
                    vec![sealed_item(
                        TraceSpoolEntity::OutputChunk,
                        "chunk",
                        Some(event.clone()),
                        1,
                    )],
                ))
                .unwrap();
            let instruction = dispatch(&uploader, &store, &current, 0);
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    success(TraceUploadAck {
                        ok: true,
                        accepted: empty_accepted(),
                        rejected: vec![TraceRejectedAck {
                            entity: TraceSpoolEntity::OutputChunk,
                            id: Some("chunk".into()),
                            code: TraceRejectedCode::CredentialRejected,
                        }],
                    }),
                    0,
                )
                .unwrap();
        }

        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let DispatchTickOutcome::RemediationRequired(remediation) = uploader
            .next_dispatch(
                &reopened,
                &current,
                &client_version(0x55),
                UploadConnectivity::Online,
                1,
            )
            .unwrap()
        else {
            panic!("expected durable remediation replay");
        };
        assert_eq!(remediation.items(), std::slice::from_ref(&chunk));
        reopened
            .register_remediated_attempt(
                remediation.inflight_handle(),
                manifest(
                    2,
                    current.clone(),
                    vec![sealed_item(
                        TraceSpoolEntity::OutputChunk,
                        "chunk",
                        Some(event),
                        2,
                    )],
                ),
                remediation.items(),
            )
            .unwrap();

        assert!(matches!(
            uploader
                .next_dispatch(
                    &reopened,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1,
                )
                .unwrap(),
            DispatchTickOutcome::Dispatch(_)
        ));
    }

    #[test]
    fn mixed_200_splits_accepted_reseal_and_remediation_without_attempt_increment() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let run = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        let event = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        let credential = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "credential");
        let omitted = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "omitted");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![
                    sealed_item(TraceSpoolEntity::Run, "run", None, 1),
                    sealed_item(TraceSpoolEntity::Event, "event", Some(run.clone()), 1),
                    sealed_item(
                        TraceSpoolEntity::OutputChunk,
                        "credential",
                        Some(event.clone()),
                        1,
                    ),
                    sealed_item(
                        TraceSpoolEntity::OutputChunk,
                        "omitted",
                        Some(event.clone()),
                        1,
                    ),
                ],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        let outcome = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                success(TraceUploadAck {
                    ok: true,
                    accepted: TraceAcceptedAck {
                        runs: vec!["run".into()],
                        ..empty_accepted()
                    },
                    rejected: vec![
                        TraceRejectedAck {
                            entity: TraceSpoolEntity::Event,
                            id: Some("event".into()),
                            code: TraceRejectedCode::Invalid,
                        },
                        TraceRejectedAck {
                            entity: TraceSpoolEntity::OutputChunk,
                            id: Some("credential".into()),
                            code: TraceRejectedCode::CredentialRejected,
                        },
                    ],
                }),
                0,
            )
            .unwrap();

        let UploadTickOutcome::RemediationRequired(remediation) = outcome else {
            panic!("expected remediation instruction");
        };
        assert_eq!(remediation.items(), std::slice::from_ref(&credential));
        assert_eq!(store.current_revision_for_test(&current, &run), None);
        assert_eq!(store.current_revision_for_test(&current, &event), Some(1));
        assert_eq!(store.current_revision_for_test(&current, &omitted), Some(1));
        let reseal = store
            .due_reseal_items(&current, 1_000, &client_version(0x55))
            .unwrap();
        assert_eq!(reseal.len(), 2);
        assert!(reseal.iter().all(|item| item.attempt_count() == 0));
        assert_eq!(
            reseal
                .iter()
                .map(|item| item.item().key().clone())
                .collect::<HashSet<_>>(),
            HashSet::from([event, omitted])
        );
    }

    #[test]
    fn retryable_statuses_increment_with_bounded_exponential_backoff_and_jitter() {
        for (status, expected_retry_at) in [(429, 1_250), (503, 1_250)] {
            let root = TempDir::new().unwrap();
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            let current = owner(status as u8, 1);
            store
                .register_sealed_attempt(manifest(
                    1,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
                ))
                .unwrap();
            let uploader = TraceUploader::new(250);
            let instruction = dispatch(&uploader, &store, &current, 0);

            assert_eq!(
                uploader
                    .apply_http_outcome(
                        &store,
                        instruction.inflight_handle(),
                        TraceHttpOutcome::Response(TraceHttpResponse {
                            status,
                            envelope: None
                        }),
                        0,
                    )
                    .unwrap(),
                UploadTickOutcome::Backoff {
                    retry_at_ms: expected_retry_at
                }
            );
            assert_eq!(
                store
                    .due_reseal_items(&current, expected_retry_at, &client_version(0x55))
                    .unwrap()[0]
                    .attempt_count(),
                1
            );
        }

        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(9, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(u64::MAX);
        let instruction = dispatch(&uploader, &store, &current, 0);
        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    TraceHttpOutcome::TransportFailure,
                    0,
                )
                .unwrap(),
            UploadTickOutcome::Backoff { retry_at_ms: 2_000 }
        );
    }

    #[test]
    fn forbidden_and_manual_failures_retain_and_do_not_busy_dispatch() {
        for (status, expected) in [
            (403, UploadTickOutcome::Forbidden),
            (400, UploadTickOutcome::ManualIntervention { status: 400 }),
            (409, UploadTickOutcome::ManualIntervention { status: 409 }),
            (413, UploadTickOutcome::ManualIntervention { status: 413 }),
            (201, UploadTickOutcome::ManualIntervention { status: 201 }),
        ] {
            let root = TempDir::new().unwrap();
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            let current = owner(status as u8, 1);
            let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
            store
                .register_sealed_attempt(manifest(
                    1,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
                ))
                .unwrap();
            let uploader = TraceUploader::new(0);
            let instruction = dispatch(&uploader, &store, &current, 0);

            assert_eq!(
                uploader
                    .apply_http_outcome(
                        &store,
                        instruction.inflight_handle(),
                        TraceHttpOutcome::Response(TraceHttpResponse {
                            status,
                            envelope: None
                        }),
                        0,
                    )
                    .unwrap(),
                expected
            );
            assert_eq!(store.current_revision_for_test(&current, &key), Some(1));
            assert_eq!(
                uploader
                    .next_dispatch(
                        &store,
                        &current,
                        &client_version(0x55),
                        UploadConnectivity::Online,
                        1
                    )
                    .unwrap(),
                DispatchTickOutcome::Idle
            );
        }
    }

    #[test]
    fn malformed_200_ack_deletes_zero_and_persistently_pauses_attempt() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);

        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    success(TraceUploadAck {
                        ok: true,
                        accepted: TraceAcceptedAck {
                            runs: vec!["unknown".into()],
                            ..empty_accepted()
                        },
                        rejected: vec![],
                    }),
                    0,
                )
                .unwrap(),
            UploadTickOutcome::ManualIntervention { status: 200 }
        );
        assert_eq!(store.current_revision_for_test(&current, &key), Some(1));
        assert_eq!(
            uploader
                .next_dispatch(
                    &store,
                    &current,
                    &client_version(0x55),
                    UploadConnectivity::Online,
                    1
                )
                .unwrap(),
            DispatchTickOutcome::Idle
        );
    }

    #[test]
    fn incomplete_422_deletes_zero_and_can_request_chunk_remediation() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let parent = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        let chunk_key = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![
                    sealed_item(TraceSpoolEntity::Run, "run", None, 1),
                    sealed_item(TraceSpoolEntity::OutputChunk, "chunk", Some(parent), 1),
                ],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);
        let outcome = uploader
            .apply_http_outcome(
                &store,
                instruction.inflight_handle(),
                TraceHttpOutcome::Response(TraceHttpResponse {
                    status: 422,
                    envelope: Some(TraceHttpEnvelope::Error(TraceUploadErrorEnvelope {
                        ok: false,
                        code: TraceApiErrorCode::Incomplete,
                        details: vec![TraceRejectedAck {
                            entity: TraceSpoolEntity::OutputChunk,
                            id: Some("chunk".into()),
                            code: TraceRejectedCode::CredentialRejected,
                        }],
                    })),
                }),
                0,
            )
            .unwrap();

        let UploadTickOutcome::RemediationRequired(remediation) = outcome else {
            panic!("expected remediation instruction");
        };
        assert_eq!(remediation.items(), std::slice::from_ref(&chunk_key));
        assert_eq!(
            store.current_revision_for_test(
                &current,
                &TraceItemKey::new(TraceSpoolEntity::Run, "run")
            ),
            Some(1)
        );
        assert_eq!(
            store.current_revision_for_test(&current, &chunk_key),
            Some(1)
        );
    }

    #[test]
    fn incomplete_422_without_credentials_reseals_without_incrementing_attempt() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let instruction = dispatch(&uploader, &store, &current, 0);

        assert_eq!(
            uploader
                .apply_http_outcome(
                    &store,
                    instruction.inflight_handle(),
                    TraceHttpOutcome::Response(TraceHttpResponse {
                        status: 422,
                        envelope: Some(TraceHttpEnvelope::Error(TraceUploadErrorEnvelope {
                            ok: false,
                            code: TraceApiErrorCode::Incomplete,
                            details: vec![TraceRejectedAck {
                                entity: TraceSpoolEntity::Run,
                                id: Some("run".into()),
                                code: TraceRejectedCode::IncompleteTrace,
                            }],
                        })),
                    }),
                    0,
                )
                .unwrap(),
            UploadTickOutcome::Backoff { retry_at_ms: 1_000 }
        );
        assert_eq!(store.current_revision_for_test(&current, &key), Some(1));
        assert_eq!(
            store
                .due_reseal_items(&current, 1_000, &client_version(0x55))
                .unwrap()[0]
                .attempt_count(),
            0
        );
    }

    #[test]
    fn repeated_retryable_failures_double_and_then_cap_at_five_minutes() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);
        let mut now_ms = 0;

        for attempt in 0..10u32 {
            let instruction = dispatch(&uploader, &store, &current, now_ms);
            let delay = RETRY_BASE_MS
                .checked_shl(attempt)
                .unwrap_or(u64::MAX)
                .min(RETRY_MAX_MS);
            let retry_at_ms = now_ms + delay;
            assert_eq!(
                uploader
                    .apply_http_outcome(
                        &store,
                        instruction.inflight_handle(),
                        TraceHttpOutcome::TransportFailure,
                        now_ms,
                    )
                    .unwrap(),
                UploadTickOutcome::Backoff { retry_at_ms }
            );
            assert_eq!(
                store
                    .due_reseal_items(&current, retry_at_ms, &client_version(0x55))
                    .unwrap()[0]
                    .attempt_count(),
                attempt + 1
            );
            store
                .register_resealed_attempt(manifest(
                    (attempt + 2) as u8,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
                ))
                .unwrap();
            now_ms = retry_at_ms;
        }
    }

    #[test]
    fn invalid_422_details_fail_closed() {
        for details in [
            vec![TraceRejectedAck {
                entity: TraceSpoolEntity::OutputChunk,
                id: Some("unknown".into()),
                code: TraceRejectedCode::CredentialRejected,
            }],
            vec![
                TraceRejectedAck {
                    entity: TraceSpoolEntity::Run,
                    id: Some("run".into()),
                    code: TraceRejectedCode::Invalid,
                },
                TraceRejectedAck {
                    entity: TraceSpoolEntity::Run,
                    id: Some("run".into()),
                    code: TraceRejectedCode::Invalid,
                },
            ],
        ] {
            let root = TempDir::new().unwrap();
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            let current = owner(1, 1);
            let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
            store
                .register_sealed_attempt(manifest(
                    1,
                    current.clone(),
                    vec![sealed_item(TraceSpoolEntity::Run, "run", None, 1)],
                ))
                .unwrap();
            let uploader = TraceUploader::new(0);
            let instruction = dispatch(&uploader, &store, &current, 0);

            assert_eq!(
                uploader
                    .apply_http_outcome(
                        &store,
                        instruction.inflight_handle(),
                        TraceHttpOutcome::Response(TraceHttpResponse {
                            status: 422,
                            envelope: Some(TraceHttpEnvelope::Error(TraceUploadErrorEnvelope {
                                ok: false,
                                code: TraceApiErrorCode::Incomplete,
                                details,
                            })),
                        }),
                        0,
                    )
                    .unwrap(),
                UploadTickOutcome::ManualIntervention { status: 422 }
            );
            assert_eq!(store.current_revision_for_test(&current, &key), Some(1));
        }
    }

    #[test]
    fn stale_pending_attempt_is_never_dispatched_after_item_revision_advances() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(manifest(
                1,
                current.clone(),
                vec![
                    sealed_item(TraceSpoolEntity::Run, "run", None, 1),
                    sealed_item(
                        TraceSpoolEntity::Event,
                        "event",
                        Some(TraceItemKey::new(TraceSpoolEntity::Run, "run")),
                        1,
                    ),
                ],
            ))
            .unwrap();
        store
            .register_sealed_attempt(manifest(
                2,
                current.clone(),
                vec![sealed_item(TraceSpoolEntity::Run, "run", None, 2)],
            ))
            .unwrap();
        let uploader = TraceUploader::new(0);

        let instruction = dispatch(&uploader, &store, &current, 0);
        assert_eq!(instruction.inflight_handle().items().len(), 1);
        assert_eq!(instruction.inflight_handle().items()[0].revision(), 2);
        let reseal = store
            .due_reseal_items(&current, 0, &client_version(0x55))
            .unwrap();
        assert_eq!(reseal.len(), 1);
        assert_eq!(reseal[0].item().key().item_id(), "event");
    }
}
