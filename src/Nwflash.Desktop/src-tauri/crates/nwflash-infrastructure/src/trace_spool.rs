#![allow(dead_code)] // Wave 2 seam consumed by the forthcoming concrete protection bridge.

//! Crash-safe metadata spool for protection-sealed trace uploads.
//!
//! Sealed HTTP bodies are intentionally not persisted by this store. On a real
//! process restart, any pending/inflight attempt whose body was only held by a
//! live protection capability is therefore swept into a durable loss tombstone
//! instead of being presented as replayable metadata.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const TRACE_SPOOL_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
pub(crate) const TRACE_SPOOL_MAX_WIRE_BYTES: u64 = 1_048_576;
pub(crate) const TRACE_SPOOL_MAX_RUNS: u16 = 20;
pub(crate) const TRACE_SPOOL_MAX_EVENTS: u16 = 100;
pub(crate) const TRACE_SPOOL_MAX_CHUNKS: u16 = 200;

const MANIFEST_VERSION: u32 = 2;
const RETENTION_REASON: &str = "retention_expired_7d";
const STARTUP_ORPHAN_REASON: &str = "restart_payload_unrecoverable";
const ROOT_LOCK_FILE: &str = ".trace-spool.lock";
const ROOT_LOCK_TIMEOUT: Duration = Duration::from_millis(500);
const ROOT_LOCK_RETRY: Duration = Duration::from_millis(10);
const MAX_MANIFEST_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LOSS_BYTES: u64 = 64 * 1024;
const MAX_VERSION_GATE_BYTES: u64 = 256 * 1024;
const MAX_MANIFEST_ITEMS: usize = 20 + 100 + 200;
const MAX_STORED_ATTEMPTS: usize = 256;
const MAX_TOMBSTONES: usize = 4096;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct TraceOwnerGeneration {
    username_hash: [u8; 32],
    login_generation: u64,
}
impl TraceOwnerGeneration {
    pub(crate) fn from_canonical_username_hash(
        username_hash: [u8; 32],
        login_generation: u64,
    ) -> Self {
        Self {
            username_hash,
            login_generation,
        }
    }
    pub fn login_generation(&self) -> u64 {
        self.login_generation
    }
    fn directory_hash(&self) -> String {
        hex(&Sha256::digest(self.username_hash))
    }
    fn scope_hash(&self) -> String {
        hex(&Sha256::digest(
            [
                self.username_hash.as_slice(),
                &self.login_generation.to_le_bytes(),
            ]
            .concat(),
        ))
    }
}
impl std::fmt::Debug for TraceOwnerGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TraceOwnerGeneration")
            .field("username_hash_prefix", &&self.directory_hash()[..8])
            .field("login_generation", &self.login_generation)
            .finish()
    }
}

#[derive(Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ProtectionSealedUploadId([u8; 32]);
impl ProtectionSealedUploadId {
    pub(crate) fn from_digest(value: [u8; 32]) -> Self {
        Self(value)
    }
}
impl std::fmt::Debug for ProtectionSealedUploadId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectionSealedUploadId([opaque])")
    }
}

#[derive(Clone, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub(crate) struct ProtectionClientVersionHash([u8; 32]);
impl ProtectionClientVersionHash {
    pub(crate) fn from_digest(value: [u8; 32]) -> Self {
        Self(value)
    }
}
impl std::fmt::Debug for ProtectionClientVersionHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectionClientVersionHash([opaque])")
    }
}

