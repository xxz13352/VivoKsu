//! Crate-private metadata-only bridge from protection-sealed uploads to the durable spool.
//!
//! Export is intentionally blocked until the login/session layer can mint one
//! concrete receipt that binds a verified `SessionLease`, canonical username
//! hash, numeric login generation, and compiled build identity atomically.
//!
//! This facade does **not** solve durable HTTP replay. The attested body and
//! its `RegisteredSealedAttempt` capability remain live-memory-only, while the
//! disk spool stores metadata. After process restart there is no public claim
//! path without that exact live capability. An orphan sweep and durable loss
//! tombstone are also not implemented, so restart recovery remains a P0
//! blocker rather than a completed recovery or loss-accounting guarantee.
//!
//! Production wiring is also blocked until protection exposes an attested
//! run-record path, the legacy crate-internal metadata-only claim seam is
//! retired, and completed-attempt retention is bounded without losing replay
//! or loss-accounting evidence. `AuthenticatedTraceScope` therefore has no
//! production constructor in this checkpoint.

#![allow(
    dead_code,
    reason = "crate-private seam awaiting a signed-login trace scope adapter"
)]

use crate::trace_spool::{
    DueSealedAttempt, InflightAttemptHandle, ProtectionClientVersionHash, ProtectionSealedUploadId,
    SealedAttemptId, SealedAttemptManifest, SealedItemRevision, TraceItemKey, TraceOwnerGeneration,
    TraceSpoolEntity, TraceSpoolStore,
};
use crate::trace_uploader::{
    validate_success_ack, TraceAcceptedAck as InternalAcceptedAck,
    TraceRejectedAck as InternalRejectedAck, TraceRejectedCode as InternalRejectedCode,
    TraceUploadAck as InternalUploadAck, SERVER_UNACKED_DELAY_MS,
};
use nwflash_domain::{
    TraceId, TRACE_UPLOAD_MAX_BODY_BYTES, TRACE_UPLOAD_MAX_EVENTS, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS,
    TRACE_UPLOAD_MAX_RUNS,
};
use nwflash_protection::{
    SealedTraceMetadataEntity, SealedTraceMetadataKey, SealedTraceMetadataView,
    SentinelAttestedTraceUpload,
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

const TRACE_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
const ATTEMPT_DOMAIN: &[u8] = b"nwflash-trace-attempt-v1\0";

/// Authenticated account identity captured at login admission time.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct AuthenticatedTraceOwner {
    username_hash: [u8; 32],
    login_generation: u64,
}

impl AuthenticatedTraceOwner {
    #[cfg(test)]
    fn try_new(
        canonical_username_hash: [u8; 32],
        login_generation: u64,
    ) -> Result<Self, TraceFacadeError> {
        if canonical_username_hash == [0; 32] || login_generation == 0 {
            return Err(TraceFacadeError::InvalidOwner);
        }
        Ok(Self {
            username_hash: canonical_username_hash,
            login_generation,
        })
    }

    pub fn canonical_username_hash(&self) -> &[u8; 32] {
        &self.username_hash
    }

    pub fn login_generation(&self) -> u64 {
        self.login_generation
    }

    fn spool_owner(&self) -> TraceOwnerGeneration {
        TraceOwnerGeneration::from_canonical_username_hash(
            self.username_hash,
            self.login_generation,
        )
    }
}

/// Opaque owner/version admission captured from one authenticated login.
///
/// There is deliberately no crate-visible raw constructor. The production
/// facade stays unreachable until the signed-login adapter can mint this type
/// from one verified lease, canonical username and compiled client identity.
pub(crate) struct AuthenticatedTraceScope {
    owner: AuthenticatedTraceOwner,
    spool_owner: TraceOwnerGeneration,
    client_version_hash: ProtectionClientVersionHash,
}

impl AuthenticatedTraceScope {
    #[cfg(test)]
    fn try_new(
        owner: AuthenticatedTraceOwner,
        client_version_hash: [u8; 32],
    ) -> Result<Self, TraceFacadeError> {
        if client_version_hash == [0; 32] {
            return Err(TraceFacadeError::InvalidClientVersion);
        }
        let spool_owner = owner.spool_owner();
        Ok(Self {
            owner,
            spool_owner,
            client_version_hash: ProtectionClientVersionHash::from_digest(client_version_hash),
        })
    }
}

impl fmt::Debug for AuthenticatedTraceScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedTraceScope")
            .field("owner", &self.owner)
            .field("client_version_hash", &self.client_version_hash)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AuthenticatedTraceOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthenticatedTraceOwner")
            .field("login_generation", &self.login_generation)
            .finish_non_exhaustive()
    }
}

/// The only caller-supplied per-item data. Entity, item ID and parent links are
/// reconstructed from the concrete sealed upload and cannot be overridden.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceItemRevisionInput {
    item_id: TraceId,
    trace_id: TraceId,
    revision: u64,
    created_at_ms: u64,
}

impl TraceItemRevisionInput {
    pub fn try_new(
        item_id: TraceId,
        trace_id: TraceId,
        revision: u64,
        created_at_ms: u64,
    ) -> Result<Self, TraceFacadeError> {
        if revision == 0
            || revision > TRACE_SAFE_INTEGER_MAX
            || created_at_ms > TRACE_SAFE_INTEGER_MAX
        {
            return Err(TraceFacadeError::InvalidRevisionManifest);
        }
        Ok(Self {
            item_id,
            trace_id,
            revision,
            created_at_ms,
        })
    }

    pub fn item_id(&self) -> TraceId {
        self.item_id
    }

    pub fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceRevisionManifest {
    items: Vec<TraceItemRevisionInput>,
}

impl TraceRevisionManifest {
    pub fn try_new(items: Vec<TraceItemRevisionInput>) -> Result<Self, TraceFacadeError> {
        if items.is_empty()
            || items.len()
                > TRACE_UPLOAD_MAX_RUNS + TRACE_UPLOAD_MAX_EVENTS + TRACE_UPLOAD_MAX_OUTPUT_CHUNKS
        {
            return Err(TraceFacadeError::InvalidRevisionManifest);
        }
        let unique = items
            .iter()
            .map(|item| item.item_id)
            .collect::<HashSet<_>>();
        if unique.len() != items.len() {
            return Err(TraceFacadeError::InvalidRevisionManifest);
        }
        Ok(Self { items })
    }

    pub fn items(&self) -> &[TraceItemRevisionInput] {
        &self.items
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TraceManifestEntity {
    Run,
    Event,
    OutputChunk,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TraceManifestKey {
    entity: TraceManifestEntity,
    item_id: TraceId,
}

impl TraceManifestKey {
    pub fn entity(&self) -> TraceManifestEntity {
        self.entity
    }

    pub fn item_id(&self) -> TraceId {
        self.item_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceManifestItem {
    key: TraceManifestKey,
    trace_id: TraceId,
    parent: Option<TraceManifestKey>,
    revision: u64,
    created_at_ms: u64,
}

impl TraceManifestItem {
    pub fn key(&self) -> &TraceManifestKey {
        &self.key
    }

    pub fn trace_id(&self) -> TraceId {
        self.trace_id
    }

    pub fn parent(&self) -> Option<&TraceManifestKey> {
        self.parent.as_ref()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct OpaqueSealedUploadIdentity(ProtectionSealedUploadId);

impl fmt::Debug for OpaqueSealedUploadIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueSealedUploadIdentity([opaque])")
    }
}

impl OpaqueSealedUploadIdentity {
    pub fn matches(&self, upload: &SentinelAttestedTraceUpload) -> Result<bool, TraceFacadeError> {
        let view = upload
            .metadata_view()
            .map_err(|_| TraceFacadeError::InvalidSealedUpload)?;
        Ok(self.0 == ProtectionSealedUploadId::from_digest(*view.body_sha256()))
    }
}

#[derive(Clone, Eq, Hash, PartialEq)]
pub struct OpaqueTraceAttemptIdentity(SealedAttemptId);

impl fmt::Debug for OpaqueTraceAttemptIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OpaqueTraceAttemptIdentity([opaque])")
    }
}

/// Live-only capability binding one exact attested body to one persisted
/// metadata attempt. It intentionally has no `Clone`, serialization, or
/// deserialization path and is required again before the spool may mutate a
/// pending attempt into `Inflight`.
pub struct RegisteredSealedAttempt {
    upload_id: TraceId,
    attempt_identity: OpaqueTraceAttemptIdentity,
    sealed_upload_identity: OpaqueSealedUploadIdentity,
    owner: AuthenticatedTraceOwner,
    client_version_hash: ProtectionClientVersionHash,
    wire_bytes: usize,
    items: Vec<TraceManifestItem>,
}

impl RegisteredSealedAttempt {
    pub fn upload_id(&self) -> TraceId {
        self.upload_id
    }

    pub fn attempt_identity(&self) -> &OpaqueTraceAttemptIdentity {
        &self.attempt_identity
    }

    pub fn sealed_upload_identity(&self) -> &OpaqueSealedUploadIdentity {
        &self.sealed_upload_identity
    }

    pub fn owner(&self) -> &AuthenticatedTraceOwner {
        &self.owner
    }

    pub fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }

    pub fn items(&self) -> &[TraceManifestItem] {
        &self.items
    }
}

impl fmt::Debug for RegisteredSealedAttempt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredSealedAttempt")
            .field("upload_id", &self.upload_id)
            .field("attempt_identity", &self.attempt_identity)
            .field("sealed_upload_identity", &self.sealed_upload_identity)
            .field("owner", &self.owner)
            .field("client_version_hash", &self.client_version_hash)
            .field("wire_bytes", &self.wire_bytes)
            .field("item_count", &self.items.len())
            .finish()
    }
}

pub struct TraceDispatchInstruction {
    handle: InflightAttemptHandle,
    attempt_identity: OpaqueTraceAttemptIdentity,
    sealed_upload_identity: OpaqueSealedUploadIdentity,
    owner: AuthenticatedTraceOwner,
    client_version_hash: ProtectionClientVersionHash,
    wire_bytes: usize,
    items: Vec<TraceManifestItem>,
}

impl TraceDispatchInstruction {
    pub fn attempt_identity(&self) -> &OpaqueTraceAttemptIdentity {
        &self.attempt_identity
    }

    pub fn sealed_upload_identity(&self) -> &OpaqueSealedUploadIdentity {
        &self.sealed_upload_identity
    }

    pub fn owner(&self) -> &AuthenticatedTraceOwner {
        &self.owner
    }

    pub fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }

    pub fn items(&self) -> &[TraceManifestItem] {
        &self.items
    }
}