#[derive(Clone, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct SealedAttemptId([u8; 32]);
impl SealedAttemptId {
    pub(crate) fn from_digest(value: [u8; 32]) -> Self {
        Self(value)
    }
}
impl std::fmt::Debug for SealedAttemptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealedAttemptId([opaque])")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TraceSpoolEntity {
    Run,
    Event,
    OutputChunk,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) struct TraceItemKey {
    entity: TraceSpoolEntity,
    item_id: String,
}
impl TraceItemKey {
    pub fn new(entity: TraceSpoolEntity, item_id: impl Into<String>) -> Self {
        Self {
            entity,
            item_id: item_id.into(),
        }
    }
    pub fn entity(&self) -> TraceSpoolEntity {
        self.entity
    }
    pub fn item_id(&self) -> &str {
        &self.item_id
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct SealedItemRevision {
    key: TraceItemKey,
    trace_id: String,
    parent: Option<TraceItemKey>,
    revision: u64,
    created_at_ms: u64,
}
impl SealedItemRevision {
    pub(crate) fn new(
        key: TraceItemKey,
        trace_id: impl Into<String>,
        parent: Option<TraceItemKey>,
        revision: u64,
        created_at_ms: u64,
    ) -> Self {
        Self {
            key,
            trace_id: trace_id.into(),
            parent,
            revision,
            created_at_ms,
        }
    }
    pub fn key(&self) -> &TraceItemKey {
        &self.key
    }
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }
    pub fn parent(&self) -> Option<&TraceItemKey> {
        self.parent.as_ref()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn created_at_ms(&self) -> u64 {
        self.created_at_ms
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct SealedAttemptManifest {
    attempt_id: SealedAttemptId,
    owner: TraceOwnerGeneration,
    sealed_upload_id: ProtectionSealedUploadId,
    client_version_hash: ProtectionClientVersionHash,
    wire_bytes: u64,
    run_count: u16,
    event_count: u16,
    chunk_count: u16,
    items: Vec<SealedItemRevision>,
}
impl SealedAttemptManifest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        attempt_id: SealedAttemptId,
        owner: TraceOwnerGeneration,
        sealed_upload_id: ProtectionSealedUploadId,
        client_version_hash: ProtectionClientVersionHash,
        wire_bytes: u64,
        run_count: u16,
        event_count: u16,
        chunk_count: u16,
        items: Vec<SealedItemRevision>,
    ) -> Self {
        Self {
            attempt_id,
            owner,
            sealed_upload_id,
            client_version_hash,
            wire_bytes,
            run_count,
            event_count,
            chunk_count,
            items,
        }
    }
}
impl std::fmt::Debug for SealedAttemptManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealedAttemptManifest")
            .field("attempt_id", &self.attempt_id)
            .field("owner", &self.owner)
            .field("wire_bytes", &self.wire_bytes)
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResealReason {
    ServerUnacked,
    Retryable,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OwnerPauseReason {
    Unauthorized,
    Forbidden,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptPauseReason {
    Forbidden,
    ManualIntervention,
    LocalContract,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CasMutationResult {
    matched_items: usize,
    stale_items: usize,
}
impl CasMutationResult {
    pub fn matched_items(&self) -> usize {
        self.matched_items
    }
    pub fn stale_items(&self) -> usize {
        self.stale_items
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedAckMutationResult {
    accepted: CasMutationResult,
    remediation: Vec<TraceItemKey>,
}

impl ValidatedAckMutationResult {
    pub(crate) fn accepted(&self) -> &CasMutationResult {
        &self.accepted
    }

    pub(crate) fn remediation(&self) -> &[TraceItemKey] {
        &self.remediation
    }
}

#[derive(Clone)]
pub(crate) struct DueSealedAttempt {
    manifest: SealedAttemptManifest,
    attempt_count: u32,
    next_attempt_at_ms: u64,
}
#[derive(Clone)]
pub(crate) struct InflightAttemptHandle {
    manifest: SealedAttemptManifest,
    attempt_count: u32,
}
#[derive(Clone, Debug)]
pub(crate) struct DueResealItem {
    item: SealedItemRevision,
    attempt_count: u32,
    next_attempt_at_ms: u64,
}
#[derive(Clone)]
pub(crate) struct DueRemediation {
    handle: InflightAttemptHandle,
    affected: Vec<TraceItemKey>,
}

macro_rules! attempt_getters {
    ($type:ty) => {
        impl $type {
            pub fn attempt_id(&self) -> &SealedAttemptId {
                &self.manifest.attempt_id
            }
            pub fn owner(&self) -> &TraceOwnerGeneration {
                &self.manifest.owner
            }
            pub fn sealed_upload_id(&self) -> &ProtectionSealedUploadId {
                &self.manifest.sealed_upload_id
            }
            pub fn client_version_hash(&self) -> &ProtectionClientVersionHash {
                &self.manifest.client_version_hash
            }
            pub fn wire_bytes(&self) -> u64 {
                self.manifest.wire_bytes
            }
            pub fn run_count(&self) -> u16 {
                self.manifest.run_count
            }
            pub fn event_count(&self) -> u16 {
                self.manifest.event_count
            }
            pub fn chunk_count(&self) -> u16 {
                self.manifest.chunk_count
            }
            pub fn items(&self) -> &[SealedItemRevision] {
                &self.manifest.items
            }
            pub fn attempt_count(&self) -> u32 {
                self.attempt_count
            }
        }
    };
}
attempt_getters!(DueSealedAttempt);
attempt_getters!(InflightAttemptHandle);
impl DueSealedAttempt {
    pub fn next_attempt_at_ms(&self) -> u64 {
        self.next_attempt_at_ms
    }
}
impl DueResealItem {
    pub fn item(&self) -> &SealedItemRevision {
        &self.item
    }
    pub fn attempt_count(&self) -> u32 {
        self.attempt_count
    }
    pub fn next_attempt_at_ms(&self) -> u64 {
        self.next_attempt_at_ms
    }
}
impl DueRemediation {
    pub fn handle(&self) -> &InflightAttemptHandle {
        &self.handle
    }
    pub fn affected(&self) -> &[TraceItemKey] {
        &self.affected
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TraceSpoolError {
    #[error("trace spool I/O operation failed")]
    Io(#[from] std::io::Error),
    #[error("trace spool JSON operation failed")]
    Json(#[from] serde_json::Error),
    #[error("trace spool manifest scope mismatch")]
    ScopeMismatch,
    #[error("sealed attempt metadata is invalid")]
    InvalidAttempt,
    #[error("sealed item revision transition is invalid")]
    InvalidRevision,
    #[error("sealed attempt was not found")]
    AttemptNotFound,
    #[error("sealed attempt is not claimable")]
    AttemptNotClaimable,
    #[error("inflight handle is stale or invalid")]
    InvalidHandle,
    #[error("owner generation is paused")]
    OwnerPaused,
    #[error("client version is blocked by update policy")]
    ClientVersionBlocked,
    #[error("trace was discarded by retention")]
    ExpiredTrace,
    #[error("retry attempt overflow")]
    AttemptOverflow,
    #[error("trace spool root lock is unavailable")]
    LockUnavailable,
    #[error("trace spool path is not a safe directory")]
    UnsafePath,
    #[cfg(test)]
    #[error("injected atomic replace failure")]
    InjectedAtomicReplace,
}

#[derive(Clone, Deserialize, Serialize)]
enum ItemDelivery {
    Sealed(SealedAttemptId),
    NeedsSeal,
}
#[derive(Clone, Deserialize, Serialize)]
struct CurrentItem {
    item: SealedItemRevision,
    delivery: ItemDelivery,
    client_version_hash: ProtectionClientVersionHash,
    attempt_count: u32,
    next_attempt_at_ms: u64,
}
#[derive(Clone, Deserialize, Serialize)]
enum AttemptState {
    Pending,
    Inflight,
    NeedsRemediation(Vec<TraceItemKey>),
    Retired,
    Paused(AttemptPauseReason),
}
#[derive(Clone, Deserialize, Serialize)]
struct StoredAttempt {
    manifest: SealedAttemptManifest,
    state: AttemptState,
    attempt_count: u32,
    next_attempt_at_ms: u64,
}
#[derive(Deserialize, Serialize)]
struct DiskManifest {
    version: u32,
    owner: TraceOwnerGeneration,
    owner_pause: Option<OwnerPauseReason>,
    items: Vec<CurrentItem>,
    attempts: Vec<StoredAttempt>,
}
impl DiskManifest {
    fn empty(owner: &TraceOwnerGeneration) -> Self {
        Self {
            version: MANIFEST_VERSION,
            owner: owner.clone(),
            owner_pause: None,
            items: vec![],
            attempts: vec![],
        }
    }
}
#[derive(Default, Deserialize, Serialize)]
struct VersionGates {
    client_version_hashes: Vec<ProtectionClientVersionHash>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct TraceLossDiagnostic {
    scope_hash: String,
    trace_id: String,
    item_count: usize,
    attempt_count: usize,
    expired_at_ms: u64,
    reason: String,
}

pub(crate) struct TraceSpoolStore {
    root: PathBuf,
    state: Arc<RootState>,
    #[cfg(test)]
    fail_manifest: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_loss: std::sync::atomic::AtomicBool,
}

struct RootState {
    mutation: Mutex<()>,
}

struct RootMutationGuard<'a> {
    _in_process: std::sync::MutexGuard<'a, ()>,
    _root: DirectoryAnchor,
    _file_lock: RootFileLock,
}

struct RootFileLock {
    file: fs::File,
}

/// Keeps a spool directory itself open without `FILE_SHARE_DELETE`.  On Windows this is
/// deliberately a handle, rather than a metadata check: a junction cannot replace the
/// held directory between validation and a child operation.
struct DirectoryAnchor {
    path: PathBuf,
    #[cfg(windows)]
    _handle: fs::File,
}

impl DirectoryAnchor {
    #[cfg(windows)]
    fn open_existing(path: &Path) -> Result<Self, TraceSpoolError> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };

        let handle = OpenOptions::new()
            .read(true)
            // Never follow a final-component junction/symlink and deny its deletion
            // while callers resolve children beneath it.
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(path)?;
        let metadata = handle.metadata()?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(TraceSpoolError::UnsafePath);
        }
        Ok(Self {
            path: path.to_path_buf(),
            _handle: handle,
        })
    }

    #[cfg(not(windows))]
    fn open_existing(path: &Path) -> Result<Self, TraceSpoolError> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(TraceSpoolError::UnsafePath);
        }
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn open_child(&self, name: &str) -> Result<Self, TraceSpoolError> {
        Self::open_existing(&self.path.join(name))
    }

    fn create_or_open_child(&self, name: &str) -> Result<Self, TraceSpoolError> {
        let path = self.path.join(name);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Self::open_existing(&path)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

struct DirectoryChain {
    anchors: Vec<DirectoryAnchor>,
}

impl DirectoryChain {
    fn directory(&self) -> &DirectoryAnchor {
        self.anchors
            .last()
            .expect("root directory anchor is always present")
    }
}

impl RootFileLock {
    fn acquire(root: &DirectoryAnchor) -> Result<Self, TraceSpoolError> {
        let path = root.path().join(ROOT_LOCK_FILE);
        let file = open_regular_file(&path, true, true)?;
        let deadline = Instant::now() + ROOT_LOCK_TIMEOUT;
        loop {
            match try_lock_file(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(()) if Instant::now() < deadline => std::thread::sleep(ROOT_LOCK_RETRY),
                Err(()) => return Err(TraceSpoolError::LockUnavailable),
            }
        }
    }
}

impl Drop for RootFileLock {
    fn drop(&mut self) {
        let _ = unlock_file(&self.file);
    }
}

static ROOT_STATES: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<RootState>>>> =
    OnceLock::new();

fn safe_spool_root(root: &Path) -> Result<PathBuf, TraceSpoolError> {
    let root = if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()?.join(root)
    };

    // Walk all the way to the first existing ancestor with no-follow directory
    // opens. This both rejects a junction above a missing component and gives
    // us an anchored parent from which each missing component can be created.
    let mut cursor = root.clone();
    let mut missing = Vec::new();
    let mut nearest = None;
    loop {
        match DirectoryAnchor::open_existing(&cursor) {
            Ok(anchor) => {
                if nearest.is_none() {
                    nearest = Some(anchor);
                }
            }
            Err(TraceSpoolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                if nearest.is_some() {
                    return Err(TraceSpoolError::UnsafePath);
                }
                missing.push(
                    cursor
                        .file_name()
                        .ok_or(TraceSpoolError::UnsafePath)?
                        .to_os_string(),
                );
            }
            Err(error) => return Err(error),
        }

        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent.to_path_buf();
    }

    let mut anchors = vec![nearest.ok_or(TraceSpoolError::UnsafePath)?];
    for component in missing.iter().rev() {
        let name = component.to_str().ok_or(TraceSpoolError::UnsafePath)?;
        let child = anchors
            .last()
            .expect("nearest existing ancestor anchor is always present")
            .create_or_open_child(name)?;
        anchors.push(child);
    }

    // Keep the existing canonical-root contract for registry keying while all
    // security decisions above were made from no-follow anchored directories.
    let canonical = fs::canonicalize(
        anchors
            .last()
            .expect("root directory anchor is always present")
            .path(),
    )?;
    assert_safe_existing_ancestors(&canonical)?;
    Ok(canonical)
}

#[cfg(windows)]
fn assert_safe_existing_ancestors(path: &Path) -> Result<(), TraceSpoolError> {
    let mut current = path.to_path_buf();
    loop {
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || is_reparse_point(&metadata) {
                    return Err(TraceSpoolError::UnsafePath);
                }
            }
            // A missing leaf is allowed, but continue walking so an existing
            // parent junction cannot be hidden behind that missing component.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if parent == current {
            break;
        }
        current = parent.to_path_buf();
    }
    Ok(())
}

#[cfg(not(windows))]
fn assert_safe_existing_ancestors(path: &Path) -> Result<(), TraceSpoolError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if !metadata.is_dir() || is_reparse_point(&metadata) {
                    return Err(TraceSpoolError::UnsafePath);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn assert_safe_file_if_present(path: &Path) -> Result<(), TraceSpoolError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_reparse_point(&metadata) => Ok(()),
        Ok(_) => Err(TraceSpoolError::UnsafePath),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn open_regular_file(path: &Path, write: bool, create: bool) -> Result<fs::File, TraceSpoolError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .create(create)
        .truncate(false)
        // Bind the final component itself so a file reparse point is rejected from
        // its handle, not from a stale path metadata check.
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(TraceSpoolError::UnsafePath);
    }
    Ok(file)
}

#[cfg(not(windows))]
fn open_regular_file(path: &Path, write: bool, create: bool) -> Result<fs::File, TraceSpoolError> {
    assert_safe_file_if_present(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .create(create)
        .truncate(false)
        .open(path)?;
    assert_safe_file_if_present(path)?;
    Ok(file)
}

#[cfg(windows)]
fn create_new_regular_file(path: &Path) -> Result<fs::File, TraceSpoolError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(TraceSpoolError::UnsafePath);
    }
    Ok(file)
}

#[cfg(not(windows))]
fn create_new_regular_file(path: &Path) -> Result<fs::File, TraceSpoolError> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    assert_safe_file_if_present(path)?;
    Ok(file)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn try_lock_file(file: &fs::File) -> Result<(), ()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::LockFile;
    if unsafe { LockFile(file.as_raw_handle() as HANDLE, 0, 0, 1, 0) } != 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(windows)]
fn unlock_file(file: &fs::File) -> Result<(), ()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::UnlockFile;
    if unsafe { UnlockFile(file.as_raw_handle() as HANDLE, 0, 0, 1, 0) } != 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(unix)]
fn try_lock_file(file: &fs::File) -> Result<(), ()> {
    use std::os::unix::io::AsRawFd;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(unix)]
fn unlock_file(file: &fs::File) -> Result<(), ()> {
    use std::os::unix::io::AsRawFd;
    const LOCK_UN: i32 = 8;
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    if unsafe { flock(file.as_raw_fd(), LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(not(any(windows, unix)))]
fn try_lock_file(_file: &fs::File) -> Result<(), ()> {
    Err(())
}

#[cfg(not(any(windows, unix)))]
fn unlock_file(_file: &fs::File) -> Result<(), ()> {
    Ok(())
}

impl TraceSpoolStore {
    pub(crate) fn open(root: PathBuf) -> Result<Self, TraceSpoolError> {
        let root = safe_spool_root(&root)?;
        let registry = ROOT_STATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
        let mut registry = registry
            .lock()
            .map_err(|_| TraceSpoolError::InvalidAttempt)?;
        let (state, first_open) = match registry.get(&root).and_then(Weak::upgrade) {
            Some(state) => (state, false),
            None => {
                let state = Arc::new(RootState {
                    mutation: Mutex::new(()),
                });
                registry.insert(root.clone(), Arc::downgrade(&state));
                (state, true)
            }
        };
        drop(registry);
        let store = Self {
            root,
            state,
            #[cfg(test)]
            fail_manifest: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_loss: std::sync::atomic::AtomicBool::new(false),
        };
        if first_open {
            let _guard = store.lock()?;
            store.recover_startup()?;
        }
        Ok(store)
    }

    pub(crate) fn register_sealed_attempt(
        &self,
        manifest: SealedAttemptManifest,
    ) -> Result<(), TraceSpoolError> {
        self.register_attempt(manifest, RegisterMode::NewRevision)
    }

    pub(crate) fn register_resealed_attempt(
        &self,
        manifest: SealedAttemptManifest,
    ) -> Result<(), TraceSpoolError> {
        self.register_attempt(manifest, RegisterMode::Reseal)
    }

    fn register_attempt(
        &self,
        incoming: SealedAttemptManifest,
        mode: RegisterMode,
    ) -> Result<(), TraceSpoolError> {
        validate_attempt(&incoming)?;
        let _guard = self.lock()?;
        let mut disk = self.load(&incoming.owner)?;
        if disk.owner_pause.is_some() {
            return Err(TraceSpoolError::OwnerPaused);
        }
        if self
            .load_version_gates()?
            .client_version_hashes
            .contains(&incoming.client_version_hash)
        {
            return Err(TraceSpoolError::ClientVersionBlocked);
        }
        let tombstones = self.tombstones(&incoming.owner)?;
        if incoming
            .items
            .iter()
            .any(|item| tombstones.contains(item.trace_id()))
        {
            return Err(TraceSpoolError::ExpiredTrace);
        }
        if disk.attempts.iter().any(|attempt| {
            attempt.manifest.attempt_id == incoming.attempt_id
                || attempt.manifest.sealed_upload_id == incoming.sealed_upload_id
        }) {
            return Err(TraceSpoolError::InvalidAttempt);
        }

        let mut states = Vec::with_capacity(incoming.items.len());
        for item in &incoming.items {
            match disk
                .items
                .iter()
                .find(|current| current.item.key == item.key)
            {
                None if mode == RegisterMode::NewRevision && item.revision == 1 => {
                    states.push((0, 0))
                }
                Some(current) if mode == RegisterMode::NewRevision => {
                    if item.revision
                        != current
                            .item
                            .revision
                            .checked_add(1)
                            .ok_or(TraceSpoolError::InvalidRevision)?
                        || !same_metadata(&current.item, item)
                    {
                        return Err(TraceSpoolError::InvalidRevision);
                    }
                    states.push((current.attempt_count, 0));
                }
                Some(current) if mode == RegisterMode::Reseal => {
                    if !matches!(current.delivery, ItemDelivery::NeedsSeal) || current.item != *item
                    {
                        return Err(TraceSpoolError::InvalidRevision);
                    }
                    states.push((current.attempt_count, current.next_attempt_at_ms));
                }
                _ => return Err(TraceSpoolError::InvalidRevision),
            }
        }

        let attempt_count = states.iter().map(|state| state.0).max().unwrap_or(0);
        let next_attempt_at_ms = states.iter().map(|state| state.1).max().unwrap_or(0);
        if mode == RegisterMode::NewRevision {
            let superseded = incoming
                .items
                .iter()
                .map(|item| item.key.clone())
                .collect::<HashSet<_>>();
            let retired = disk
                .attempts
                .iter_mut()
                .filter(|attempt| {
                    matches!(attempt.state, AttemptState::Pending)
                        && attempt
                            .manifest
                            .items
                            .iter()
                            .any(|item| superseded.contains(&item.key))
                })
                .map(|attempt| {
                    attempt.state = AttemptState::Retired;
                    attempt.manifest.attempt_id.clone()
                })
                .collect::<Vec<_>>();
            for current in &mut disk.items {
                if matches!(&current.delivery, ItemDelivery::Sealed(id) if retired.contains(id)) {
                    current.delivery = ItemDelivery::NeedsSeal;
                }
            }
        }
        for (item, (count, next)) in incoming.items.iter().cloned().zip(states) {
            if let Some(current) = disk
                .items
                .iter_mut()
                .find(|current| current.item.key == item.key)
            {
                *current = CurrentItem {
                    item,
                    delivery: ItemDelivery::Sealed(incoming.attempt_id.clone()),
                    client_version_hash: incoming.client_version_hash.clone(),
                    attempt_count: count,
                    next_attempt_at_ms: next,
                };
            } else {
                disk.items.push(CurrentItem {
                    item,
                    delivery: ItemDelivery::Sealed(incoming.attempt_id.clone()),
                    client_version_hash: incoming.client_version_hash.clone(),
                    attempt_count: count,
                    next_attempt_at_ms: next,
                });
            }
        }
        disk.attempts.push(StoredAttempt {
            manifest: incoming,
            state: AttemptState::Pending,
            attempt_count,
            next_attempt_at_ms,
        });
        self.persist(&disk)
    }

    pub(crate) fn due_attempts(
        &self,
        owner: &TraceOwnerGeneration,
        now_ms: u64,
    ) -> Result<Vec<DueSealedAttempt>, TraceSpoolError> {
        self.expire(owner, now_ms)?;
        self.peek_due_attempts(owner, now_ms)
    }

    /// Returns the due metadata snapshot without expiry, recovery, or any
    /// other state transition. Callers must validate the concrete protection
    /// seal against this snapshot before claiming it.
    pub(crate) fn peek_due_attempts(
        &self,
        owner: &TraceOwnerGeneration,
        now_ms: u64,
    ) -> Result<Vec<DueSealedAttempt>, TraceSpoolError> {
        let _guard = self.lock()?;
        let disk = self.load(owner)?;
        if disk.owner_pause.is_some() {
            return Ok(vec![]);
        }
        let gates = self.load_version_gates()?;
        let mut due = disk
            .attempts
            .iter()
            .filter(|attempt| {
                matches!(attempt.state, AttemptState::Pending)
                    && attempt.next_attempt_at_ms <= now_ms
                    && !gates
                        .client_version_hashes
                        .contains(&attempt.manifest.client_version_hash)
            })
            .map(|attempt| DueSealedAttempt {
                manifest: attempt.manifest.clone(),
                attempt_count: attempt.attempt_count,
                next_attempt_at_ms: attempt.next_attempt_at_ms,
            })
            .collect::<Vec<_>>();
        due.sort_by(|a, b| {
            a.next_attempt_at_ms
                .cmp(&b.next_attempt_at_ms)
                .then_with(|| a.attempt_id().cmp(b.attempt_id()))
        });
        Ok(due)
    }

    pub(crate) fn due_reseal_items(
        &self,
        owner: &TraceOwnerGeneration,
        now_ms: u64,
        current_client_version_hash: &ProtectionClientVersionHash,
    ) -> Result<Vec<DueResealItem>, TraceSpoolError> {
        self.expire(owner, now_ms)?;
        let _guard = self.lock()?;
        let mut disk = self.load(owner)?;
        if disk.owner_pause.is_some() {
            return Ok(vec![]);
        }
        let gates = self.load_version_gates()?;
        if gates
            .client_version_hashes
            .contains(current_client_version_hash)
        {
            return Ok(Vec::new());
        }
        if reconcile_blocked_attempts(&mut disk, &gates) {
            self.persist(&disk)?;
        }
        let mut due = disk
            .items
            .iter()
            .filter(|item| {
                matches!(item.delivery, ItemDelivery::NeedsSeal)
                    && item.next_attempt_at_ms <= now_ms
            })
            .map(|item| DueResealItem {
                item: item.item.clone(),
                attempt_count: item.attempt_count,
                next_attempt_at_ms: item.next_attempt_at_ms,
            })
            .collect::<Vec<_>>();
        due.sort_by(|a, b| {
            a.next_attempt_at_ms
                .cmp(&b.next_attempt_at_ms)
                .then_with(|| a.item.created_at_ms.cmp(&b.item.created_at_ms))
                .then_with(|| a.item.key.cmp(&b.item.key))
        });
        Ok(due)
    }

    pub(crate) fn due_remediations(
        &self,
        owner: &TraceOwnerGeneration,
        current_client_version_hash: &ProtectionClientVersionHash,
    ) -> Result<Vec<DueRemediation>, TraceSpoolError> {
        let _guard = self.lock()?;
        let disk = self.load(owner)?;
        if disk.owner_pause.is_some()
            || self
                .load_version_gates()?
                .client_version_hashes
                .contains(current_client_version_hash)
        {
            return Ok(Vec::new());
        }
        let mut due = disk
            .attempts
            .iter()
            .filter_map(|attempt| match &attempt.state {
                AttemptState::NeedsRemediation(affected) if !affected.is_empty() => {
                    Some(DueRemediation {
                        handle: InflightAttemptHandle {
                            manifest: attempt.manifest.clone(),
                            attempt_count: attempt.attempt_count,
                        },
                        affected: affected.clone(),
                    })
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        due.sort_by(|left, right| left.handle.attempt_id().cmp(right.handle.attempt_id()));
        Ok(due)
    }

    pub(crate) fn begin_dispatch(
        &self,
        attempt_id: &SealedAttemptId,
        owner: &TraceOwnerGeneration,
    ) -> Result<InflightAttemptHandle, TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(owner)?;
        if disk.owner_pause.is_some() {
            return Err(TraceSpoolError::OwnerPaused);
        }
        let candidate = disk
            .attempts
            .iter()
            .find(|attempt| attempt.manifest.attempt_id == *attempt_id)
            .ok_or(TraceSpoolError::AttemptNotFound)?;
        if self
            .load_version_gates()?
            .client_version_hashes
            .contains(&candidate.manifest.client_version_hash)
        {
            return Err(TraceSpoolError::AttemptNotClaimable);
        }
        let stored = disk
            .attempts
            .iter_mut()
            .find(|attempt| attempt.manifest.attempt_id == *attempt_id)
            .ok_or(TraceSpoolError::AttemptNotFound)?;
        if !matches!(stored.state, AttemptState::Pending) {
            return Err(TraceSpoolError::AttemptNotClaimable);
        }
        let snapshot = stored.manifest.items.clone();
        let claimable = snapshot.iter().all(|item| {
            disk.items.iter().any(|current| {
                current.item.key == item.key
                    && current.item.revision == item.revision
                    && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == attempt_id)
            })
        });
        if !claimable {
            let stored = disk
                .attempts
                .iter_mut()
                .find(|attempt| attempt.manifest.attempt_id == *attempt_id)
                .unwrap();
            stored.state = AttemptState::Retired;
            for current in &mut disk.items {
                if matches!(&current.delivery, ItemDelivery::Sealed(id) if id == attempt_id) {
                    current.delivery = ItemDelivery::NeedsSeal;
                }
            }
            self.persist(&disk)?;
            return Err(TraceSpoolError::AttemptNotClaimable);
        }
        stored.state = AttemptState::Inflight;
        let handle = InflightAttemptHandle {
            manifest: stored.manifest.clone(),
            attempt_count: stored.attempt_count,
        };
        self.persist(&disk)?;
        Ok(handle)
    }

    pub(crate) fn apply_accepted_cas(
        &self,
        handle: &InflightAttemptHandle,
        accepted: &[TraceItemKey],
    ) -> Result<CasMutationResult, TraceSpoolError> {
        validate_handle_keys(handle, accepted, false)?;
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle(&disk, handle, |state| {
            matches!(state, AttemptState::Inflight)
        })?;
        let mut matched = 0;
        let mut stale = 0;
        for key in accepted {
            let snapshot = handle.items().iter().find(|item| item.key == *key).unwrap();
            if let Some(index) = disk.items.iter().position(|current| current.item.key == *key && current.item.revision == snapshot.revision && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())) {
                disk.items.remove(index); matched += 1;
            } else { stale += 1; }
        }
        self.persist(&disk)?;
        Ok(CasMutationResult {
            matched_items: matched,
            stale_items: stale,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_validated_ack_cas(
        &self,
        handle: &InflightAttemptHandle,
        accepted: &[TraceItemKey],
        rejected: &[TraceItemKey],
        unacknowledged: &[TraceItemKey],
        credential_rejected: &[TraceItemKey],
        next_attempt_at_ms: u64,
    ) -> Result<ValidatedAckMutationResult, TraceSpoolError> {
        validate_ack_partition(
            handle,
            accepted,
            rejected,
            unacknowledged,
            credential_rejected,
        )?;
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle(&disk, handle, |state| {
            matches!(state, AttemptState::Inflight)
        })?;

        let mut matched = 0;
        let mut stale = 0;
        for key in accepted {
            let snapshot = handle.items().iter().find(|item| item.key == *key).unwrap();
            if let Some(index) = disk.items.iter().position(|current| {
                current.item.key == *key
                    && current.item.revision == snapshot.revision
                    && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
            }) {
                disk.items.remove(index);
                matched += 1;
            } else {
                stale += 1;
            }
        }

        let credential_set = credential_rejected.iter().cloned().collect::<HashSet<_>>();
        let mut remediation = Vec::new();
        for key in rejected.iter().chain(unacknowledged) {
            let snapshot = handle.items().iter().find(|item| item.key == *key).unwrap();
            let Some(current) = disk.items.iter_mut().find(|current| {
                current.item.key == *key
                    && current.item.revision == snapshot.revision
                    && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
            }) else {
                continue;
            };
            if credential_set.contains(key) {
                remediation.push(key.clone());
            } else {
                current.delivery = ItemDelivery::NeedsSeal;
                current.next_attempt_at_ms = next_attempt_at_ms;
            }
        }
        validate_handle_mut(&mut disk, handle, |state| {
            matches!(state, AttemptState::Inflight)
        })?
        .state = if remediation.is_empty() {
            AttemptState::Retired
        } else {
            AttemptState::NeedsRemediation(remediation.clone())
        };
        self.persist(&disk)?;
        Ok(ValidatedAckMutationResult {
            accepted: CasMutationResult {
                matched_items: matched,
                stale_items: stale,
            },
            remediation,
        })
    }

    pub(crate) fn retire_attempt_and_mark_reseal_cas(
        &self,
        handle: &InflightAttemptHandle,
        next_attempt_at_ms: u64,
        reason: ResealReason,
    ) -> Result<CasMutationResult, TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        let affected = {
            let attempt = validate_handle_mut(&mut disk, handle, |state| {
                matches!(
                    state,
                    AttemptState::Inflight | AttemptState::NeedsRemediation(_)
                )
            })?;
            match &attempt.state {
                AttemptState::NeedsRemediation(keys) => keys.clone(),
                _ => vec![],
            }
        };
        let mut matched = 0;
        let mut stale = 0;
        for snapshot in handle.items() {
            if affected.contains(&snapshot.key) {
                continue;
            }
            if let Some(current) = disk.items.iter_mut().find(|current| current.item.key == snapshot.key && current.item.revision == snapshot.revision && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())) {
                if reason == ResealReason::Retryable { current.attempt_count = current.attempt_count.checked_add(1).ok_or(TraceSpoolError::AttemptOverflow)?; }
                current.delivery = ItemDelivery::NeedsSeal; current.next_attempt_at_ms = next_attempt_at_ms; matched += 1;
            } else { stale += 1; }
        }
        if affected.is_empty() {
            validate_handle_mut(&mut disk, handle, |_| true)?.state = AttemptState::Retired;
        }
        self.persist(&disk)?;
        Ok(CasMutationResult {
            matched_items: matched,
            stale_items: stale,
        })
    }

    pub(crate) fn mark_needs_remediation(
        &self,
        handle: &InflightAttemptHandle,
        affected: &[TraceItemKey],
    ) -> Result<Vec<TraceItemKey>, TraceSpoolError> {
        validate_handle_keys(handle, affected, true)?;
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle(&disk, handle, |state| {
            matches!(state, AttemptState::Inflight)
        })?;
        let current = affected
            .iter()
            .filter(|key| {
                let snapshot = handle.items().iter().find(|item| item.key == **key).unwrap();
                disk.items.iter().any(|item| {
                    item.item.key == **key
                        && item.item.revision == snapshot.revision
                        && matches!(&item.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if !current.is_empty() {
            validate_handle_mut(&mut disk, handle, |state| {
                matches!(state, AttemptState::Inflight)
            })?
            .state = AttemptState::NeedsRemediation(current.clone());
            self.persist(&disk)?;
        }
        Ok(current)
    }

    pub(crate) fn register_remediated_attempt(
        &self,
        handle: &InflightAttemptHandle,
        incoming: SealedAttemptManifest,
        affected: &[TraceItemKey],
    ) -> Result<(), TraceSpoolError> {
        validate_attempt(&incoming)?;
        validate_handle_keys(handle, affected, true)?;
        if incoming.owner != *handle.owner() || incoming.items.len() != affected.len() {
            return Err(TraceSpoolError::InvalidRevision);
        }
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        if self
            .load_version_gates()?
            .client_version_hashes
            .contains(&incoming.client_version_hash)
        {
            return Err(TraceSpoolError::ClientVersionBlocked);
        }
        if disk.attempts.iter().any(|attempt| {
            attempt.manifest.attempt_id == incoming.attempt_id
                || attempt.manifest.sealed_upload_id == incoming.sealed_upload_id
        }) {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        let stored_affected = match &validate_handle(&disk, handle, |state| {
            matches!(state, AttemptState::NeedsRemediation(_))
        })?
        .state
        {
            AttemptState::NeedsRemediation(keys) => keys,
            _ => unreachable!(),
        };
        if as_set(stored_affected) != as_set(affected)
            || as_set(
                &incoming
                    .items
                    .iter()
                    .map(|item| item.key.clone())
                    .collect::<Vec<_>>(),
            ) != as_set(affected)
        {
            return Err(TraceSpoolError::InvalidRevision);
        }
        let mut max_count = 0;
        for replacement in &incoming.items {
            let snapshot = handle
                .items()
                .iter()
                .find(|item| item.key == replacement.key)
                .ok_or(TraceSpoolError::InvalidRevision)?;
            let current = disk
                .items
                .iter()
                .find(|current| current.item.key == replacement.key)
                .ok_or(TraceSpoolError::InvalidRevision)?;
            if current.item.revision != snapshot.revision
                || replacement.revision
                    != snapshot
                        .revision
                        .checked_add(1)
                        .ok_or(TraceSpoolError::InvalidRevision)?
                || !same_metadata(snapshot, replacement)
            {
                return Err(TraceSpoolError::InvalidRevision);
            }
            max_count = max_count.max(current.attempt_count);
        }
        for current in &mut disk.items {
            if let Some(replacement) = incoming
                .items
                .iter()
                .find(|item| item.key == current.item.key)
            {
                current.item = replacement.clone();
                current.delivery = ItemDelivery::Sealed(incoming.attempt_id.clone());
                current.client_version_hash = incoming.client_version_hash.clone();
                current.next_attempt_at_ms = 0;
            } else if handle.items().iter().any(|snapshot| {
                snapshot.key == current.item.key && snapshot.revision == current.item.revision
            }) && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
            {
                current.delivery = ItemDelivery::NeedsSeal;
            }
        }
        validate_handle_mut(&mut disk, handle, |_| true)?.state = AttemptState::Retired;
        disk.attempts.push(StoredAttempt {
            manifest: incoming,
            state: AttemptState::Pending,
            attempt_count: max_count,
            next_attempt_at_ms: 0,
        });
        self.persist(&disk)
    }

    pub(crate) fn pause_owner(
        &self,
        handle: &InflightAttemptHandle,
        reason: OwnerPauseReason,
    ) -> Result<(), TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle(&disk, handle, |state| {
            matches!(
                state,
                AttemptState::Inflight | AttemptState::NeedsRemediation(_)
            )
        })?;
        for current in &mut disk.items {
            if handle.items().iter().any(|snapshot| {
                snapshot.key == current.item.key && snapshot.revision == current.item.revision
            }) && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
            {
                current.delivery = ItemDelivery::NeedsSeal;
            }
        }
        validate_handle_mut(&mut disk, handle, |_| true)?.state = AttemptState::Retired;
        disk.owner_pause = Some(reason);
        self.persist(&disk)
    }

    pub(crate) fn pause_client_version_for_update(
        &self,
        handle: &InflightAttemptHandle,
    ) -> Result<(), TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle(&disk, handle, |state| {
            matches!(
                state,
                AttemptState::Inflight | AttemptState::NeedsRemediation(_)
            )
        })?;
        let mut gates = self.load_version_gates()?;
        if !gates
            .client_version_hashes
            .contains(handle.client_version_hash())
        {
            gates
                .client_version_hashes
                .push(handle.client_version_hash().clone());
            self.persist_version_gates(&gates)?;
        }
        for current in &mut disk.items {
            if handle.items().iter().any(|snapshot| {
                snapshot.key == current.item.key && snapshot.revision == current.item.revision
            }) && matches!(&current.delivery, ItemDelivery::Sealed(id) if id == handle.attempt_id())
            {
                current.delivery = ItemDelivery::NeedsSeal;
            }
        }
        validate_handle_mut(&mut disk, handle, |_| true)?.state = AttemptState::Retired;
        reconcile_blocked_attempts(&mut disk, &gates);
        self.persist(&disk)?;
        let blocked = handle.client_version_hash().clone();
        for path in self.manifest_paths()? {
            let mut other: DiskManifest =
                serde_json::from_slice(&self.read_bounded(&path, MAX_MANIFEST_BYTES)?)?;
            if other.items.len() > MAX_MANIFEST_ITEMS || other.attempts.len() > MAX_STORED_ATTEMPTS
            {
                return Err(TraceSpoolError::InvalidAttempt);
            }
            if other.owner == *handle.owner() {
                continue;
            }
            let retired = other
                .attempts
                .iter_mut()
                .filter(|attempt| {
                    matches!(attempt.state, AttemptState::Pending)
                        && attempt.manifest.client_version_hash == blocked
                })
                .map(|attempt| {
                    attempt.state = AttemptState::Retired;
                    attempt.manifest.attempt_id.clone()
                })
                .collect::<Vec<_>>();
            if retired.is_empty() {
                continue;
            }
            for current in &mut other.items {
                if matches!(&current.delivery, ItemDelivery::Sealed(id) if retired.contains(id)) {
                    current.delivery = ItemDelivery::NeedsSeal;
                }
            }
            self.persist(&other)?;
        }
        Ok(())
    }

    pub(crate) fn pause_attempt(
        &self,
        handle: &InflightAttemptHandle,
        reason: AttemptPauseReason,
    ) -> Result<(), TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(handle.owner())?;
        validate_handle_mut(&mut disk, handle, |state| {
            matches!(state, AttemptState::Inflight)
        })?
        .state = AttemptState::Paused(reason);
        self.persist(&disk)
    }

    pub(crate) fn expire(
        &self,
        owner: &TraceOwnerGeneration,
        now_ms: u64,
    ) -> Result<Vec<TraceLossDiagnostic>, TraceSpoolError> {
        let _guard = self.lock()?;
        let mut disk = self.load(owner)?;
        let expired = disk
            .items
            .iter()
            .filter(|item| {
                now_ms.saturating_sub(item.item.created_at_ms) >= TRACE_SPOOL_RETENTION_MS
            })
            .map(|item| item.item.trace_id.clone())
            .collect::<BTreeSet<_>>();
        if expired.is_empty() {
            return Ok(Vec::new());
        }
        let mut losses = vec![];
        for trace_id in &expired {
            let related_attempts = disk
                .attempts
                .iter()
                .filter(|attempt| {
                    attempt
                        .manifest
                        .items
                        .iter()
                        .any(|item| item.trace_id == *trace_id)
                })
                .count();
            let loss = TraceLossDiagnostic {
                scope_hash: owner.scope_hash(),
                trace_id: trace_id.clone(),
                item_count: disk
                    .items
                    .iter()
                    .filter(|item| item.item.trace_id == *trace_id)
                    .count(),
                attempt_count: related_attempts,
                expired_at_ms: now_ms,
                reason: RETENTION_REASON.to_string(),
            };
            self.persist_loss(owner, &loss)?;
            losses.push(loss);
        }
        disk.items
            .retain(|item| !expired.contains(&item.item.trace_id));
        let removed_attempts = disk
            .attempts
            .iter()
            .filter(|attempt| {
                attempt
                    .manifest
                    .items
                    .iter()
                    .any(|item| expired.contains(&item.trace_id))
            })
            .map(|attempt| attempt.manifest.attempt_id.clone())
            .collect::<Vec<_>>();
        disk.attempts
            .retain(|attempt| !removed_attempts.contains(&attempt.manifest.attempt_id));
        for current in &mut disk.items {
            if matches!(&current.delivery, ItemDelivery::Sealed(id) if removed_attempts.contains(id))
            {
                current.delivery = ItemDelivery::NeedsSeal;
            }
        }
        self.persist(&disk)?;
        Ok(losses)
    }

    fn recover_orphan_attempts(&self, disk: &mut DiskManifest) -> Result<bool, TraceSpoolError> {
        let orphaned_trace_ids = disk
            .attempts
            .iter()
            .filter(|attempt| {
                matches!(
                    &attempt.state,
                    AttemptState::Pending
                        | AttemptState::Inflight
                        | AttemptState::NeedsRemediation(_)
                        | AttemptState::Paused(_)
                )
            })
            .flat_map(|attempt| attempt.manifest.items.iter().map(|item| item.trace_id.clone()))
            .collect::<BTreeSet<_>>();
        if orphaned_trace_ids.is_empty() {
            return Ok(false);
        }

        let existing_tombstones = self.tombstones(&disk.owner)?;
        let loss_time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        for trace_id in &orphaned_trace_ids {
            if existing_tombstones.contains(trace_id) {
                continue;
            }
            let loss = TraceLossDiagnostic {
                scope_hash: disk.owner.scope_hash(),
                trace_id: trace_id.clone(),
                item_count: disk
                    .items
                    .iter()
                    .filter(|item| item.item.trace_id == *trace_id)
                    .count(),
                attempt_count: disk
                    .attempts
                    .iter()
                    .filter(|attempt| {
                        attempt
                            .manifest
                            .items
                            .iter()
                            .any(|item| item.trace_id == *trace_id)
                    })
                    .count(),
                expired_at_ms: loss_time_ms,
                reason: STARTUP_ORPHAN_REASON.to_string(),
            };
            self.persist_loss(&disk.owner, &loss)?;
        }

        disk.items
            .retain(|item| !orphaned_trace_ids.contains(&item.item.trace_id));
        disk.attempts.retain(|attempt| {
            !attempt
                .manifest
                .items
                .iter()
                .any(|item| orphaned_trace_ids.contains(&item.trace_id))
        });
        Ok(true)
    }
    fn recover_inflight(&self, disk: &mut DiskManifest) -> bool {
        let ids = disk
            .attempts
            .iter()
            .filter(|attempt| matches!(attempt.state, AttemptState::Inflight))
            .map(|attempt| attempt.manifest.attempt_id.clone())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return false;
        }
        for id in &ids {
            if let Some(attempt) = disk
                .attempts
                .iter_mut()
                .find(|attempt| attempt.manifest.attempt_id == *id)
            {
                attempt.state = AttemptState::Retired;
            }
            for item in &mut disk.items {
                if matches!(&item.delivery, ItemDelivery::Sealed(bound) if bound == id) {
                    item.delivery = ItemDelivery::NeedsSeal;
                }
            }
        }
        true
    }

    fn recover_startup(&self) -> Result<(), TraceSpoolError> {
        self.ensure_under_root(&self.root)?;
        for user in fs::read_dir(&self.root)? {
            let user = user?.path();
            let metadata = fs::symlink_metadata(&user)?;
            if !metadata.is_dir() || is_reparse_point(&metadata) {
                if metadata.is_dir() {
                    return Err(TraceSpoolError::UnsafePath);
                }
                continue;
            }
            for generation in fs::read_dir(user)? {
                let generation = generation?.path();
                let metadata = fs::symlink_metadata(&generation)?;
                if !metadata.is_dir() || is_reparse_point(&metadata) {
                    if metadata.is_dir() {
                        return Err(TraceSpoolError::UnsafePath);
                    }
                    continue;
                }
                let path = generation.join("manifest.json");
                if !path.is_file() {
                    continue;
                }
                let mut disk: DiskManifest =
                    serde_json::from_slice(&self.read_bounded(&path, MAX_MANIFEST_BYTES)?)?;
                if disk.items.len() > MAX_MANIFEST_ITEMS
                    || disk.attempts.len() > MAX_STORED_ATTEMPTS
                {
                    return Err(TraceSpoolError::InvalidAttempt);
                }
                if disk.version != MANIFEST_VERSION {
                    return Err(TraceSpoolError::ScopeMismatch);
                }
                let tombstones = self.tombstones(&disk.owner)?;
                disk.items
                    .retain(|item| !tombstones.contains(item.item.trace_id()));
                disk.attempts.retain(|attempt| {
                    !attempt
                        .manifest
                        .items
                        .iter()
                        .any(|item| tombstones.contains(item.trace_id()))
                });
                let gates = self.load_version_gates()?;
                let orphaned = self.recover_orphan_attempts(&mut disk)?;
                if orphaned
                    || self.recover_inflight(&mut disk)
                    || reconcile_blocked_attempts(&mut disk, &gates)
                {
                    self.persist(&disk)?;
                }
            }
        }
        Ok(())
    }

    fn manifest_paths(&self) -> Result<Vec<PathBuf>, TraceSpoolError> {
        self.ensure_under_root(&self.root)?;
        let mut paths = Vec::new();
        for user in fs::read_dir(&self.root)? {
            let user = user?.path();
            let metadata = fs::symlink_metadata(&user)?;
            if !metadata.is_dir() || is_reparse_point(&metadata) {
                if metadata.is_dir() {
                    return Err(TraceSpoolError::UnsafePath);
                }
                continue;
            }
            for generation in fs::read_dir(user)? {
                let generation = generation?.path();
                let metadata = fs::symlink_metadata(&generation)?;
                if !metadata.is_dir() || is_reparse_point(&metadata) {
                    if metadata.is_dir() {
                        return Err(TraceSpoolError::UnsafePath);
                    }
                    continue;
                }
                let path = generation.join("manifest.json");
                if path.is_file() {
                    paths.push(path);
                }
            }
        }
        Ok(paths)
    }

    fn lock(&self) -> Result<RootMutationGuard<'_>, TraceSpoolError> {
        let in_process = self
            .state
            .mutation
            .lock()
            .map_err(|_| TraceSpoolError::InvalidAttempt)?;
        let root = DirectoryAnchor::open_existing(&self.root)?;
        let file_lock = RootFileLock::acquire(&root)?;
        Ok(RootMutationGuard {
            _in_process: in_process,
            _root: root,
            _file_lock: file_lock,
        })
    }

    fn directory_chain(
        &self,
        directory: &Path,
        create_missing: bool,
    ) -> Result<DirectoryChain, TraceSpoolError> {
        if !directory.starts_with(&self.root) {
            return Err(TraceSpoolError::UnsafePath);
        }
        let relative = directory
            .strip_prefix(&self.root)
            .map_err(|_| TraceSpoolError::UnsafePath)?;
        let mut anchors = vec![DirectoryAnchor::open_existing(&self.root)?];
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(TraceSpoolError::UnsafePath);
            };
            let name = name.to_str().ok_or(TraceSpoolError::UnsafePath)?;
            let parent = anchors
                .last()
                .expect("root directory anchor is always present");
            let next = if create_missing {
                parent.create_or_open_child(name)?
            } else {
                parent.open_child(name)?
            };
            anchors.push(next);
        }
        Ok(DirectoryChain { anchors })
    }

    fn ensure_directory(&self, directory: &Path) -> Result<DirectoryChain, TraceSpoolError> {
        self.directory_chain(directory, true)
    }

    fn ensure_write_destination(&self, destination: &Path) -> Result<(), TraceSpoolError> {
        let parent = destination.parent().ok_or(TraceSpoolError::UnsafePath)?;
        self.ensure_under_root(parent)?;
        assert_safe_existing_ancestors(parent)?;
        assert_safe_file_if_present(destination)
    }

    fn ensure_under_root(&self, path: &Path) -> Result<(), TraceSpoolError> {
        if !path.starts_with(&self.root) {
            return Err(TraceSpoolError::UnsafePath);
        }
        assert_safe_existing_ancestors(path)
    }

    fn read_bounded(&self, path: &Path, limit: u64) -> Result<Vec<u8>, TraceSpoolError> {
        let parent = path.parent().ok_or(TraceSpoolError::UnsafePath)?;
        let _parent = self.directory_chain(parent, false)?;
        read_bounded(path, limit)
    }

    fn directory(&self, owner: &TraceOwnerGeneration) -> PathBuf {
        self.root
            .join(owner.directory_hash())
            .join(owner.login_generation.to_string())
    }
    fn manifest_path(&self, owner: &TraceOwnerGeneration) -> PathBuf {
        self.directory(owner).join("manifest.json")
    }
    fn loss_directory(&self, owner: &TraceOwnerGeneration) -> PathBuf {
        self.directory(owner).join("loss")
    }

    fn load(&self, owner: &TraceOwnerGeneration) -> Result<DiskManifest, TraceSpoolError> {
        let tombstones = self.tombstones(owner)?;
        let manifest_path = self.manifest_path(owner);
        let mut disk = match self.read_bounded(&manifest_path, MAX_MANIFEST_BYTES) {
            Ok(bytes) => {
                let disk: DiskManifest = serde_json::from_slice(&bytes)?;
                if disk.items.len() > MAX_MANIFEST_ITEMS
                    || disk.attempts.len() > MAX_STORED_ATTEMPTS
                {
                    return Err(TraceSpoolError::InvalidAttempt);
                }
                disk
            }
            Err(TraceSpoolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                DiskManifest::empty(owner)
            }
            Err(error) => return Err(error),
        };
        if disk.version != MANIFEST_VERSION || disk.owner != *owner {
            return Err(TraceSpoolError::ScopeMismatch);
        }
        disk.items
            .retain(|item| !tombstones.contains(item.item.trace_id()));
        disk.attempts.retain(|attempt| {
            !attempt
                .manifest
                .items
                .iter()
                .any(|item| tombstones.contains(item.trace_id()))
        });
        Ok(disk)
    }

    fn tombstones(
        &self,
        owner: &TraceOwnerGeneration,
    ) -> Result<BTreeSet<String>, TraceSpoolError> {
        let loss_dir = self.loss_directory(owner);
        self.ensure_under_root(&loss_dir)?;
        if let Ok(metadata) = fs::symlink_metadata(&loss_dir) {
            if !metadata.is_dir() || is_reparse_point(&metadata) {
                return Err(TraceSpoolError::UnsafePath);
            }
        }
        let entries = match fs::read_dir(loss_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeSet::new())
            }
            Err(error) => return Err(error.into()),
        };
        let mut found = BTreeSet::new();
        for entry in entries {
            if found.len() >= MAX_TOMBSTONES {
                return Err(TraceSpoolError::InvalidAttempt);
            }
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let loss: TraceLossDiagnostic =
                serde_json::from_slice(&self.read_bounded(&path, MAX_LOSS_BYTES)?)?;
            if loss.scope_hash != owner.scope_hash()
                || !matches!(loss.reason.as_str(), RETENTION_REASON | STARTUP_ORPHAN_REASON)
            {
                return Err(TraceSpoolError::ScopeMismatch);
            }
            found.insert(loss.trace_id);
        }
        Ok(found)
    }

    fn persist(&self, disk: &DiskManifest) -> Result<(), TraceSpoolError> {
        if disk.items.len() > MAX_MANIFEST_ITEMS || disk.attempts.len() > MAX_STORED_ATTEMPTS {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        let directory = self.directory(&disk.owner);
        let _directory = self.ensure_directory(&directory)?;
        let bytes = serde_json::to_vec(disk)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        self.atomic_replace(
            &directory.join("manifest.json"),
            &bytes,
            AtomicTarget::Manifest,
        )
    }
    fn persist_loss(
        &self,
        owner: &TraceOwnerGeneration,
        loss: &TraceLossDiagnostic,
    ) -> Result<(), TraceSpoolError> {
        let directory = self.loss_directory(owner);
        let _directory = self.ensure_directory(&directory)?;
        let bytes = serde_json::to_vec(loss)?;
        if bytes.len() as u64 > MAX_LOSS_BYTES {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        self.atomic_replace(
            &directory.join(format!(
                "{}.json",
                hex(&Sha256::digest(loss.trace_id.as_bytes()))
            )),
            &bytes,
            AtomicTarget::Loss,
        )
    }
    fn load_version_gates(&self) -> Result<VersionGates, TraceSpoolError> {
        match self.read_bounded(
            &self.root.join("version-gates.json"),
            MAX_VERSION_GATE_BYTES,
        ) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(TraceSpoolError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(VersionGates::default())
            }
            Err(error) => Err(error),
        }
    }
    fn persist_version_gates(&self, gates: &VersionGates) -> Result<(), TraceSpoolError> {
        if gates.client_version_hashes.len() > MAX_STORED_ATTEMPTS {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        let bytes = serde_json::to_vec(gates)?;
        if bytes.len() as u64 > MAX_VERSION_GATE_BYTES {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        self.atomic_replace(
            &self.root.join("version-gates.json"),
            &bytes,
            AtomicTarget::VersionGate,
        )
    }
    fn atomic_replace(
        &self,
        destination: &Path,
        bytes: &[u8],
        target: AtomicTarget,
    ) -> Result<(), TraceSpoolError> {
        let parent = destination.parent().ok_or(TraceSpoolError::UnsafePath)?;
        let _directory = self.directory_chain(parent, false)?;
        self.ensure_write_destination(destination)?;
        let seq = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TraceSpoolError::InvalidAttempt)?;
        let temporary =
            destination.with_file_name(format!(".{name}.tmp-{}-{seq}", std::process::id()));
        let result = (|| {
            self.ensure_write_destination(&temporary)?;
            let mut file = create_new_regular_file(&temporary)?;
            self.ensure_write_destination(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            self.maybe_fail(target)?;
            self.ensure_write_destination(destination)?;
            self.ensure_write_destination(&temporary)?;
            replace_file(&temporary, destination)?;
            self.ensure_write_destination(destination)?;
            Ok(())
        })();
        if result.is_err() && self.ensure_write_destination(&temporary).is_ok() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    #[cfg(test)]
    fn maybe_fail(&self, target: AtomicTarget) -> Result<(), TraceSpoolError> {
        let fail = match target {
            AtomicTarget::Manifest => self.fail_manifest.swap(false, Ordering::SeqCst),
            AtomicTarget::Loss => self.fail_loss.swap(false, Ordering::SeqCst),
            AtomicTarget::VersionGate => false,
        };
        if fail {
            Err(TraceSpoolError::InjectedAtomicReplace)
        } else {
            Ok(())
        }
    }
    #[cfg(not(test))]
    fn maybe_fail(&self, _target: AtomicTarget) -> Result<(), TraceSpoolError> {
        Ok(())
    }
    #[cfg(test)]
    fn fail_next_manifest_replace_for_test(&self) {
        self.fail_manifest.store(true, Ordering::SeqCst);
    }
    #[cfg(test)]
    fn fail_next_loss_write_for_test(&self) {
        self.fail_loss.store(true, Ordering::SeqCst);
    }
    #[cfg(test)]
    pub(crate) fn current_revision_for_test(
        &self,
        owner: &TraceOwnerGeneration,
        key: &TraceItemKey,
    ) -> Option<u64> {
        self.load(owner)
            .ok()?
            .items
            .into_iter()
            .find(|item| item.item.key == *key)
            .map(|item| item.item.revision)
    }
    #[cfg(test)]
    fn due_attempts_without_expiry_for_test(
        &self,
        owner: &TraceOwnerGeneration,
    ) -> Vec<DueSealedAttempt> {
        let disk = self.load(owner).unwrap();
        disk.attempts
            .into_iter()
            .filter(|attempt| matches!(attempt.state, AttemptState::Pending))
            .map(|attempt| DueSealedAttempt {
                manifest: attempt.manifest,
                attempt_count: attempt.attempt_count,
                next_attempt_at_ms: attempt.next_attempt_at_ms,
            })
            .collect()
    }
    #[cfg(test)]
    fn loss_paths_for_test(&self, owner: &TraceOwnerGeneration) -> Vec<PathBuf> {
        fs::read_dir(self.loss_directory(owner))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RegisterMode {
    NewRevision,
    Reseal,
}
#[derive(Clone, Copy)]
enum AtomicTarget {
    Manifest,
    Loss,
    VersionGate,
}

fn validate_attempt(attempt: &SealedAttemptManifest) -> Result<(), TraceSpoolError> {
    if attempt.items.is_empty()
        || attempt.wire_bytes > TRACE_SPOOL_MAX_WIRE_BYTES
        || attempt.run_count > TRACE_SPOOL_MAX_RUNS
        || attempt.event_count > TRACE_SPOOL_MAX_EVENTS
        || attempt.chunk_count > TRACE_SPOOL_MAX_CHUNKS
    {
        return Err(TraceSpoolError::InvalidAttempt);
    }
    let actual_runs = attempt
        .items
        .iter()
        .filter(|item| item.key.entity == TraceSpoolEntity::Run)
        .count();
    let actual_events = attempt
        .items
        .iter()
        .filter(|item| item.key.entity == TraceSpoolEntity::Event)
        .count();
    let actual_chunks = attempt
        .items
        .iter()
        .filter(|item| item.key.entity == TraceSpoolEntity::OutputChunk)
        .count();
    if usize::from(attempt.run_count) != actual_runs
        || usize::from(attempt.event_count) != actual_events
        || usize::from(attempt.chunk_count) != actual_chunks
    {
        return Err(TraceSpoolError::InvalidAttempt);
    }
    let mut ids = HashSet::new();
    for item in &attempt.items {
        if item.key.item_id.is_empty()
            || item.trace_id.is_empty()
            || item.revision == 0
            || !ids.insert(item.key.item_id.as_str())
        {
            return Err(TraceSpoolError::InvalidAttempt);
        }
        let parent_valid = match item.key.entity {
            TraceSpoolEntity::Run => item.parent.is_none(),
            TraceSpoolEntity::Event => {
                matches!(&item.parent, Some(parent) if parent.entity == TraceSpoolEntity::Run)
            }
            TraceSpoolEntity::OutputChunk => {
                matches!(&item.parent, Some(parent) if parent.entity == TraceSpoolEntity::Event)
            }
        };
        if !parent_valid {
            return Err(TraceSpoolError::InvalidAttempt);
        }
    }
    Ok(())
}
fn same_metadata(old: &SealedItemRevision, new: &SealedItemRevision) -> bool {
    old.key == new.key
        && old.trace_id == new.trace_id
        && old.parent == new.parent
        && old.created_at_ms == new.created_at_ms
}
fn reconcile_blocked_attempts(disk: &mut DiskManifest, gates: &VersionGates) -> bool {
    let retired = disk
        .attempts
        .iter_mut()
        .filter(|attempt| {
            matches!(attempt.state, AttemptState::Pending)
                && gates
                    .client_version_hashes
                    .contains(&attempt.manifest.client_version_hash)
        })
        .map(|attempt| {
            attempt.state = AttemptState::Retired;
            attempt.manifest.attempt_id.clone()
        })
        .collect::<Vec<_>>();
    if retired.is_empty() {
        return false;
    }
    for current in &mut disk.items {
        if matches!(&current.delivery, ItemDelivery::Sealed(id) if retired.contains(id)) {
            current.delivery = ItemDelivery::NeedsSeal;
        }
    }
    true
}
fn as_set(keys: &[TraceItemKey]) -> HashSet<TraceItemKey> {
    keys.iter().cloned().collect()
}

fn validate_ack_partition(
    handle: &InflightAttemptHandle,
    accepted: &[TraceItemKey],
    rejected: &[TraceItemKey],
    unacknowledged: &[TraceItemKey],
    credential_rejected: &[TraceItemKey],
) -> Result<(), TraceSpoolError> {
    let accepted_set = as_set(accepted);
    let rejected_set = as_set(rejected);
    let unacknowledged_set = as_set(unacknowledged);
    let credential_set = as_set(credential_rejected);
    let dispatched = handle
        .items()
        .iter()
        .map(|item| item.key.clone())
        .collect::<HashSet<_>>();
    let union = accepted_set
        .union(&rejected_set)
        .cloned()
        .collect::<HashSet<_>>()
        .union(&unacknowledged_set)
        .cloned()
        .collect::<HashSet<_>>();
    if accepted_set.len() != accepted.len()
        || rejected_set.len() != rejected.len()
        || unacknowledged_set.len() != unacknowledged.len()
        || credential_set.len() != credential_rejected.len()
        || !accepted_set.is_disjoint(&rejected_set)
        || !accepted_set.is_disjoint(&unacknowledged_set)
        || !rejected_set.is_disjoint(&unacknowledged_set)
        || union != dispatched
        || !credential_set.is_subset(&rejected_set)
        || credential_set
            .iter()
            .any(|key| key.entity != TraceSpoolEntity::OutputChunk)
    {
        return Err(TraceSpoolError::InvalidHandle);
    }
    Ok(())
}

fn validate_handle_keys(
    handle: &InflightAttemptHandle,
    keys: &[TraceItemKey],
    nonempty: bool,
) -> Result<(), TraceSpoolError> {
    let set = as_set(keys);
    if (nonempty && keys.is_empty())
        || set.len() != keys.len()
        || !keys
            .iter()
            .all(|key| handle.items().iter().any(|item| item.key == *key))
    {
        Err(TraceSpoolError::InvalidHandle)
    } else {
        Ok(())
    }
}
fn validate_handle<'a, F: Fn(&AttemptState) -> bool>(
    disk: &'a DiskManifest,
    handle: &InflightAttemptHandle,
    state: F,
) -> Result<&'a StoredAttempt, TraceSpoolError> {
    let attempt = disk
        .attempts
        .iter()
        .find(|attempt| attempt.manifest.attempt_id == *handle.attempt_id())
        .ok_or(TraceSpoolError::InvalidHandle)?;
    if attempt.manifest.owner != *handle.owner()
        || attempt.manifest.sealed_upload_id != *handle.sealed_upload_id()
        || attempt.manifest.items != handle.manifest.items
        || !state(&attempt.state)
    {
        return Err(TraceSpoolError::InvalidHandle);
    }
    Ok(attempt)
}
fn validate_handle_mut<'a, F: Fn(&AttemptState) -> bool>(
    disk: &'a mut DiskManifest,
    handle: &InflightAttemptHandle,
    state: F,
) -> Result<&'a mut StoredAttempt, TraceSpoolError> {
    let attempt = disk
        .attempts
        .iter_mut()
        .find(|attempt| attempt.manifest.attempt_id == *handle.attempt_id())
        .ok_or(TraceSpoolError::InvalidHandle)?;
    if attempt.manifest.owner != *handle.owner()
        || attempt.manifest.sealed_upload_id != *handle.sealed_upload_id()
        || attempt.manifest.items != handle.manifest.items
        || !state(&attempt.state)
    {
        return Err(TraceSpoolError::InvalidHandle);
    }
    Ok(attempt)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, TraceSpoolError> {
    let file = open_regular_file(path, false, false)?;
    let metadata = file.metadata()?;
    if metadata.len() > limit {
        return Err(TraceSpoolError::InvalidAttempt);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(TraceSpoolError::InvalidAttempt);
    }
    Ok(bytes)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    fs::rename(source, destination)?;
    if let Some(parent) = destination.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    fn owner(seed: u8, generation: u64) -> TraceOwnerGeneration {
        TraceOwnerGeneration::from_canonical_username_hash([seed; 32], generation)
    }
    fn version(seed: u8) -> ProtectionClientVersionHash {
        ProtectionClientVersionHash::from_digest([seed; 32])
    }

    #[cfg(windows)]
    #[test]
    fn cross_process_root_lock_holder() {
        let Ok(root) = std::env::var("NWFLASH_TRACE_SPOOL_LOCK_ROOT") else {
            return;
        };
        let ready = PathBuf::from(std::env::var("NWFLASH_TRACE_SPOOL_LOCK_READY").unwrap());
        let release = PathBuf::from(std::env::var("NWFLASH_TRACE_SPOOL_LOCK_RELEASE").unwrap());
        let root = PathBuf::from(root);
        fs::create_dir_all(&root).unwrap();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(root.join(".trace-spool.lock"))
            .unwrap();
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::LockFile;
        assert_ne!(
            unsafe { LockFile(file.as_raw_handle() as HANDLE, 0, 0, 1, 0) },
            0
        );
        fs::write(&ready, b"ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(windows)]
    #[test]
    fn cross_process_root_lock_blocks_open_and_fails_closed() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("spool");
        let ready = temp.path().join("ready");
        let release = temp.path().join("release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("trace_spool::tests::cross_process_root_lock_holder")
            .arg("--nocapture")
            .env("NWFLASH_TRACE_SPOOL_LOCK_ROOT", &root)
            .env("NWFLASH_TRACE_SPOOL_LOCK_READY", &ready)
            .env("NWFLASH_TRACE_SPOOL_LOCK_RELEASE", &release)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child process did not acquire the OS lock");
        let result = TraceSpoolStore::open(root);
        fs::write(&release, b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(result.is_err(), "root open bypassed the OS lock");
    }

    #[cfg(windows)]
    fn replace_with_junction(link: &Path, target: &Path) {
        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(output.status.success(), "mklink /J failed: {output:?}");
    }

    #[cfg(windows)]
    #[test]
    fn owner_generation_junction_after_open_fails_closed_without_writing_target() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(0x55, 7);
        let generation = store.directory(&current);
        let external = root.path().join("external");
        fs::create_dir_all(&generation).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::remove_dir(&generation).unwrap();
        replace_with_junction(&generation, &external);

        assert!(store.due_attempts(&current, 0).is_err());
        assert!(store
            .register_sealed_attempt(attempt(
                1,
                current,
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .is_err());
        assert!(!external.join("manifest.json").exists());
        assert!(!external.join("loss").exists());
    }

    #[cfg(windows)]
    #[test]
    fn root_junction_after_open_fails_closed_without_creating_an_external_lock() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(0x58, 10);
        let external = TempDir::new().unwrap();

        fs::remove_file(root.path().join(ROOT_LOCK_FILE)).unwrap();
        fs::remove_dir(root.path()).unwrap();
        replace_with_junction(root.path(), external.path());

        assert!(store.due_attempts(&current, 0).is_err());
        assert!(
            !external.path().join(ROOT_LOCK_FILE).exists(),
            "a post-open root junction must not receive the spool lock"
        );
        assert!(!external.path().join("manifest.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn missing_root_under_parent_junction_fails_closed_without_creating_external_state() {
        let temp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        let parent_junction = temp.path().join("parent_junction");
        replace_with_junction(&parent_junction, external.path());
        let root = parent_junction.join("missing").join("spool");

        assert!(
            TraceSpoolStore::open(root).is_err(),
            "a missing root must not be created beneath a junction"
        );
        assert!(!external.path().join("missing").exists());
        assert!(!external.path().join(ROOT_LOCK_FILE).exists());
        assert!(!external
            .path()
            .join("missing")
            .join(ROOT_LOCK_FILE)
            .exists());
        assert!(!external
            .path()
            .join("missing")
            .join("spool")
            .join("manifest.json")
            .exists());
    }

    #[cfg(windows)]
    #[test]
    fn replaced_loss_or_version_gate_paths_fail_closed_without_writing_target() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(0x56, 8);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        let external_loss = root.path().join("external-loss");
        let loss = store.loss_directory(&current);
        fs::create_dir_all(&loss).unwrap();
        fs::remove_dir(&loss).unwrap();
        fs::create_dir_all(&external_loss).unwrap();
        replace_with_junction(&loss, &external_loss);
        assert!(store.expire(&current, TRACE_SPOOL_RETENTION_MS).is_err());
        assert!(!external_loss.join("manifest.json").exists());
        assert!(!external_loss.join("loss").exists());

        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let external_gate = root.path().join("external-gate");
        fs::create_dir_all(&external_gate).unwrap();
        replace_with_junction(&root.path().join("version-gates.json"), &external_gate);
        assert!(store
            .register_sealed_attempt(attempt(
                2,
                owner(0x57, 9),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .is_err());
        assert!(!external_gate.join("manifest.json").exists());
    }

    fn item(
        entity: TraceSpoolEntity,
        id: &str,
        trace: &str,
        revision: u64,
        at: u64,
    ) -> SealedItemRevision {
        let parent = match entity {
            TraceSpoolEntity::Run => None,
            TraceSpoolEntity::Event => Some(TraceItemKey::new(TraceSpoolEntity::Run, "parent-run")),
            TraceSpoolEntity::OutputChunk => {
                Some(TraceItemKey::new(TraceSpoolEntity::Event, "parent-event"))
            }
        };
        SealedItemRevision::new(TraceItemKey::new(entity, id), trace, parent, revision, at)
    }

    fn attempt(
        seed: u8,
        owner: TraceOwnerGeneration,
        items: Vec<SealedItemRevision>,
    ) -> SealedAttemptManifest {
        attempt_version(seed, owner, items, 9)
    }

    fn attempt_version(
        seed: u8,
        owner: TraceOwnerGeneration,
        items: Vec<SealedItemRevision>,
        version: u8,
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
            ProtectionSealedUploadId::from_digest([seed.wrapping_add(64); 32]),
            ProtectionClientVersionHash::from_digest([version; 32]),
            512,
            runs,
            events,
            chunks,
            items,
        )
    }

    #[test]
    fn owner_generation_isolation_and_debug_are_hash_only() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let first = owner(0xab, 1);
        store
            .register_sealed_attempt(attempt(
                1,
                first.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        assert_eq!(store.due_attempts(&first, 0).unwrap().len(), 1);
        assert!(store.due_attempts(&owner(0xab, 2), 0).unwrap().is_empty());
        assert!(store.due_attempts(&owner(0xcd, 1), 0).unwrap().is_empty());
        assert!(!format!("{first:?}").contains(&"ab".repeat(32)));
        let first_dir = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| entry.path().is_dir())
            .unwrap()
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert_eq!(first_dir.len(), 64);
        assert!(first_dir.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn limits_duplicates_and_revision_rollback_jump_or_metadata_drift_fail() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        for revision in [1, 3] {
            assert!(store
                .register_sealed_attempt(attempt(
                    revision as u8 + 1,
                    current.clone(),
                    vec![item(TraceSpoolEntity::Run, "run", "trace", revision, 0)]
                ))
                .is_err());
        }
        assert!(store
            .register_sealed_attempt(attempt(
                7,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "changed-trace", 2, 0)]
            ))
            .is_err());
        assert!(store
            .register_sealed_attempt(attempt(
                8,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 2, 1)]
            ))
            .is_err());
        assert!(store
            .register_sealed_attempt(attempt(
                9,
                current.clone(),
                vec![
                    item(TraceSpoolEntity::Run, "dup", "x", 1, 0),
                    item(TraceSpoolEntity::Run, "dup", "x", 1, 0),
                ]
            ))
            .is_err());
        let mut too_large = attempt(
            10,
            current,
            vec![item(TraceSpoolEntity::Run, "new", "new", 1, 0)],
        );
        too_large.wire_bytes = TRACE_SPOOL_MAX_WIRE_BYTES + 1;
        assert!(store.register_sealed_attempt(too_large).is_err());
    }

    #[test]
    fn wrong_owner_and_double_claim_fail_durably() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let id = SealedAttemptId::from_digest([1; 32]);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        assert!(matches!(
            store.begin_dispatch(&id, &owner(1, 2)),
            Err(TraceSpoolError::AttemptNotFound)
        ));
        let handle = store.begin_dispatch(&id, &current).unwrap();
        assert_eq!(handle.owner(), &current);
        assert_eq!(handle.items()[0].revision(), 1);
        assert!(matches!(
            store.begin_dispatch(&id, &current),
            Err(TraceSpoolError::AttemptNotClaimable)
        ));
        assert!(store.due_attempts(&current, 0).unwrap().is_empty());
        assert!(store
            .due_reseal_items(&current, 0, &version(9))
            .unwrap()
            .is_empty());
        drop(handle);
        drop(store);
        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        assert!(reopened.due_attempts(&current, 0).unwrap().is_empty());
        assert_eq!(
            reopened
                .due_reseal_items(&current, 0, &version(9))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn old_open_ack_and_reseal_cannot_mutate_terminal_revision_two() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        let old = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 2, 0)],
            ))
            .unwrap();
        let accepted = store
            .apply_accepted_cas(&old, std::slice::from_ref(&key))
            .unwrap();
        assert_eq!((accepted.matched_items(), accepted.stale_items()), (0, 1));
        let reseal = store
            .retire_attempt_and_mark_reseal_cas(&old, 10, ResealReason::Retryable)
            .unwrap();
        assert_eq!((reseal.matched_items(), reseal.stale_items()), (0, 1));
        assert_eq!(store.current_revision_for_test(&current, &key), Some(2));
    }

    #[test]
    fn registering_revision_two_retires_obsolete_pending_body_and_releases_siblings() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(8, 1);
        let old_id = SealedAttemptId::from_digest([1; 32]);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![
                    item(TraceSpoolEntity::Run, "run", "trace", 1, 0),
                    item(TraceSpoolEntity::Event, "event", "trace", 1, 0),
                ],
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 2, 0)],
            ))
            .unwrap();
        assert!(matches!(
            store.begin_dispatch(&old_id, &current),
            Err(TraceSpoolError::AttemptNotClaimable)
        ));
        assert_eq!(store.due_attempts(&current, 0).unwrap().len(), 1);
        let reseal = store.due_reseal_items(&current, 0, &version(9)).unwrap();
        assert_eq!(reseal.len(), 1);
        assert_eq!(reseal[0].item().key().item_id(), "event");
    }

    #[test]
    fn accepted_parent_never_cascades_to_child() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let parent = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        let child = TraceItemKey::new(TraceSpoolEntity::Event, "event");
        let child_item =
            SealedItemRevision::new(child.clone(), "trace", Some(parent.clone()), 1, 0);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![
                    item(TraceSpoolEntity::Run, "run", "trace", 1, 0),
                    child_item,
                ],
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .apply_accepted_cas(&handle, std::slice::from_ref(&parent))
            .unwrap();
        assert_eq!(store.current_revision_for_test(&current, &parent), None);
        assert_eq!(store.current_revision_for_test(&current, &child), Some(1));
    }

    #[test]
    fn remediation_rejects_empty_id_drift_and_advances_exact_revision() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::OutputChunk, "chunk", "trace", 1, 0)],
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .mark_needs_remediation(&handle, std::slice::from_ref(&key))
            .unwrap();
        assert!(store
            .register_remediated_attempt(&handle, attempt(2, current.clone(), vec![]), &[])
            .is_err());
        assert!(store
            .register_remediated_attempt(
                &handle,
                attempt(
                    3,
                    current.clone(),
                    vec![item(TraceSpoolEntity::OutputChunk, "drift", "trace", 2, 0)]
                ),
                std::slice::from_ref(&key)
            )
            .is_err());
        store
            .register_remediated_attempt(
                &handle,
                attempt(
                    4,
                    current.clone(),
                    vec![item(TraceSpoolEntity::OutputChunk, "chunk", "trace", 2, 0)],
                ),
                std::slice::from_ref(&key),
            )
            .unwrap();
        assert_eq!(store.current_revision_for_test(&current, &key), Some(2));
    }

    #[test]
    fn remediation_outbox_survives_reopen_until_exact_revision_is_registered() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(5, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "chunk");
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::OutputChunk, "chunk", "trace", 1, 0)],
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .mark_needs_remediation(&handle, std::slice::from_ref(&key))
            .unwrap();
        drop(handle);
        drop(store);
        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let outbox = reopened.due_remediations(&current, &version(9)).unwrap();
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].affected(), std::slice::from_ref(&key));
        reopened
            .register_remediated_attempt(
                outbox[0].handle(),
                attempt(
                    2,
                    current.clone(),
                    vec![item(TraceSpoolEntity::OutputChunk, "chunk", "trace", 2, 0)],
                ),
                std::slice::from_ref(&key),
            )
            .unwrap();
        assert!(reopened
            .due_remediations(&current, &version(9))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn retry_retires_old_seal_and_requires_fresh_attempt_and_opaque_id() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let first_id = SealedAttemptId::from_digest([1; 32]);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        let handle = store.begin_dispatch(&first_id, &current).unwrap();
        let result = store
            .retire_attempt_and_mark_reseal_cas(&handle, 100, ResealReason::Retryable)
            .unwrap();
        assert_eq!(result.matched_items(), 1);
        assert!(store.due_attempts(&current, 100).unwrap().is_empty());
        let reseal = store.due_reseal_items(&current, 100, &version(9)).unwrap();
        assert_eq!(reseal[0].attempt_count(), 1);
        assert!(store
            .register_resealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)]
            ))
            .is_err());
        store
            .register_resealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "trace", 1, 0)],
            ))
            .unwrap();
        let due = store.due_attempts(&current, 100).unwrap();
        assert_eq!(due.len(), 1);
        assert_ne!(due[0].attempt_id(), &first_id);
        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        assert_eq!(
            reopened.due_attempts(&current, 100).unwrap()[0].attempt_count(),
            1
        );
    }

    #[test]
    fn server_unacked_does_not_increment_and_owner_pause_is_generation_scoped() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let next = owner(1, 2);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "a", "a", 1, 0)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 0)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt(
                3,
                next.clone(),
                vec![item(TraceSpoolEntity::Run, "c", "c", 1, 0)],
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .retire_attempt_and_mark_reseal_cas(&handle, 0, ResealReason::ServerUnacked)
            .unwrap();
        assert_eq!(
            store.due_reseal_items(&current, 0, &version(9)).unwrap()[0].attempt_count(),
            0
        );
        let second = store
            .begin_dispatch(&SealedAttemptId::from_digest([2; 32]), &current)
            .unwrap();
        store
            .pause_owner(&second, OwnerPauseReason::Unauthorized)
            .unwrap();
        assert!(store.due_attempts(&current, 0).unwrap().is_empty());
        assert!(store
            .due_reseal_items(&current, 0, &version(9))
            .unwrap()
            .is_empty());
        assert_eq!(store.due_attempts(&next, 0).unwrap().len(), 1);
    }

    #[test]
    fn update_gate_is_global_by_version_and_new_version_remains_due() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let first = owner(1, 1);
        let other = owner(2, 9);
        let upgraded = owner(3, 1);
        let remediation_owner = owner(4, 2);
        store
            .register_sealed_attempt(attempt_version(
                1,
                first.clone(),
                vec![item(TraceSpoolEntity::Run, "a", "a", 1, 0)],
                7,
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt_version(
                5,
                first.clone(),
                vec![item(TraceSpoolEntity::Run, "a2", "a2", 1, 0)],
                7,
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt_version(
                2,
                other.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 0)],
                7,
            ))
            .unwrap();
        let remediation_key = TraceItemKey::new(TraceSpoolEntity::OutputChunk, "credential");
        store
            .register_sealed_attempt(attempt_version(
                6,
                remediation_owner.clone(),
                vec![item(
                    TraceSpoolEntity::OutputChunk,
                    "credential",
                    "credential-trace",
                    1,
                    0,
                )],
                7,
            ))
            .unwrap();
        let remediation_handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([6; 32]), &remediation_owner)
            .unwrap();
        store
            .mark_needs_remediation(&remediation_handle, std::slice::from_ref(&remediation_key))
            .unwrap();
        assert_eq!(store.due_attempts(&other, 0).unwrap().len(), 1);
        store
            .register_sealed_attempt(attempt_version(
                3,
                upgraded.clone(),
                vec![item(TraceSpoolEntity::Run, "c", "c", 1, 0)],
                8,
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &first)
            .unwrap();
        store.fail_next_manifest_replace_for_test();
        assert!(store.pause_client_version_for_update(&handle).is_err());
        drop(handle);
        drop(store);
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        assert!(store.due_attempts(&other, 0).unwrap().is_empty());
        assert!(matches!(
            store.begin_dispatch(&SealedAttemptId::from_digest([2; 32]), &other),
            Err(TraceSpoolError::AttemptNotClaimable)
        ));
        assert_eq!(
            store
                .due_reseal_items(&other, 0, &version(8))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .due_reseal_items(&first, 0, &version(8))
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            store.register_sealed_attempt(attempt_version(
                8,
                other.clone(),
                vec![item(TraceSpoolEntity::Run, "blocked", "blocked", 1, 0)],
                7
            )),
            Err(TraceSpoolError::ClientVersionBlocked)
        ));
        assert!(matches!(
            store.register_resealed_attempt(attempt_version(
                9,
                other.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 0)],
                7
            )),
            Err(TraceSpoolError::ClientVersionBlocked)
        ));
        let remediation = store
            .due_remediations(&remediation_owner, &version(8))
            .unwrap();
        assert!(matches!(
            store.register_remediated_attempt(
                remediation[0].handle(),
                attempt_version(
                    10,
                    remediation_owner.clone(),
                    vec![item(
                        TraceSpoolEntity::OutputChunk,
                        "credential",
                        "credential-trace",
                        2,
                        0
                    )],
                    7
                ),
                std::slice::from_ref(&remediation_key)
            ),
            Err(TraceSpoolError::ClientVersionBlocked)
        ));
        store
            .register_resealed_attempt(attempt_version(
                4,
                other.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 0)],
                8,
            ))
            .unwrap();
        assert_eq!(store.due_attempts(&other, 0).unwrap().len(), 1);
        assert_eq!(store.due_attempts(&upgraded, 0).unwrap().len(), 1);
    }

    #[test]
    fn forbidden_pauses_all_attempts_for_exact_owner_generation() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(4, 1);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "a", "a", 1, 0)],
            ))
            .unwrap();
        store
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 0)],
            ))
            .unwrap();
        let handle = store
            .begin_dispatch(&SealedAttemptId::from_digest([1; 32]), &current)
            .unwrap();
        store
            .pause_owner(&handle, OwnerPauseReason::Forbidden)
            .unwrap();
        assert!(store.due_attempts(&current, 0).unwrap().is_empty());
    }

    #[test]
    fn startup_sweeps_unrecoverable_attempts_into_a_durable_loss_tombstone() {
        let root = TempDir::new().unwrap();
        let current = owner(9, 1);
        {
            let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
            store
                .register_sealed_attempt(attempt(
                    1,
                    current.clone(),
                    vec![item(TraceSpoolEntity::Run, "run", "restart-loss", 1, 0)],
                ))
                .unwrap();
        }

        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        assert!(reopened
            .due_attempts_without_expiry_for_test(&current)
            .unwrap()
            .is_empty());
        let losses = reopened.loss_paths_for_test(&current);
        assert_eq!(losses.len(), 1);
        let loss: TraceLossDiagnostic =
            serde_json::from_slice(&fs::read(&losses[0]).unwrap()).unwrap();
        assert_eq!(loss.reason, STARTUP_ORPHAN_REASON);
        assert_eq!(loss.trace_id, "restart-loss");
        assert!(matches!(
            reopened.register_sealed_attempt(attempt(
                2,
                current,
                vec![item(TraceSpoolEntity::Run, "revive", "restart-loss", 1, 0)],
            )),
            Err(TraceSpoolError::ExpiredTrace)
        ));
    }

    #[test]
    fn atomic_replace_and_retention_tombstone_order_are_crash_safe() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(1, 1);
        let key = TraceItemKey::new(TraceSpoolEntity::Run, "run");
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "doomed", 1, 0)],
            ))
            .unwrap();
        store.fail_next_manifest_replace_for_test();
        assert!(store
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "run", "doomed", 2, 0)]
            ))
            .is_err());
        assert_eq!(
            TraceSpoolStore::open(root.path().to_path_buf())
                .unwrap()
                .current_revision_for_test(&current, &key),
            Some(1)
        );

        store.fail_next_manifest_replace_for_test();
        assert!(store.expire(&current, TRACE_SPOOL_RETENTION_MS).is_err());
        let reopened = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        assert!(reopened
            .due_attempts_without_expiry_for_test(&current)
            .is_empty());
        assert_eq!(reopened.loss_paths_for_test(&current).len(), 1);
        assert!(matches!(
            reopened.register_sealed_attempt(attempt(
                3,
                current.clone(),
                vec![item(
                    TraceSpoolEntity::Run,
                    "revive",
                    "doomed",
                    1,
                    TRACE_SPOOL_RETENTION_MS
                )]
            )),
            Err(TraceSpoolError::ExpiredTrace)
        ));

        let failed = owner(2, 1);
        reopened
            .register_sealed_attempt(attempt(
                4,
                failed.clone(),
                vec![item(TraceSpoolEntity::Run, "safe", "safe", 1, 0)],
            ))
            .unwrap();
        reopened.fail_next_loss_write_for_test();
        assert!(reopened.expire(&failed, TRACE_SPOOL_RETENTION_MS).is_err());
        assert_eq!(
            TraceSpoolStore::open(root.path().to_path_buf())
                .unwrap()
                .due_attempts_without_expiry_for_test(&failed)
                .len(),
            1
        );
    }

    #[test]
    fn mixed_trace_expiry_releases_young_items_from_removed_attempt() {
        let root = TempDir::new().unwrap();
        let store = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(6, 1);
        store
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![
                    item(TraceSpoolEntity::Run, "old", "expired", 1, 0),
                    item(TraceSpoolEntity::Run, "young", "young", 1, 1),
                ],
            ))
            .unwrap();
        store.expire(&current, TRACE_SPOOL_RETENTION_MS).unwrap();
        assert_eq!(
            store
                .due_reseal_items(&current, TRACE_SPOOL_RETENTION_MS, &version(9))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn same_root_stores_share_lock_and_empty_expiry_does_not_write() {
        let root = TempDir::new().unwrap();
        let first = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let second = TraceSpoolStore::open(root.path().to_path_buf()).unwrap();
        let current = owner(7, 1);
        first
            .register_sealed_attempt(attempt(
                1,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "a", "a", 1, 100)],
            ))
            .unwrap();
        second
            .register_sealed_attempt(attempt(
                2,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "b", "b", 1, 100)],
            ))
            .unwrap();
        assert_eq!(first.due_attempts(&current, 100).unwrap().len(), 2);
        first.fail_next_manifest_replace_for_test();
        assert!(first.expire(&current, 100).unwrap().is_empty());
        assert!(first
            .register_sealed_attempt(attempt(
                3,
                current.clone(),
                vec![item(TraceSpoolEntity::Run, "c", "c", 1, 100)]
            ))
            .is_err());
    }
}