impl fmt::Debug for TraceDispatchInstruction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceDispatchInstruction")
            .field("attempt_identity", &self.attempt_identity)
            .field("sealed_upload_identity", &self.sealed_upload_identity)
            .field("owner", &self.owner)
            .field("client_version_hash", &self.client_version_hash)
            .field("wire_bytes", &self.wire_bytes)
            .field("item_count", &self.items.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TraceRevisionCasResult {
    matched_items: usize,
    stale_items: usize,
}

impl TraceRevisionCasResult {
    pub fn matched_items(&self) -> usize {
        self.matched_items
    }

    pub fn stale_items(&self) -> usize {
        self.stale_items
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceRejectedCode {
    Invalid,
    MissingParent,
    SequenceConflict,
    IncompleteTrace,
    CredentialRejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceRejectedAck {
    key: TraceManifestKey,
    code: TraceRejectedCode,
}

impl TraceRejectedAck {
    pub fn new(key: TraceManifestKey, code: TraceRejectedCode) -> Self {
        Self { key, code }
    }

    pub fn key(&self) -> &TraceManifestKey {
        &self.key
    }

    pub fn code(&self) -> TraceRejectedCode {
        self.code
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceFullAck {
    accepted: Vec<TraceManifestKey>,
    rejected: Vec<TraceRejectedAck>,
}

impl TraceFullAck {
    pub fn new(accepted: Vec<TraceManifestKey>, rejected: Vec<TraceRejectedAck>) -> Self {
        Self { accepted, rejected }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceAckApplyResult {
    accepted: TraceRevisionCasResult,
    rejected: Vec<TraceManifestKey>,
    unacknowledged: Vec<TraceManifestKey>,
    credential_remediation: Vec<TraceManifestKey>,
}

impl TraceAckApplyResult {
    pub fn accepted(&self) -> &TraceRevisionCasResult {
        &self.accepted
    }

    pub fn rejected(&self) -> &[TraceManifestKey] {
        &self.rejected
    }

    pub fn unacknowledged(&self) -> &[TraceManifestKey] {
        &self.unacknowledged
    }

    pub fn credential_remediation(&self) -> &[TraceManifestKey] {
        &self.credential_remediation
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum TraceFacadeError {
    #[error("trace owner scope is invalid")]
    InvalidOwner,
    #[error("trace client version identity is invalid")]
    InvalidClientVersion,
    #[error("trace revision manifest is invalid")]
    InvalidRevisionManifest,
    #[error("protection-sealed trace upload is invalid")]
    InvalidSealedUpload,
    #[error("protection-sealed trace upload does not match the pending attempt")]
    SealMismatch,
    #[error("fresh trace attempt identity could not be generated")]
    AttemptIdentityGeneration,
    #[error("trace acknowledgement is invalid")]
    InvalidAck,
    #[error("trace metadata spool operation failed")]
    Storage,
}

/// Metadata-only crate-private facade. The body remains owned by
/// [`SentinelAttestedTraceUpload`] in caller memory; only opaque digests, identities and
/// revision snapshots cross into the durable spool.
///
/// Persisted digest constructors remain inaccessible to external crates:
///
/// ```compile_fail
/// use nwflash_infrastructure::trace_spool::ProtectionSealedUploadId;
/// ```
pub(crate) struct TraceSpoolFacade {
    store: TraceSpoolStore,
    owner: AuthenticatedTraceOwner,
    spool_owner: TraceOwnerGeneration,
    client_version_hash: ProtectionClientVersionHash,
}

impl TraceSpoolFacade {
    pub(crate) fn open(
        root: impl AsRef<Path>,
        scope: AuthenticatedTraceScope,
    ) -> Result<Self, TraceFacadeError> {
        let store = TraceSpoolStore::open(root.as_ref().to_path_buf())
            .map_err(|_| TraceFacadeError::Storage)?;
        Ok(Self {
            store,
            owner: scope.owner,
            spool_owner: scope.spool_owner,
            client_version_hash: scope.client_version_hash,
        })
    }

    pub fn owner(&self) -> &AuthenticatedTraceOwner {
        &self.owner
    }

    pub fn register_sealed_upload(
        &self,
        upload: &SentinelAttestedTraceUpload,
        revisions: &TraceRevisionManifest,
    ) -> Result<RegisteredSealedAttempt, TraceFacadeError> {
        let view = upload
            .metadata_view()
            .map_err(|_| TraceFacadeError::InvalidSealedUpload)?;
        if view.upload_id() != upload.upload_id()
            || view.body_len() == 0
            || view.body_len() > TRACE_UPLOAD_MAX_BODY_BYTES
            || view.run_count() > TRACE_UPLOAD_MAX_RUNS
            || view.event_count() > TRACE_UPLOAD_MAX_EVENTS
            || view.chunk_count() > TRACE_UPLOAD_MAX_OUTPUT_CHUNKS
        {
            return Err(TraceFacadeError::InvalidSealedUpload);
        }
        let items = manifest_items(&view, revisions)?;
        let spool_items = items
            .iter()
            .map(spool_item)
            .collect::<Result<Vec<_>, _>>()?;
        let sealed_digest = *view.body_sha256();
        let sealed_upload_identity =
            OpaqueSealedUploadIdentity(ProtectionSealedUploadId::from_digest(sealed_digest));
        let attempt_identity =
            fresh_attempt_identity(sealed_digest, &self.owner, upload.upload_id())?;
        let wire_bytes = view.body_len();
        let manifest = SealedAttemptManifest::new(
            attempt_identity.0.clone(),
            self.spool_owner.clone(),
            sealed_upload_identity.0.clone(),
            self.client_version_hash.clone(),
            u64::try_from(wire_bytes).map_err(|_| TraceFacadeError::InvalidSealedUpload)?,
            u16::try_from(view.run_count()).map_err(|_| TraceFacadeError::InvalidSealedUpload)?,
            u16::try_from(view.event_count()).map_err(|_| TraceFacadeError::InvalidSealedUpload)?,
            u16::try_from(view.chunk_count()).map_err(|_| TraceFacadeError::InvalidSealedUpload)?,
            spool_items,
        );
        self.store
            .register_sealed_attempt(manifest)
            .map_err(|_| TraceFacadeError::Storage)?;
        Ok(RegisteredSealedAttempt {
            upload_id: upload.upload_id(),
            attempt_identity,
            sealed_upload_identity,
            owner: self.owner.clone(),
            client_version_hash: self.client_version_hash.clone(),
            wire_bytes,
            items,
        })
    }

    pub fn begin_next_dispatch(
        &self,
        registered: &RegisteredSealedAttempt,
        upload: &SentinelAttestedTraceUpload,
        now_ms: u64,
    ) -> Result<Option<TraceDispatchInstruction>, TraceFacadeError> {
        if now_ms > TRACE_SAFE_INTEGER_MAX {
            return Err(TraceFacadeError::Storage);
        }
        let view = upload
            .metadata_view()
            .map_err(|_| TraceFacadeError::InvalidSealedUpload)?;
        if registered.owner != self.owner
            || registered.client_version_hash != self.client_version_hash
            || registered.upload_id != view.upload_id()
            || registered.wire_bytes != view.body_len()
            || registered.sealed_upload_identity.0
                != ProtectionSealedUploadId::from_digest(*view.body_sha256())
        {
            return Err(TraceFacadeError::SealMismatch);
        }
        let Some(candidate) = self
            .store
            .peek_due_attempts(&self.spool_owner, now_ms)
            .map_err(|_| TraceFacadeError::Storage)?
            .into_iter()
            .find(|attempt| attempt.attempt_id() == &registered.attempt_identity.0)
        else {
            return Ok(None);
        };
        if !seal_matches_due(
            &view,
            &candidate,
            registered,
            &self.spool_owner,
            &self.client_version_hash,
        )? {
            return Err(TraceFacadeError::SealMismatch);
        }

        // Retention is a state transition, so it occurs only after the exact
        // concrete seal has been authenticated against the read-only snapshot.
        self.store
            .expire(&self.spool_owner, now_ms)
            .map_err(|_| TraceFacadeError::Storage)?;
        let Some(due) = self
            .store
            .peek_due_attempts(&self.spool_owner, now_ms)
            .map_err(|_| TraceFacadeError::Storage)?
            .into_iter()
            .find(|attempt| attempt.attempt_id() == candidate.attempt_id())
        else {
            return Ok(None);
        };
        if !seal_matches_due(
            &view,
            &due,
            registered,
            &self.spool_owner,
            &self.client_version_hash,
        )? {
            return Err(TraceFacadeError::SealMismatch);
        }
        let handle = self
            .store
            .begin_dispatch(due.attempt_id(), &self.spool_owner)
            .map_err(|_| TraceFacadeError::Storage)?;
        let items = handle
            .items()
            .iter()
            .map(public_item)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some(TraceDispatchInstruction {
            attempt_identity: OpaqueTraceAttemptIdentity(handle.attempt_id().clone()),
            sealed_upload_identity: OpaqueSealedUploadIdentity(handle.sealed_upload_id().clone()),
            owner: self.owner.clone(),
            client_version_hash: handle.client_version_hash().clone(),
            wire_bytes: usize::try_from(handle.wire_bytes())
                .map_err(|_| TraceFacadeError::Storage)?,
            items,
            handle,
        }))
    }

    pub fn apply_validated_ack(
        &self,
        instruction: &TraceDispatchInstruction,
        ack: TraceFullAck,
        now_ms: u64,
    ) -> Result<TraceAckApplyResult, TraceFacadeError> {
        if instruction.owner != self.owner {
            return Err(TraceFacadeError::InvalidOwner);
        }
        if instruction.handle.owner() != &self.spool_owner {
            return Err(TraceFacadeError::InvalidOwner);
        }
        if instruction.client_version_hash != self.client_version_hash
            || instruction.handle.client_version_hash() != &self.client_version_hash
        {
            return Err(TraceFacadeError::InvalidClientVersion);
        }
        if !instruction_matches_handle(instruction)? {
            return Err(TraceFacadeError::InvalidAck);
        }
        let next_attempt_at_ms = now_ms
            .checked_add(SERVER_UNACKED_DELAY_MS)
            .filter(|next| *next <= TRACE_SAFE_INTEGER_MAX)
            .ok_or(TraceFacadeError::InvalidAck)?;
        let mut accepted = InternalAcceptedAck::default();
        for key in ack.accepted {
            match key.entity {
                TraceManifestEntity::Run => accepted.runs.push(key.item_id.to_string()),
                TraceManifestEntity::Event => accepted.events.push(key.item_id.to_string()),
                TraceManifestEntity::OutputChunk => {
                    accepted.output_chunks.push(key.item_id.to_string())
                }
            }
        }
        let rejected = ack
            .rejected
            .into_iter()
            .map(|item| InternalRejectedAck {
                entity: spool_entity(item.key.entity),
                id: Some(item.key.item_id.to_string()),
                code: internal_rejected_code(item.code),
            })
            .collect();
        let dispatched = instruction
            .handle
            .items()
            .iter()
            .map(|item| item.key().clone())
            .collect::<Vec<_>>();
        let validated = validate_success_ack(
            &dispatched,
            &InternalUploadAck {
                ok: true,
                accepted,
                rejected,
            },
        )
        .map_err(|_| TraceFacadeError::InvalidAck)?;
        let rejected = validated
            .rejected()
            .iter()
            .map(public_key)
            .collect::<Result<Vec<_>, _>>()?;
        let unacknowledged = validated
            .unacknowledged()
            .iter()
            .map(public_key)
            .collect::<Result<Vec<_>, _>>()?;
        let transition = self
            .store
            .apply_validated_ack_cas(
                &instruction.handle,
                validated.accepted(),
                validated.rejected(),
                validated.unacknowledged(),
                validated.credential_rejected(),
                next_attempt_at_ms,
            )
            .map_err(|_| TraceFacadeError::Storage)?;
        let credential_remediation = transition
            .remediation()
            .iter()
            .map(public_key)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(TraceAckApplyResult {
            accepted: TraceRevisionCasResult {
                matched_items: transition.accepted().matched_items(),
                stale_items: transition.accepted().stale_items(),
            },
            rejected,
            unacknowledged,
            credential_remediation,
        })
    }
}

impl fmt::Debug for TraceSpoolFacade {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceSpoolFacade")
            .field("owner", &self.owner)
            .finish_non_exhaustive()
    }
}

fn seal_matches_due(
    view: &SealedTraceMetadataView,
    due: &DueSealedAttempt,
    registered: &RegisteredSealedAttempt,
    owner: &TraceOwnerGeneration,
    client_version_hash: &ProtectionClientVersionHash,
) -> Result<bool, TraceFacadeError> {
    if registered.upload_id != view.upload_id()
        || registered.owner.spool_owner() != *owner
        || registered.client_version_hash != *client_version_hash
        || registered.attempt_identity.0 != *due.attempt_id()
        || registered.sealed_upload_identity.0
            != ProtectionSealedUploadId::from_digest(*view.body_sha256())
        || registered.wire_bytes != view.body_len()
        || registered.items.len() != view.items().len()
        || due.owner() != owner
        || due.client_version_hash() != client_version_hash
        || due.sealed_upload_id() != &registered.sealed_upload_identity.0
        || due.wire_bytes()
            != u64::try_from(view.body_len()).map_err(|_| TraceFacadeError::InvalidSealedUpload)?
        || usize::from(due.run_count()) != view.run_count()
        || usize::from(due.event_count()) != view.event_count()
        || usize::from(due.chunk_count()) != view.chunk_count()
        || due.items().len() != view.items().len()
    {
        return Ok(false);
    }
    let registered_items = registered
        .items
        .iter()
        .map(spool_item)
        .collect::<Result<Vec<_>, _>>()?;
    if due.items() != registered_items {
        return Ok(false);
    }
    Ok(view.items().iter().all(|sealed| {
        registered.items.iter().any(|item| {
            item.key == public_sealed_key(sealed.key())
                && item.trace_id == sealed.trace_id()
                && item.parent == sealed.parent().map(public_sealed_key)
                && item.created_at_ms == sealed.created_at_ms()
                && item.revision > 0
        })
    }))
}

fn fresh_attempt_identity(
    sealed_upload_digest: [u8; 32],
    owner: &AuthenticatedTraceOwner,
    upload_id: TraceId,
) -> Result<OpaqueTraceAttemptIdentity, TraceFacadeError> {
    let mut random = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| TraceFacadeError::AttemptIdentityGeneration)?;
    let mut digest = Sha256::new();
    digest.update(ATTEMPT_DOMAIN);
    digest.update(random);
    digest.update(owner.username_hash);
    digest.update(owner.login_generation.to_le_bytes());
    digest.update(upload_id.to_string().as_bytes());
    digest.update(sealed_upload_digest);
    Ok(OpaqueTraceAttemptIdentity(SealedAttemptId::from_digest(
        digest.finalize().into(),
    )))
}

fn instruction_matches_handle(
    instruction: &TraceDispatchInstruction,
) -> Result<bool, TraceFacadeError> {
    if instruction.attempt_identity.0 != *instruction.handle.attempt_id()
        || instruction.sealed_upload_identity.0 != *instruction.handle.sealed_upload_id()
        || u64::try_from(instruction.wire_bytes).map_err(|_| TraceFacadeError::InvalidAck)?
            != instruction.handle.wire_bytes()
        || instruction.items.len() != instruction.handle.items().len()
    {
        return Ok(false);
    }
    let items = instruction
        .items
        .iter()
        .map(spool_item)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(items == instruction.handle.items())
}

fn manifest_items(
    view: &SealedTraceMetadataView,
    revisions: &TraceRevisionManifest,
) -> Result<Vec<TraceManifestItem>, TraceFacadeError> {
    let mut revisions = revisions
        .items
        .iter()
        .map(|item| (item.item_id, *item))
        .collect::<HashMap<_, _>>();
    let mut items = Vec::with_capacity(view.items().len());
    for sealed in view.items() {
        let revision = take_revision(&mut revisions, sealed.item_id())?;
        if revision.trace_id != sealed.trace_id()
            || revision.created_at_ms != sealed.created_at_ms()
        {
            return Err(TraceFacadeError::InvalidRevisionManifest);
        }
        items.push(TraceManifestItem {
            key: public_sealed_key(sealed.key()),
            trace_id: revision.trace_id,
            parent: sealed.parent().map(public_sealed_key),
            revision: revision.revision,
            created_at_ms: revision.created_at_ms,
        });
    }
    if !revisions.is_empty() || items.len() != view.items().len() {
        return Err(TraceFacadeError::InvalidRevisionManifest);
    }
    let unique = items
        .iter()
        .map(|item| item.key.item_id)
        .collect::<HashSet<_>>();
    if unique.len() != items.len() {
        return Err(TraceFacadeError::InvalidRevisionManifest);
    }
    Ok(items)
}

fn public_sealed_key(key: &SealedTraceMetadataKey) -> TraceManifestKey {
    TraceManifestKey {
        entity: match key.entity() {
            SealedTraceMetadataEntity::Run => TraceManifestEntity::Run,
            SealedTraceMetadataEntity::Event => TraceManifestEntity::Event,
            SealedTraceMetadataEntity::OutputChunk => TraceManifestEntity::OutputChunk,
        },
        item_id: key.item_id(),
    }
}

fn take_revision(
    revisions: &mut HashMap<TraceId, TraceItemRevisionInput>,
    item_id: TraceId,
) -> Result<TraceItemRevisionInput, TraceFacadeError> {
    revisions
        .remove(&item_id)
        .ok_or(TraceFacadeError::InvalidRevisionManifest)
}

fn spool_item(item: &TraceManifestItem) -> Result<SealedItemRevision, TraceFacadeError> {
    Ok(SealedItemRevision::new(
        spool_key(&item.key),
        item.trace_id.to_string(),
        item.parent.as_ref().map(spool_key),
        item.revision,
        item.created_at_ms,
    ))
}

fn spool_key(key: &TraceManifestKey) -> TraceItemKey {
    TraceItemKey::new(spool_entity(key.entity), key.item_id.to_string())
}

fn spool_entity(entity: TraceManifestEntity) -> TraceSpoolEntity {
    match entity {
        TraceManifestEntity::Run => TraceSpoolEntity::Run,
        TraceManifestEntity::Event => TraceSpoolEntity::Event,
        TraceManifestEntity::OutputChunk => TraceSpoolEntity::OutputChunk,
    }
}

fn internal_rejected_code(code: TraceRejectedCode) -> InternalRejectedCode {
    match code {
        TraceRejectedCode::Invalid => InternalRejectedCode::Invalid,
        TraceRejectedCode::MissingParent => InternalRejectedCode::MissingParent,
        TraceRejectedCode::SequenceConflict => InternalRejectedCode::SequenceConflict,
        TraceRejectedCode::IncompleteTrace => InternalRejectedCode::IncompleteTrace,
        TraceRejectedCode::CredentialRejected => InternalRejectedCode::CredentialRejected,
    }
}

fn public_item(item: &SealedItemRevision) -> Result<TraceManifestItem, TraceFacadeError> {
    Ok(TraceManifestItem {
        key: public_key(item.key())?,
        trace_id: item
            .trace_id()
            .parse()
            .map_err(|_| TraceFacadeError::Storage)?,
        parent: item.parent().map(public_key).transpose()?,
        revision: item.revision(),
        created_at_ms: item.created_at_ms(),
    })
}

fn public_key(key: &TraceItemKey) -> Result<TraceManifestKey, TraceFacadeError> {
    let entity = match key.entity() {
        TraceSpoolEntity::Run => TraceManifestEntity::Run,
        TraceSpoolEntity::Event => TraceManifestEntity::Event,
        TraceSpoolEntity::OutputChunk => TraceManifestEntity::OutputChunk,
    };
    Ok(TraceManifestKey {
        entity,
        item_id: key
            .item_id()
            .parse()
            .map_err(|_| TraceFacadeError::Storage)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nwflash_domain::{TraceEventKindV2, TraceEventStatusV2, TraceId, TraceOutputStreamV2};
    use nwflash_protection::{
        ExactSecretSet, SentinelAttestedTraceUpload, TraceEventText, TraceOutputSession,
    };
    use std::fs;
    use std::io::Cursor;
    use tempfile::TempDir;

    const BODY_SENTINEL: &str = "sealed-body-must-never-reach-spool";

    fn open_facade(
        root: &std::path::Path,
        owner: AuthenticatedTraceOwner,
        client_version_hash: [u8; 32],
    ) -> Result<TraceSpoolFacade, TraceFacadeError> {
        let scope = AuthenticatedTraceScope::try_new(owner, client_version_hash)?;
        TraceSpoolFacade::open(root, scope)
    }

    fn sealed_event_upload(run_id: TraceId, event_id: TraceId) -> SentinelAttestedTraceUpload {
        let mut output = Cursor::new(BODY_SENTINEL.as_bytes());
        let session = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut output,
            &ExactSecretSet::empty(),
        )
        .expect("complete output must seal");
        let upload = session
            .into_event_upload_attempts(
                TraceEventText {
                    event_id,
                    run_id,
                    sequence: 1,
                    kind: TraceEventKindV2::Command,
                    step_name: "flash",
                    partition_name: None,
                    status: TraceEventStatusV2::Success,
                    started_at_ms: 11,
                    ended_at_ms: Some(12),
                    duration_ms: Some(1),
                    command: None,
                    exit_code: Some(0),
                    verification: None,
                    device_state: None,
                    retry_safe: Some(true),
                    remedies: &[],
                    error_class: None,
                    error_code: None,
                    error_message: None,
                },
                &ExactSecretSet::empty(),
            )
            .expect("bounded event attempts")
            .into_iter()
            .next()
            .expect("event manifest attempt");
        SentinelAttestedTraceUpload::try_from(upload).expect("sentinel-attested upload")
    }

    fn revisions_for(
        upload: &SentinelAttestedTraceUpload,
        event_revision: u64,
    ) -> TraceRevisionManifest {
        let view = upload.metadata_view().expect("attested metadata view");
        TraceRevisionManifest::try_new(
            view.items()
                .iter()
                .map(|item| {
                    let revision = if item.entity() == SealedTraceMetadataEntity::Event {
                        event_revision
                    } else {
                        1
                    };
                    TraceItemRevisionInput::try_new(
                        item.item_id(),
                        item.trace_id(),
                        revision,
                        item.created_at_ms(),
                    )
                    .expect("bounded revision")
                })
                .collect(),
        )
        .expect("exact attested manifest")
    }

    fn persisted_bytes(root: &std::path::Path) -> Vec<u8> {
        let mut bytes = Vec::new();
        for entry in fs::read_dir(root).expect("spool root") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                bytes.extend(persisted_bytes(&path));
            } else {
                bytes.extend(fs::read(path).expect("spool file"));
            }
        }
        bytes
    }

    #[test]
    fn sealed_upload_registration_persists_only_metadata() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let owner = AuthenticatedTraceOwner::try_new([0x11; 32], 7).expect("owner scope");
        let facade = open_facade(root.path(), owner, [0x22; 32]).expect("safe spool root");
        let revisions = revisions_for(&upload, 1);

        let registered = facade
            .register_sealed_upload(&upload, &revisions)
            .expect("metadata registration");

        assert_eq!(registered.items().len(), 2);
        assert!(!format!("{facade:?}{registered:?}").contains(BODY_SENTINEL));
        assert!(!persisted_bytes(root.path())
            .windows(BODY_SENTINEL.len())
            .any(|window| window == BODY_SENTINEL.as_bytes()));
    }

    #[test]
    fn dispatch_identity_is_fresh_owner_bound_and_stable_for_the_same_seal() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let revisions = revisions_for(&upload, 1);
        let owner_a = AuthenticatedTraceOwner::try_new([0x31; 32], 3).expect("owner A");
        let owner_b = AuthenticatedTraceOwner::try_new([0x41; 32], 4).expect("owner B");
        let facade_a =
            open_facade(root.path(), owner_a.clone(), [0x51; 32]).expect("owner A spool");
        let facade_b =
            open_facade(root.path(), owner_b.clone(), [0x51; 32]).expect("owner B spool");

        let registered_a = facade_a
            .register_sealed_upload(&upload, &revisions)
            .expect("owner A registration");
        let registered_b = facade_b
            .register_sealed_upload(&upload, &revisions)
            .expect("owner B registration");

        assert_eq!(
            registered_a.sealed_upload_identity(),
            registered_b.sealed_upload_identity()
        );
        assert_ne!(
            registered_a.attempt_identity(),
            registered_b.attempt_identity()
        );
        assert_eq!(registered_a.items(), registered_b.items());
        let dispatch_a = facade_a
            .begin_next_dispatch(&registered_a, &upload, 0)
            .expect("dispatch query")
            .expect("owner A dispatch");
        let dispatch_b = facade_b
            .begin_next_dispatch(&registered_b, &upload, 0)
            .expect("dispatch query")
            .expect("owner B dispatch");
        assert_eq!(dispatch_a.owner(), &owner_a);
        assert_eq!(dispatch_b.owner(), &owner_b);
        assert_eq!(
            dispatch_a.sealed_upload_identity(),
            registered_a.sealed_upload_identity()
        );
        assert_eq!(dispatch_a.items(), registered_a.items());
        assert!(dispatch_a
            .sealed_upload_identity()
            .matches(&upload)
            .expect("sealed identity comparison"));
    }

    #[test]
    fn delayed_ack_cannot_delete_a_newer_item_revision() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let owner = AuthenticatedTraceOwner::try_new([0x61; 32], 6).expect("owner");
        let facade = open_facade(root.path(), owner, [0x71; 32]).expect("owner spool");
        let upload_one = sealed_event_upload(run_id, event_id);
        let revision_one = revisions_for(&upload_one, 1);
        let registered_one = facade
            .register_sealed_upload(&upload_one, &revision_one)
            .expect("open revision");
        let old_dispatch = facade
            .begin_next_dispatch(&registered_one, &upload_one, 0)
            .expect("dispatch query")
            .expect("open dispatch");
        let old_key = old_dispatch
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::Event)
            .expect("event revision")
            .key()
            .clone();

        let upload_two = sealed_event_upload(run_id, event_id);
        let revision_two = revisions_for(&upload_two, 2);
        let registered_two = facade
            .register_sealed_upload(&upload_two, &revision_two)
            .expect("terminal revision");

        let cas = facade
            .apply_validated_ack(&old_dispatch, TraceFullAck::new(vec![old_key], vec![]), 0)
            .expect("old ACK is handled by revision CAS");
        assert_eq!(cas.accepted().matched_items(), 0);
        assert_eq!(cas.accepted().stale_items(), 1);
        let terminal_dispatch = facade
            .begin_next_dispatch(&registered_two, &upload_two, 0)
            .expect("dispatch query")
            .expect("terminal dispatch remains pending");
        assert_eq!(
            terminal_dispatch
                .items()
                .iter()
                .find(|item| item.key().entity() == TraceManifestEntity::Event)
                .expect("terminal event revision")
                .revision(),
            2
        );
    }

    #[test]
    fn stale_credential_rejection_does_not_report_or_schedule_remediation() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let owner = AuthenticatedTraceOwner::try_new([0x62; 32], 7).expect("owner");
        let facade = open_facade(root.path(), owner, [0x72; 32]).expect("owner spool");
        let upload = sealed_event_upload(run_id, event_id);
        let revision_one = revisions_for(&upload, 1);
        let registered_one = facade
            .register_sealed_upload(&upload, &revision_one)
            .expect("open revision");
        let old_dispatch = facade
            .begin_next_dispatch(&registered_one, &upload, 0)
            .expect("dispatch query")
            .expect("open dispatch");
        let old_event = old_dispatch
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::Event)
            .expect("event revision")
            .key()
            .clone();
        let old_chunk = old_dispatch
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::OutputChunk)
            .expect("chunk revision")
            .key()
            .clone();

        let mut replacement_chunk = old_dispatch
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::OutputChunk)
            .expect("chunk revision")
            .clone();
        replacement_chunk.revision = 2;
        facade
            .store
            .register_sealed_attempt(SealedAttemptManifest::new(
                SealedAttemptId::from_digest([0x82; 32]),
                facade.spool_owner.clone(),
                ProtectionSealedUploadId::from_digest([0x92; 32]),
                facade.client_version_hash.clone(),
                1,
                0,
                0,
                1,
                vec![spool_item(&replacement_chunk).expect("replacement chunk")],
            ))
            .expect("concurrent terminal chunk revision");

        let result = facade
            .apply_validated_ack(
                &old_dispatch,
                TraceFullAck::new(
                    vec![old_event],
                    vec![TraceRejectedAck::new(
                        old_chunk,
                        TraceRejectedCode::CredentialRejected,
                    )],
                ),
                0,
            )
            .expect("stale ACK is handled by revision CAS");
        assert_eq!(result.accepted().matched_items(), 1);
        assert_eq!(result.accepted().stale_items(), 0);
        assert!(result.credential_remediation().is_empty());
        assert!(facade
            .store
            .due_remediations(&facade.spool_owner, &facade.client_version_hash)
            .expect("remediation query")
            .is_empty());
    }

    #[test]
    fn chunk_revision_context_must_match_its_sealed_event() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let chunk_id = upload.output_chunks()[0].chunk_id();
        let owner = AuthenticatedTraceOwner::try_new([0x81; 32], 8).expect("owner");
        let facade = open_facade(root.path(), owner, [0x91; 32]).expect("owner spool");
        let wrong_timestamp = TraceRevisionManifest::try_new(vec![
            TraceItemRevisionInput::try_new(event_id, run_id, 1, 11).expect("event revision"),
            TraceItemRevisionInput::try_new(chunk_id, run_id, 1, 12).expect("chunk revision"),
        ])
        .expect("bounded revision manifest");

        assert!(matches!(
            facade.register_sealed_upload(&upload, &wrong_timestamp),
            Err(TraceFacadeError::InvalidRevisionManifest)
        ));

        let exact = TraceRevisionManifest::try_new(vec![
            TraceItemRevisionInput::try_new(event_id, run_id, 1, 11).expect("event revision"),
            TraceItemRevisionInput::try_new(chunk_id, run_id, 1, 11).expect("chunk revision"),
        ])
        .expect("bounded revision manifest");
        let registered = facade
            .register_sealed_upload(&upload, &exact)
            .expect("exact sealed hierarchy");
        let chunk = registered
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::OutputChunk)
            .expect("chunk metadata");
        assert_eq!(chunk.trace_id(), run_id);
        assert_eq!(chunk.parent().expect("event parent").item_id(), event_id);
        assert!(!persisted_bytes(root.path())
            .windows(BODY_SENTINEL.len())
            .any(|window| window == BODY_SENTINEL.as_bytes()));
    }

    #[test]
    fn invalid_owner_client_and_revision_membership_fail_before_registration() {
        assert!(matches!(
            AuthenticatedTraceOwner::try_new([0; 32], 1),
            Err(TraceFacadeError::InvalidOwner)
        ));
        assert!(matches!(
            AuthenticatedTraceOwner::try_new([1; 32], 0),
            Err(TraceFacadeError::InvalidOwner)
        ));
        assert!(matches!(
            TraceRevisionManifest::try_new(vec![]),
            Err(TraceFacadeError::InvalidRevisionManifest)
        ));

        let root = TempDir::new().expect("temporary spool");
        let owner = AuthenticatedTraceOwner::try_new([0xa1; 32], 10).expect("owner");
        assert!(matches!(
            open_facade(root.path(), owner.clone(), [0; 32]),
            Err(TraceFacadeError::InvalidClientVersion)
        ));
        let facade = open_facade(root.path(), owner, [0xb1; 32]).expect("owner spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let foreign_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let foreign = TraceRevisionManifest::try_new(vec![TraceItemRevisionInput::try_new(
            foreign_id, run_id, 1, 11,
        )
        .expect("bounded foreign revision")])
        .expect("bounded foreign manifest");

        assert!(matches!(
            facade.register_sealed_upload(&upload, &foreign),
            Err(TraceFacadeError::InvalidRevisionManifest)
        ));
        assert!(!persisted_bytes(root.path())
            .windows(BODY_SENTINEL.len())
            .any(|window| window == BODY_SENTINEL.as_bytes()));
    }

    #[test]
    fn one_owner_cannot_apply_another_owners_dispatch_ack() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let revisions = revisions_for(&upload, 1);
        let facade_a = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0xc1; 32], 12).expect("owner A"),
            [0xd1; 32],
        )
        .expect("owner A spool");
        let facade_b = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0xe1; 32], 13).expect("owner B"),
            [0xd1; 32],
        )
        .expect("owner B spool");
        let registered = facade_a
            .register_sealed_upload(&upload, &revisions)
            .expect("owner A registration");
        let dispatch = facade_a
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("dispatch query")
            .expect("owner A dispatch");
        let key = dispatch.items()[0].key().clone();

        assert!(matches!(
            facade_b.apply_validated_ack(&dispatch, TraceFullAck::new(vec![key], vec![]), 0,),
            Err(TraceFacadeError::InvalidOwner)
        ));
    }

    #[test]
    fn another_client_version_for_the_same_owner_cannot_apply_a_dispatch_ack() {
        let root = TempDir::new().expect("temporary spool");
        let owner = AuthenticatedTraceOwner::try_new([0xef; 32], 21).expect("owner");
        let exact_facade =
            open_facade(root.path(), owner.clone(), [0x10; 32]).expect("exact version spool");
        let other_version_facade =
            open_facade(root.path(), owner, [0x20; 32]).expect("other version spool");
        let upload = sealed_event_upload(
            TraceId::try_new_v7().expect("run"),
            TraceId::try_new_v7().expect("event"),
        );
        let registered = exact_facade
            .register_sealed_upload(&upload, &revisions_for(&upload, 1))
            .expect("registration");
        let instruction = exact_facade
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("dispatch query")
            .expect("dispatch");
        let key = instruction.items()[0].key().clone();

        assert!(matches!(
            other_version_facade.apply_validated_ack(
                &instruction,
                TraceFullAck::new(vec![key.clone()], vec![]),
                0,
            ),
            Err(TraceFacadeError::InvalidClientVersion)
        ));

        let result = exact_facade
            .apply_validated_ack(&instruction, TraceFullAck::new(vec![key], vec![]), 0)
            .expect("exact version ACK");
        assert_eq!(result.accepted().matched_items(), 1);
    }

    #[test]
    fn later_chunk_attempt_uses_unforgeable_protection_trace_context() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let safe_line = b"safe output line\n";
        let large_output = std::iter::repeat_n(safe_line.as_slice(), 90_000)
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let mut output = Cursor::new(large_output);
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut output,
            &ExactSecretSet::empty(),
        )
        .expect("complete output")
        .into_event_upload_attempts(
            TraceEventText {
                event_id,
                run_id,
                sequence: 1,
                kind: TraceEventKindV2::Command,
                step_name: "flash",
                partition_name: None,
                status: TraceEventStatusV2::Success,
                started_at_ms: 29,
                ended_at_ms: Some(30),
                duration_ms: Some(1),
                command: None,
                exit_code: Some(0),
                verification: None,
                device_state: None,
                retry_safe: Some(true),
                remedies: &[],
                error_class: None,
                error_code: None,
                error_message: None,
            },
            &ExactSecretSet::empty(),
        )
        .expect("bounded attempts");
        let later = SentinelAttestedTraceUpload::try_from(
            attempts.into_iter().last().expect("later attempt"),
        )
        .expect("sentinel-attested later attempt");
        let view = later.metadata_view().expect("sealed metadata");
        assert_eq!(view.event_count(), 0);
        assert!(view.chunk_count() > 0);
        let foreign_trace = TraceId::try_new_v7().expect("UUIDv7");
        let forged = TraceRevisionManifest::try_new(
            view.items()
                .iter()
                .map(|item| {
                    TraceItemRevisionInput::try_new(
                        item.item_id(),
                        foreign_trace,
                        1,
                        item.created_at_ms(),
                    )
                    .expect("bounded forged revision")
                })
                .collect(),
        )
        .expect("bounded forged manifest");
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0xf1; 32], 14).expect("owner"),
            [0x12; 32],
        )
        .expect("owner spool");
        assert!(matches!(
            facade.register_sealed_upload(&later, &forged),
            Err(TraceFacadeError::InvalidRevisionManifest)
        ));

        let exact = TraceRevisionManifest::try_new(
            view.items()
                .iter()
                .map(|item| {
                    TraceItemRevisionInput::try_new(
                        item.item_id(),
                        item.trace_id(),
                        1,
                        item.created_at_ms(),
                    )
                    .expect("exact revision")
                })
                .collect(),
        )
        .expect("exact manifest");
        let registered = facade
            .register_sealed_upload(&later, &exact)
            .expect("later chunk registration");
        assert!(registered.items().iter().all(|item| {
            item.key().entity() == TraceManifestEntity::OutputChunk
                && item.trace_id() == run_id
                && item.parent().is_some_and(|parent| {
                    parent.entity() == TraceManifestEntity::Event && parent.item_id() == event_id
                })
        }));
    }

    #[test]
    fn wrong_seal_cannot_claim_or_mutate_the_pending_attempt() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let correct_upload = sealed_event_upload(run_id, event_id);
        let wrong_upload = sealed_event_upload(run_id, event_id);
        let revisions = revisions_for(&correct_upload, 1);
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x21; 32], 15).expect("owner"),
            [0x32; 32],
        )
        .expect("owner spool");
        let registered = facade
            .register_sealed_upload(&correct_upload, &revisions)
            .expect("registration");

        assert!(matches!(
            facade.begin_next_dispatch(&registered, &wrong_upload, 0),
            Err(TraceFacadeError::SealMismatch)
        ));
        let instruction = facade
            .begin_next_dispatch(&registered, &correct_upload, 0)
            .expect("correct seal query")
            .expect("correct seal remains claimable");
        assert!(instruction
            .sealed_upload_identity()
            .matches(&correct_upload)
            .expect("identity match"));
    }

    #[test]
    fn exact_registered_attempt_is_claimed_when_an_earlier_due_attempt_has_the_same_version() {
        let root = TempDir::new().expect("temporary spool");
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x33; 32], 18).expect("owner"),
            [0x44; 32],
        )
        .expect("owner spool");
        let upload_a = sealed_event_upload(
            TraceId::try_new_v7().expect("run A"),
            TraceId::try_new_v7().expect("event A"),
        );
        let upload_b = sealed_event_upload(
            TraceId::try_new_v7().expect("run B"),
            TraceId::try_new_v7().expect("event B"),
        );
        let registered_a = facade
            .register_sealed_upload(&upload_a, &revisions_for(&upload_a, 1))
            .expect("registration A");
        let registered_b = facade
            .register_sealed_upload(&upload_b, &revisions_for(&upload_b, 1))
            .expect("registration B");

        let (target_registration, target_upload) =
            if registered_a.attempt_identity.0 < registered_b.attempt_identity.0 {
                (&registered_b, &upload_b)
            } else {
                (&registered_a, &upload_a)
            };

        let instruction = facade
            .begin_next_dispatch(target_registration, target_upload, 0)
            .expect("exact registration query")
            .expect("exact registered attempt remains claimable");
        assert_eq!(
            instruction.attempt_identity(),
            target_registration.attempt_identity()
        );
    }

    #[test]
    fn claim_requires_the_exact_registered_item_revision_before_any_mutation() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("run");
        let event_id = TraceId::try_new_v7().expect("event");
        let upload = sealed_event_upload(run_id, event_id);
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x55; 32], 19).expect("owner"),
            [0x66; 32],
        )
        .expect("owner spool");
        let mut registered = facade
            .register_sealed_upload(&upload, &revisions_for(&upload, 1))
            .expect("registration");
        let event = registered
            .items
            .iter_mut()
            .find(|item| item.key.entity == TraceManifestEntity::Event)
            .expect("registered event");
        event.revision = 2;

        assert!(matches!(
            facade.begin_next_dispatch(&registered, &upload, 0),
            Err(TraceFacadeError::SealMismatch)
        ));

        registered
            .items
            .iter_mut()
            .find(|item| item.key.entity == TraceManifestEntity::Event)
            .expect("registered event")
            .revision = 1;
        assert!(facade
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("exact registration query")
            .is_some());
    }

    #[test]
    fn claim_rejects_a_registered_capability_from_another_client_version() {
        let root = TempDir::new().expect("temporary spool");
        let upload = sealed_event_upload(
            TraceId::try_new_v7().expect("run"),
            TraceId::try_new_v7().expect("event"),
        );
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x77; 32], 20).expect("owner"),
            [0x88; 32],
        )
        .expect("owner spool");
        let mut registered = facade
            .register_sealed_upload(&upload, &revisions_for(&upload, 1))
            .expect("registration");
        let exact_version = registered.client_version_hash.clone();
        registered.client_version_hash = ProtectionClientVersionHash::from_digest([0x99; 32]);

        assert!(matches!(
            facade.begin_next_dispatch(&registered, &upload, 0),
            Err(TraceFacadeError::SealMismatch)
        ));

        registered.client_version_hash = exact_version;
        assert!(facade
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("exact version query")
            .is_some());
    }

    #[test]
    fn reopened_metadata_only_spool_rejects_an_unrelated_live_capability() {
        let root = TempDir::new().expect("temporary spool");
        let owner = AuthenticatedTraceOwner::try_new([0xaa; 32], 22).expect("owner");
        let version = [0xbb; 32];
        let target_upload = sealed_event_upload(
            TraceId::try_new_v7().expect("target run"),
            TraceId::try_new_v7().expect("target event"),
        );
        let unrelated_upload = sealed_event_upload(
            TraceId::try_new_v7().expect("unrelated run"),
            TraceId::try_new_v7().expect("unrelated event"),
        );
        let facade = open_facade(root.path(), owner.clone(), version).expect("owner spool");
        let target_capability = facade
            .register_sealed_upload(&target_upload, &revisions_for(&target_upload, 1))
            .expect("target registration");
        let unrelated_capability = facade
            .register_sealed_upload(&unrelated_upload, &revisions_for(&unrelated_upload, 1))
            .expect("unrelated registration");
        drop(target_capability);
        drop(facade);

        let reopened = open_facade(root.path(), owner, version).expect("reopened metadata spool");
        assert!(matches!(
            reopened.begin_next_dispatch(&unrelated_capability, &target_upload, 0),
            Err(TraceFacadeError::SealMismatch)
        ));
    }

    #[test]
    fn malformed_ack_mutates_nothing_and_mixed_ack_applies_atomically() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let chunk_id = upload.output_chunks()[0].chunk_id();
        let revisions = TraceRevisionManifest::try_new(vec![
            TraceItemRevisionInput::try_new(event_id, run_id, 1, 11).expect("event revision"),
            TraceItemRevisionInput::try_new(chunk_id, run_id, 1, 11).expect("chunk revision"),
        ])
        .expect("manifest");
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x42; 32], 16).expect("owner"),
            [0x53; 32],
        )
        .expect("owner spool");
        let registered = facade
            .register_sealed_upload(&upload, &revisions)
            .expect("registration");
        let instruction = facade
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("dispatch query")
            .expect("dispatch");
        let event_key = instruction
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::Event)
            .expect("event")
            .key()
            .clone();
        let chunk_key = instruction
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::OutputChunk)
            .expect("chunk")
            .key()
            .clone();

        let overlap = TraceFullAck::new(
            vec![event_key.clone()],
            vec![TraceRejectedAck::new(
                event_key.clone(),
                TraceRejectedCode::Invalid,
            )],
        );
        assert!(matches!(
            facade.apply_validated_ack(&instruction, overlap, 100),
            Err(TraceFacadeError::InvalidAck)
        ));

        let valid = TraceFullAck::new(
            vec![event_key.clone()],
            vec![TraceRejectedAck::new(
                chunk_key.clone(),
                TraceRejectedCode::CredentialRejected,
            )],
        );
        let result = facade
            .apply_validated_ack(&instruction, valid, 100)
            .expect("atomic validated ACK");
        assert_eq!(result.accepted().matched_items(), 1);
        assert_eq!(result.accepted().stale_items(), 0);
        assert_eq!(result.rejected(), std::slice::from_ref(&chunk_key));
        assert!(result.unacknowledged().is_empty());
        assert_eq!(result.credential_remediation(), &[chunk_key]);
    }

    #[test]
    fn omitted_ack_member_is_returned_as_unacknowledged_and_retires_the_old_seal() {
        let root = TempDir::new().expect("temporary spool");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let upload = sealed_event_upload(run_id, event_id);
        let chunk_id = upload.output_chunks()[0].chunk_id();
        let revisions = TraceRevisionManifest::try_new(vec![
            TraceItemRevisionInput::try_new(event_id, run_id, 1, 11).expect("event revision"),
            TraceItemRevisionInput::try_new(chunk_id, run_id, 1, 11).expect("chunk revision"),
        ])
        .expect("manifest");
        let facade = open_facade(
            root.path(),
            AuthenticatedTraceOwner::try_new([0x62; 32], 17).expect("owner"),
            [0x73; 32],
        )
        .expect("owner spool");
        let registered = facade
            .register_sealed_upload(&upload, &revisions)
            .expect("registration");
        let instruction = facade
            .begin_next_dispatch(&registered, &upload, 0)
            .expect("dispatch query")
            .expect("dispatch");
        let event_key = instruction
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::Event)
            .expect("event")
            .key()
            .clone();
        let chunk_key = instruction
            .items()
            .iter()
            .find(|item| item.key().entity() == TraceManifestEntity::OutputChunk)
            .expect("chunk")
            .key()
            .clone();

        let result = facade
            .apply_validated_ack(
                &instruction,
                TraceFullAck::new(vec![event_key], vec![]),
                100,
            )
            .expect("atomic ACK");
        assert_eq!(result.accepted().matched_items(), 1);
        assert!(result.rejected().is_empty());
        assert_eq!(result.unacknowledged(), &[chunk_key]);
        assert!(result.credential_remediation().is_empty());
        assert!(facade
            .begin_next_dispatch(&registered, &upload, 1_100)
            .expect("dispatch query")
            .is_none());
    }
}
