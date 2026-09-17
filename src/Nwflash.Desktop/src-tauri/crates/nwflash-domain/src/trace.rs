use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use uuid::Uuid;

pub const TRACE_UPLOAD_ENDPOINT_V2: &str = "/api/usage/traces/v2";
pub const TRACE_SCHEMA_VERSION: u8 = 2;
pub const TRACE_UPLOAD_MAX_BODY_BYTES: usize = 1_048_576;
pub const TRACE_UPLOAD_MAX_RUNS: usize = 20;
pub const TRACE_UPLOAD_MAX_EVENTS: usize = 100;
pub const TRACE_UPLOAD_MAX_OUTPUT_CHUNKS: usize = 200;
pub const TRACE_OUTPUT_MAX_BYTES: usize = 32_768;
pub const TRACE_RUN_MAX_EVENTS: usize = 100;
pub const TRACE_RUN_MAX_EVENT_STORAGE_BYTES: usize = 8_388_608;
pub const TRACE_SHORT_TEXT_MAX_BYTES: usize = 1_024;
pub const TRACE_TEXT_MAX_BYTES: usize = 16_384;
pub const TRACE_TEXT_LIST_MAX_ITEMS: usize = 100;
pub const TRACE_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;
pub const TRACE_API_MAX_REJECTED_ITEMS: usize =
    TRACE_UPLOAD_MAX_RUNS + TRACE_UPLOAD_MAX_EVENTS + TRACE_UPLOAD_MAX_OUTPUT_CHUNKS;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TraceContractError {
    #[error("trace upload body is {actual} bytes; maximum is {maximum}")]
    BodyTooLarge { actual: usize, maximum: usize },
    #[error("invalid trace JSON: {0}")]
    Json(String),
    #[error("invalid trace contract: {0}")]
    Invalid(String),
}

pub type TraceContractResult<T> = Result<T, TraceContractError>;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TraceIdGenerationError {
    #[error("system clock cannot produce a UUIDv7 timestamp: {0}")]
    Clock(String),
    #[error("system random source cannot produce a UUIDv7: {0}")]
    Random(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceId(Uuid);

impl TraceId {
    pub fn try_new_v7() -> Result<Self, TraceIdGenerationError> {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| TraceIdGenerationError::Clock(error.to_string()))?;
        let millis = elapsed.as_millis();
        if millis > 0x0000_ffff_ffff_ffff {
            return Err(TraceIdGenerationError::Clock(
                "timestamp exceeds the UUIDv7 48-bit millisecond field".to_string(),
            ));
        }

        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| TraceIdGenerationError::Random(error.to_string()))?;
        let timestamp = (millis as u64).to_be_bytes();
        bytes[..6].copy_from_slice(&timestamp[2..]);
        bytes[6] = (bytes[6] & 0x0f) | 0x70;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        Ok(Self(Uuid::from_bytes(bytes)))
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for TraceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0.hyphenated())
    }
}

impl FromStr for TraceId {
    type Err = TraceContractError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let bytes = value.as_bytes();
        let canonical_shape = bytes.len() == 36
            && bytes.get(8) == Some(&b'-')
            && bytes.get(13) == Some(&b'-')
            && bytes.get(18) == Some(&b'-')
            && bytes.get(23) == Some(&b'-')
            && bytes.get(14) == Some(&b'7')
            && matches!(bytes.get(19), Some(b'8' | b'9' | b'a' | b'b'))
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte) || *byte == b'-');
        if !canonical_shape {
            return Err(invalid("ID must be a canonical lowercase UUIDv7"));
        }
        let uuid = Uuid::parse_str(value)
            .map_err(|_| invalid("ID must be a canonical lowercase UUIDv7"))?;
        if uuid.hyphenated().to_string() != value {
            return Err(invalid("ID must be a canonical lowercase UUIDv7"));
        }
        Ok(Self(uuid))
    }
}

impl Serialize for TraceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TraceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOutcomeV2 {
    Running,
    Success,
    Failed,
    Canceled,
    Denied,
    Aborted,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceEventKindV2 {
    Authorization,
    Stage,
    Partition,
    Command,
    Skip,
    Verification,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceEventStatusV2 {
    Started,
    Success,
    Failed,
    Canceled,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceOutputStreamV2 {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceRejectedEntityV2 {
    Run,
    Event,
    OutputChunk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceRejectedCodeV2 {
    Invalid,
    MissingParent,
    SequenceConflict,
    IncompleteTrace,
    CredentialRejected,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct CredentialRedactionKindV2(String);

impl CredentialRedactionKindV2 {
    pub fn try_new(value: impl Into<String>) -> TraceContractResult<Self> {
        let value = value.into();
        require(
            !value.is_empty() && value.len() <= TRACE_SHORT_TEXT_MAX_BYTES,
            "credential redaction kind must contain 1..=1024 UTF-8 bytes",
        )?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CredentialRedactionKindV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRedactionKindV2")
            .field("utf8_bytes", &self.0.len())
            .finish()
    }
}

impl Serialize for CredentialRedactionKindV2 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for CredentialRedactionKindV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(D::Error::custom)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRedactionCountV2 {
    kind: CredentialRedactionKindV2,
    count: u64,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceCommandV2 {
    program: String,
    argv: Vec<String>,
    display_command: String,
    #[serde(deserialize_with = "required_option")]
    working_directory: Option<String>,
    paths: Vec<String>,
    urls: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    serial: Option<String>,
    #[serde(skip)]
    redaction_counts: Vec<CredentialRedactionCountV2>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceRunV2 {
    run_id: TraceId,
    operation_kind: String,
    title: String,
    outcome: TraceOutcomeV2,
    #[serde(deserialize_with = "required_option")]
    device_serial: Option<String>,
    source_paths: Vec<String>,
    source_urls: Vec<String>,
    client_version: String,
    started_at_ms: u64,
    #[serde(deserialize_with = "required_option")]
    ended_at_ms: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    duration_ms: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    error_class: Option<String>,
    #[serde(deserialize_with = "required_option")]
    error_code: Option<String>,
    #[serde(deserialize_with = "required_option")]
    error_message: Option<String>,
    #[serde(deserialize_with = "required_option")]
    final_sequence: Option<u64>,
    trace_complete: bool,
    #[serde(deserialize_with = "required_option")]
    trace_loss_reason: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceEventV2 {
    event_id: TraceId,
    run_id: TraceId,
    sequence: u64,
    kind: TraceEventKindV2,
    step_name: String,
    #[serde(deserialize_with = "required_option")]
    partition_name: Option<String>,
    status: TraceEventStatusV2,
    started_at_ms: u64,
    #[serde(deserialize_with = "required_option")]
    ended_at_ms: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    duration_ms: Option<u64>,
    #[serde(deserialize_with = "required_option")]
    command: Option<TraceCommandV2>,
    #[serde(deserialize_with = "required_option")]
    exit_code: Option<i32>,
    stdout_chunks: u64,
    stderr_chunks: u64,
    #[serde(deserialize_with = "required_option")]
    verification: Option<String>,
    #[serde(deserialize_with = "required_option")]
    device_state: Option<String>,
    #[serde(deserialize_with = "required_option")]
    retry_safe: Option<bool>,
    remedies: Vec<String>,
    #[serde(deserialize_with = "required_option")]
    error_class: Option<String>,
    #[serde(deserialize_with = "required_option")]
    error_code: Option<String>,
    #[serde(deserialize_with = "required_option")]
    error_message: Option<String>,
    credential_redactions: Vec<CredentialRedactionCountV2>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceOutputChunkV2 {
    chunk_id: TraceId,
    event_id: TraceId,
    stream: TraceOutputStreamV2,
    chunk_index: u64,
    text: String,
    byte_count: u64,
    sha256: String,
}

/// Strictly decoded V2 wire data that has not yet passed semantic validation.
///
/// This raw DTO intentionally does not implement [`Serialize`]. Future spool
/// code must additionally convert every command/output string into a
/// `RedactedTraceText`-style proof type before any disk write, hash, or upload;
/// structural validation alone is not evidence of credential redaction.
///
/// ```compile_fail
/// # use nwflash_domain::TraceUploadRequestV2;
/// fn raw_trace_is_not_a_json_sink(raw: &TraceUploadRequestV2) {
///     let _ = serde_json::to_vec(raw);
/// }
/// ```
///
/// Structural validation is inbound-only and cannot be upgraded into an
/// outbound serializer inside this crate:
///
/// ```compile_fail
/// # use nwflash_domain::TraceUploadRequestV2;
/// # fn attempt(raw: TraceUploadRequestV2) {
/// let validated = raw.into_validated().unwrap();
/// let _body = validated.to_json_vec().unwrap();
/// # }
/// ```
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceUploadRequestV2 {
    schema_version: u8,
    upload_id: TraceId,
    runs: Vec<TraceRunV2>,
    events: Vec<TraceEventV2>,
    output_chunks: Vec<TraceOutputChunkV2>,
}

impl TraceUploadRequestV2 {
    pub fn from_json_slice(bytes: &[u8]) -> TraceContractResult<Self> {
        enforce_body_limit(bytes.len())?;
        let request: Self = serde_json::from_slice(bytes).map_err(sanitized_json_error)?;
        request.validate()?;
        Ok(request)
    }

    pub fn schema_version(&self) -> u8 {
        self.schema_version
    }

    pub fn upload_id(&self) -> TraceId {
        self.upload_id
    }

    pub fn runs(&self) -> &[TraceRunV2] {
        &self.runs
    }

    pub fn events(&self) -> &[TraceEventV2] {
        &self.events
    }

    pub fn output_chunks(&self) -> &[TraceOutputChunkV2] {
        &self.output_chunks
    }

    fn validate(&self) -> TraceContractResult<()> {
        require(
            self.schema_version == TRACE_SCHEMA_VERSION,
            "schema_version must equal 2",
        )?;
        require_items("runs", self.runs.len(), TRACE_UPLOAD_MAX_RUNS)?;
        require_items("events", self.events.len(), TRACE_UPLOAD_MAX_EVENTS)?;
        require_items(
            "output_chunks",
            self.output_chunks.len(),
            TRACE_UPLOAD_MAX_OUTPUT_CHUNKS,
        )?;

        let mut run_ids = HashSet::new();
        for run in &self.runs {
            require(run_ids.insert(run.run_id), "duplicate run_id")?;
            run.validate()?;
        }

        let mut event_ids = HashSet::new();
        let mut event_tuples = HashSet::new();
        let mut run_event_usage: HashMap<TraceId, (usize, usize)> = HashMap::new();
        for event in &self.events {
            require(event_ids.insert(event.event_id), "duplicate event_id")?;
            require(
                event_tuples.insert((event.run_id, event.sequence)),
                "duplicate (run_id, sequence)",
            )?;
            event.validate()?;
            let usage = run_event_usage.entry(event.run_id).or_default();
            usage.0 += 1;
            usage.1 = usage
                .1
                .checked_add(event.metadata_storage_bytes()?)
                .ok_or_else(|| invalid("event metadata byte count overflow"))?;
            require(
                usage.0 <= TRACE_RUN_MAX_EVENTS,
                "logical run exceeds 100 events in this request",
            )?;
            require(
                usage.1 <= TRACE_RUN_MAX_EVENT_STORAGE_BYTES,
                "logical run exceeds 8,388,608 event metadata bytes in this request",
            )?;
        }

        let mut chunk_ids = HashSet::new();
        let mut chunk_tuples = HashSet::new();
        for chunk in &self.output_chunks {
            require(chunk_ids.insert(chunk.chunk_id), "duplicate chunk_id")?;
            require(
                chunk_tuples.insert((chunk.event_id, chunk.stream, chunk.chunk_index)),
                "duplicate (event_id, stream, chunk_index)",
            )?;
            chunk.validate()?;
        }
        Ok(())
    }
}

impl fmt::Debug for TraceUploadRequestV2 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceUploadRequestV2")
            .field("schema_version", &self.schema_version)
            .field("upload_id", &self.upload_id)
            .field("run_count", &self.runs.len())
            .field("event_count", &self.events.len())
            .field("output_chunk_count", &self.output_chunks.len())
            .finish()
    }
}

impl TraceRunV2 {
    pub fn run_id(&self) -> TraceId {
        self.run_id
    }

    pub fn operation_kind(&self) -> &str {
        &self.operation_kind
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn outcome(&self) -> TraceOutcomeV2 {
        self.outcome
    }

    pub fn device_serial(&self) -> Option<&str> {
        self.device_serial.as_deref()
    }

    pub fn source_paths(&self) -> &[String] {
        &self.source_paths
    }

    pub fn source_urls(&self) -> &[String] {
        &self.source_urls
    }

    pub fn client_version(&self) -> &str {
        &self.client_version
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub fn ended_at_ms(&self) -> Option<u64> {
        self.ended_at_ms
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn error_class(&self) -> Option<&str> {
        self.error_class.as_deref()
    }

    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    pub fn final_sequence(&self) -> Option<u64> {
        self.final_sequence
    }

    pub fn trace_complete(&self) -> bool {
        self.trace_complete
    }

    pub fn trace_loss_reason(&self) -> Option<&str> {
        self.trace_loss_reason.as_deref()
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require_short_text("run.operation_kind", &self.operation_kind)?;
        require_text("run.title", &self.title)?;
        require_nullable_short_text("run.device_serial", self.device_serial.as_deref())?;
        require_text_list("run.source_paths", &self.source_paths)?;
        require_text_list("run.source_urls", &self.source_urls)?;
        require_short_text("run.client_version", &self.client_version)?;
        require_safe_integer("run.started_at_ms", self.started_at_ms)?;
        require_nullable_safe_integer("run.ended_at_ms", self.ended_at_ms)?;
        require_nullable_safe_integer("run.duration_ms", self.duration_ms)?;
        require_timestamp_pair(
            "run",
            self.started_at_ms,
            self.ended_at_ms,
            self.duration_ms,
        )?;
        require_nullable_short_text("run.error_class", self.error_class.as_deref())?;
        require_nullable_short_text("run.error_code", self.error_code.as_deref())?;
        require_nullable_text("run.error_message", self.error_message.as_deref())?;
        if let Some(sequence) = self.final_sequence {
            require(
                (1..=TRACE_RUN_MAX_EVENTS as u64).contains(&sequence),
                "run.final_sequence must be from 1 to 100",
            )?;
        }
        require_nullable_text("run.trace_loss_reason", self.trace_loss_reason.as_deref())?;
        if self.trace_complete {
            require(
                self.outcome != TraceOutcomeV2::Running,
                "complete trace outcome cannot be running",
            )?;
            require(
                self.final_sequence.is_some(),
                "complete trace requires final_sequence",
            )?;
            require(
                self.trace_loss_reason.is_none(),
                "complete trace requires a null trace_loss_reason",
            )?;
        }
        Ok(())
    }
}

impl TraceEventV2 {
    pub fn event_id(&self) -> TraceId {
        self.event_id
    }

    pub fn run_id(&self) -> TraceId {
        self.run_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn kind(&self) -> TraceEventKindV2 {
        self.kind
    }

    pub fn step_name(&self) -> &str {
        &self.step_name
    }

    pub fn partition_name(&self) -> Option<&str> {
        self.partition_name.as_deref()
    }

    pub fn status(&self) -> TraceEventStatusV2 {
        self.status
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub fn ended_at_ms(&self) -> Option<u64> {
        self.ended_at_ms
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    pub fn command(&self) -> Option<&TraceCommandV2> {
        self.command.as_ref()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn stdout_chunks(&self) -> u64 {
        self.stdout_chunks
    }

    pub fn stderr_chunks(&self) -> u64 {
        self.stderr_chunks
    }

    pub fn verification(&self) -> Option<&str> {
        self.verification.as_deref()
    }

    pub fn device_state(&self) -> Option<&str> {
        self.device_state.as_deref()
    }

    pub fn retry_safe(&self) -> Option<bool> {
        self.retry_safe
    }

    pub fn remedies(&self) -> &[String] {
        &self.remedies
    }

    pub fn error_class(&self) -> Option<&str> {
        self.error_class.as_deref()
    }

    pub fn error_code(&self) -> Option<&str> {
        self.error_code.as_deref()
    }

    pub fn error_message(&self) -> Option<&str> {
        self.error_message.as_deref()
    }

    pub fn credential_redactions(&self) -> &[CredentialRedactionCountV2] {
        &self.credential_redactions
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require(
            (1..=TRACE_RUN_MAX_EVENTS as u64).contains(&self.sequence),
            "event.sequence must be from 1 to 100",
        )?;
        require_short_text("event.step_name", &self.step_name)?;
        require_nullable_short_text("event.partition_name", self.partition_name.as_deref())?;
        require_safe_integer("event.started_at_ms", self.started_at_ms)?;
        require_nullable_safe_integer("event.ended_at_ms", self.ended_at_ms)?;
        require_nullable_safe_integer("event.duration_ms", self.duration_ms)?;
        require_timestamp_pair(
            "event",
            self.started_at_ms,
            self.ended_at_ms,
            self.duration_ms,
        )?;
        if let Some(command) = &self.command {
            command.validate()?;
        }
        require(
            self.stdout_chunks <= TRACE_UPLOAD_MAX_OUTPUT_CHUNKS as u64,
            "event.stdout_chunks must be at most 200",
        )?;
        require(
            self.stderr_chunks <= TRACE_UPLOAD_MAX_OUTPUT_CHUNKS as u64,
            "event.stderr_chunks must be at most 200",
        )?;
        require_nullable_text("event.verification", self.verification.as_deref())?;
        require_nullable_short_text("event.device_state", self.device_state.as_deref())?;
        require_text_list("event.remedies", &self.remedies)?;
        require_nullable_short_text("event.error_class", self.error_class.as_deref())?;
        require_nullable_short_text("event.error_code", self.error_code.as_deref())?;
        require_nullable_text("event.error_message", self.error_message.as_deref())?;
        require_items(
            "event.credential_redactions",
            self.credential_redactions.len(),
            TRACE_TEXT_LIST_MAX_ITEMS,
        )?;
        for redaction in &self.credential_redactions {
            redaction.validate()?;
        }
        Ok(())
    }

    pub fn metadata_storage_bytes(&self) -> TraceContractResult<usize> {
        let argv = match &self.command {
            Some(command) => json_string(&command.argv)?,
            None => String::new(),
        };
        let paths = json_string(
            self.command
                .as_ref()
                .map(|command| command.paths.as_slice())
                .unwrap_or(&[]),
        )?;
        let urls = json_string(
            self.command
                .as_ref()
                .map(|command| command.urls.as_slice())
                .unwrap_or(&[]),
        )?;
        let remedies = json_string(&self.remedies)?;
        let redactions = json_string(&self.credential_redactions)?;
        let values = [
            self.event_id.to_string(),
            self.run_id.to_string(),
            enum_json(&self.kind)?,
            self.step_name.clone(),
            self.partition_name.clone().unwrap_or_default(),
            enum_json(&self.status)?,
            self.command
                .as_ref()
                .map(|command| command.program.clone())
                .unwrap_or_default(),
            argv,
            self.command
                .as_ref()
                .map(|command| command.display_command.clone())
                .unwrap_or_default(),
            self.command
                .as_ref()
                .and_then(|command| command.working_directory.clone())
                .unwrap_or_default(),
            paths,
            urls,
            self.command
                .as_ref()
                .and_then(|command| command.serial.clone())
                .unwrap_or_default(),
            self.verification.clone().unwrap_or_default(),
            self.device_state.clone().unwrap_or_default(),
            remedies,
            self.error_class.clone().unwrap_or_default(),
            self.error_code.clone().unwrap_or_default(),
            self.error_message.clone().unwrap_or_default(),
            redactions,
        ];
        Ok(values.iter().map(|value| value.len()).sum())
    }
}

impl TraceCommandV2 {
    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn display_command(&self) -> &str {
        &self.display_command
    }

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }

    pub fn paths(&self) -> &[String] {
        &self.paths
    }

    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn credential_redactions(&self) -> &[CredentialRedactionCountV2] {
        &self.redaction_counts
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require_text("command.program", &self.program)?;
        require_text_list("command.argv", &self.argv)?;
        require_text("command.display_command", &self.display_command)?;
        require_nullable_text(
            "command.working_directory",
            self.working_directory.as_deref(),
        )?;
        require_text_list("command.paths", &self.paths)?;
        require_text_list("command.urls", &self.urls)?;
        require_nullable_short_text("command.serial", self.serial.as_deref())?;
        for count in &self.redaction_counts {
            count.validate()?;
        }
        Ok(())
    }
}

impl CredentialRedactionCountV2 {
    pub fn try_new(kind: CredentialRedactionKindV2, count: u64) -> TraceContractResult<Self> {
        let value = Self { kind, count };
        value.validate()?;
        Ok(value)
    }

    pub fn kind(&self) -> &CredentialRedactionKindV2 {
        &self.kind
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require(
            (1..=TRACE_SAFE_INTEGER_MAX).contains(&self.count),
            "redaction.count must be a positive safe integer",
        )
    }
}

impl TraceOutputChunkV2 {
    pub fn chunk_id(&self) -> TraceId {
        self.chunk_id
    }

    pub fn event_id(&self) -> TraceId {
        self.event_id
    }

    pub fn stream(&self) -> TraceOutputStreamV2 {
        self.stream
    }

    pub fn chunk_index(&self) -> u64 {
        self.chunk_index
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn byte_count(&self) -> u64 {
        self.byte_count
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require_safe_integer("chunk.chunk_index", self.chunk_index)?;
        let actual_bytes = self.text.as_bytes();
        require(
            actual_bytes.len() <= TRACE_OUTPUT_MAX_BYTES,
            "chunk.text exceeds the 32 KiB UTF-8 byte limit",
        )?;
        require(
            self.byte_count <= TRACE_OUTPUT_MAX_BYTES as u64,
            "chunk.byte_count exceeds the 32 KiB limit",
        )?;
        require(
            self.byte_count == actual_bytes.len() as u64,
            "chunk.byte_count does not match UTF-8 text",
        )?;
        require(
            self.sha256.len() == 64
                && self
                    .sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "chunk.sha256 must be 64 lowercase hexadecimal characters",
        )?;
        let actual_sha256 = format!("{:x}", Sha256::digest(actual_bytes));
        require(
            self.sha256 == actual_sha256,
            "chunk.sha256 does not match UTF-8 text",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceAcceptedItemsV2 {
    runs: Vec<TraceId>,
    events: Vec<TraceId>,
    output_chunks: Vec<TraceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceRejectedItemV2 {
    entity: TraceRejectedEntityV2,
    #[serde(deserialize_with = "required_option")]
    id: Option<TraceId>,
    code: TraceRejectedCodeV2,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceUploadResponseV2 {
    ok: bool,
    accepted: TraceAcceptedItemsV2,
    rejected: Vec<TraceRejectedItemV2>,
}

pub type TraceUploadAckV2 = TraceUploadResponseV2;

impl TraceUploadResponseV2 {
    pub fn from_json_slice(bytes: &[u8]) -> TraceContractResult<Self> {
        enforce_body_limit(bytes.len())?;
        let response: Self = serde_json::from_slice(bytes).map_err(sanitized_json_error)?;
        response.validate()?;
        Ok(response)
    }

    pub fn accepted(&self) -> &TraceAcceptedItemsV2 {
        &self.accepted
    }

    pub fn rejected(&self) -> &[TraceRejectedItemV2] {
        &self.rejected
    }

    /// Validates the frozen response shape and contradictions within one ACK.
    /// Attempt membership, row revision, owner/login generation, and HTTP
    /// status are deliberately enforced by the uploader's attempt manifest;
    /// this decode-only domain model has no authority to delete spool rows.
    pub fn validate(&self) -> TraceContractResult<()> {
        require(self.ok, "successful trace response requires ok=true")?;
        self.accepted.validate()?;
        require_items(
            "response.rejected",
            self.rejected.len(),
            TRACE_API_MAX_REJECTED_ITEMS,
        )?;
        for rejected in &self.rejected {
            rejected.validate()?;
        }
        let mut rejected_keys = HashSet::new();
        for rejected in &self.rejected {
            require(
                rejected_keys.insert((rejected.entity, rejected.id)),
                "duplicate rejected acknowledgement item",
            )?;
            require(
                rejected.code != TraceRejectedCodeV2::IncompleteTrace,
                "successful acknowledgement cannot contain incomplete_trace",
            )?;
            if let Some(id) = rejected.id {
                let overlaps = match rejected.entity {
                    TraceRejectedEntityV2::Run => self.accepted.runs.contains(&id),
                    TraceRejectedEntityV2::Event => self.accepted.events.contains(&id),
                    TraceRejectedEntityV2::OutputChunk => self.accepted.output_chunks.contains(&id),
                };
                require(
                    !overlaps,
                    "acknowledgement item cannot be accepted and rejected",
                )?;
            }
        }
        Ok(())
    }
}

impl TraceAcceptedItemsV2 {
    pub fn runs(&self) -> &[TraceId] {
        &self.runs
    }

    pub fn events(&self) -> &[TraceId] {
        &self.events
    }

    pub fn output_chunks(&self) -> &[TraceId] {
        &self.output_chunks
    }

    fn validate(&self) -> TraceContractResult<()> {
        require_items("accepted.runs", self.runs.len(), TRACE_UPLOAD_MAX_RUNS)?;
        require_items(
            "accepted.events",
            self.events.len(),
            TRACE_UPLOAD_MAX_EVENTS,
        )?;
        require_items(
            "accepted.output_chunks",
            self.output_chunks.len(),
            TRACE_UPLOAD_MAX_OUTPUT_CHUNKS,
        )?;
        require(
            self.runs.iter().copied().collect::<HashSet<_>>().len() == self.runs.len(),
            "duplicate accepted run id",
        )?;
        require(
            self.events.iter().copied().collect::<HashSet<_>>().len() == self.events.len(),
            "duplicate accepted event id",
        )?;
        require(
            self.output_chunks
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                == self.output_chunks.len(),
            "duplicate accepted output chunk id",
        )
    }
}

impl TraceRejectedItemV2 {
    pub fn entity(&self) -> TraceRejectedEntityV2 {
        self.entity
    }

    pub fn id(&self) -> Option<TraceId> {
        self.id
    }

    pub fn code(&self) -> TraceRejectedCodeV2 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn validate(&self) -> TraceContractResult<()> {
        require_text("rejected.message", &self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TraceApiErrorCodeV2 {
    #[serde(rename = "TRACE_INVALID")]
    Invalid,
    #[serde(rename = "TRACE_UNAUTHORIZED")]
    Unauthorized,
    #[serde(rename = "TRACE_FORBIDDEN")]
    Forbidden,
    #[serde(rename = "TRACE_OWNERSHIP_CONFLICT")]
    OwnershipConflict,
    #[serde(rename = "TRACE_BODY_TOO_LARGE")]
    BodyTooLarge,
    #[serde(rename = "TRACE_INCOMPLETE")]
    Incomplete,
    #[serde(rename = "TRACE_INTERNAL")]
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceApiRequestId(Uuid);

impl fmt::Display for TraceApiRequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0.hyphenated())
    }
}

impl Serialize for TraceApiRequestId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TraceApiRequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let uuid = Uuid::parse_str(&value).map_err(D::Error::custom)?;
        if uuid.hyphenated().to_string() != value {
            return Err(D::Error::custom(
                "request_id must be a canonical lowercase UUID",
            ));
        }
        Ok(Self(uuid))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceApiErrorBodyV2 {
    code: TraceApiErrorCodeV2,
    message: String,
    request_id: TraceApiRequestId,
    #[serde(
        default,
        deserialize_with = "present_vec",
        skip_serializing_if = "Option::is_none"
    )]
    details: Option<Vec<TraceRejectedItemV2>>,
}

impl TraceApiErrorBodyV2 {
    pub fn code(&self) -> TraceApiErrorCodeV2 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn request_id(&self) -> TraceApiRequestId {
        self.request_id
    }

    pub fn details(&self) -> Option<&[TraceRejectedItemV2]> {
        self.details.as_deref()
    }

    fn validate(&self) -> TraceContractResult<()> {
        require_text("error.message", &self.message)?;
        if let Some(details) = &self.details {
            require_items("error.details", details.len(), TRACE_API_MAX_REJECTED_ITEMS)?;
            for detail in details {
                detail.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceApiErrorV2 {
    ok: bool,
    error: TraceApiErrorBodyV2,
}

impl TraceApiErrorV2 {
    pub fn from_json_slice(bytes: &[u8]) -> TraceContractResult<Self> {
        enforce_body_limit(bytes.len())?;
        let response: Self = serde_json::from_slice(bytes).map_err(sanitized_json_error)?;
        response.validate()?;
        Ok(response)
    }

    pub fn error(&self) -> &TraceApiErrorBodyV2 {
        &self.error
    }

    pub fn validate(&self) -> TraceContractResult<()> {
        require(!self.ok, "trace API error requires ok=false")?;
        self.error.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceApiResponseV2 {
    Success(TraceUploadResponseV2),
    Error(TraceApiErrorV2),
}

impl TraceApiResponseV2 {
    pub fn from_json_slice(bytes: &[u8]) -> TraceContractResult<Self> {
        enforce_body_limit(bytes.len())?;
        let wire: TraceApiResponseWire =
            serde_json::from_slice(bytes).map_err(sanitized_json_error)?;
        match wire {
            TraceApiResponseWire::Success(response) => {
                response.validate()?;
                Ok(Self::Success(response))
            }
            TraceApiResponseWire::Error(response) => {
                response.validate()?;
                Ok(Self::Error(response))
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TraceApiResponseWire {
    Success(TraceUploadResponseV2),
    Error(TraceApiErrorV2),
}

fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn present_vec<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Vec::<T>::deserialize(deserializer).map(Some)
}

fn enforce_body_limit(actual: usize) -> TraceContractResult<()> {
    if actual > TRACE_UPLOAD_MAX_BODY_BYTES {
        return Err(TraceContractError::BodyTooLarge {
            actual,
            maximum: TRACE_UPLOAD_MAX_BODY_BYTES,
        });
    }
    Ok(())
}

fn sanitized_json_error(error: serde_json::Error) -> TraceContractError {
    TraceContractError::Json(format!(
        "malformed trace JSON at line {} column {}",
        error.line(),
        error.column()
    ))
}

fn require(condition: bool, message: impl Into<String>) -> TraceContractResult<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn invalid(message: impl Into<String>) -> TraceContractError {
    TraceContractError::Invalid(message.into())
}

fn require_items(field: &str, actual: usize, maximum: usize) -> TraceContractResult<()> {
    require(
        actual <= maximum,
        format!("{field} must contain at most {maximum} items"),
    )
}

fn require_bytes(field: &str, value: &str, maximum: usize) -> TraceContractResult<()> {
    require(
        value.len() <= maximum,
        format!("{field} exceeds UTF-8 byte limit of {maximum}"),
    )
}

fn require_short_text(field: &str, value: &str) -> TraceContractResult<()> {
    require_bytes(field, value, TRACE_SHORT_TEXT_MAX_BYTES)
}

fn require_nullable_short_text(field: &str, value: Option<&str>) -> TraceContractResult<()> {
    match value {
        Some(value) => require_short_text(field, value),
        None => Ok(()),
    }
}

fn require_text(field: &str, value: &str) -> TraceContractResult<()> {
    require_bytes(field, value, TRACE_TEXT_MAX_BYTES)
}

fn require_nullable_text(field: &str, value: Option<&str>) -> TraceContractResult<()> {
    match value {
        Some(value) => require_text(field, value),
        None => Ok(()),
    }
}

fn require_text_list(field: &str, values: &[String]) -> TraceContractResult<()> {
    require_items(field, values.len(), TRACE_TEXT_LIST_MAX_ITEMS)?;
    for value in values {
        require_text(field, value)?;
    }
    Ok(())
}

fn require_safe_integer(field: &str, value: u64) -> TraceContractResult<()> {
    require(
        value <= TRACE_SAFE_INTEGER_MAX,
        format!("{field} exceeds the JSON safe-integer maximum"),
    )
}

fn require_nullable_safe_integer(field: &str, value: Option<u64>) -> TraceContractResult<()> {
    match value {
        Some(value) => require_safe_integer(field, value),
        None => Ok(()),
    }
}

fn require_timestamp_pair(
    field: &str,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    duration_ms: Option<u64>,
) -> TraceContractResult<()> {
    require(
        ended_at_ms.is_some() == duration_ms.is_some(),
        format!("{field}.ended_at_ms and duration_ms must both be null or present"),
    )?;
    if let (Some(ended), Some(duration)) = (ended_at_ms, duration_ms) {
        require(
            ended >= started_at_ms && ended - started_at_ms == duration,
            format!("{field}.duration_ms must equal ended_at_ms - started_at_ms"),
        )?;
    }
    Ok(())
}

fn json_string<T>(value: &T) -> TraceContractResult<String>
where
    T: Serialize + ?Sized,
{
    serde_json::to_string(value).map_err(|error| TraceContractError::Json(error.to_string()))
}

fn enum_json<T>(value: &T) -> TraceContractResult<String>
where
    T: Serialize,
{
    let encoded = json_string(value)?;
    Ok(encoded.trim_matches('"').to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const SUCCESS: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.success.json");
    const FAILED: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.failed.json");
    const OPEN: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.open.json");
    const EVENT_ONLY: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.event-only.json");
    const CHUNK_ONLY: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.chunk-only.json");
    const FINALIZE_ONLY: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload.finalize-only.json");
    const ACK: &str =
        include_str!("../../../../../../cloudflare/contracts/trace-v2/upload-ack.success.json");

    fn success_value() -> Value {
        serde_json::from_str(SUCCESS).expect("fixture JSON")
    }

    fn encode(value: &Value) -> Vec<u8> {
        serde_json::to_vec(value).expect("test JSON")
    }

    fn decode_validated(bytes: &[u8]) -> TraceContractResult<TraceUploadRequestV2> {
        TraceUploadRequestV2::from_json_slice(bytes)
    }

    #[test]
    fn canonical_upload_fixtures_parse_and_validate_strictly() {
        for fixture in [SUCCESS, FAILED, OPEN, EVENT_ONLY, CHUNK_ONLY, FINALIZE_ONLY] {
            let request = TraceUploadRequestV2::from_json_slice(fixture.as_bytes())
                .expect("canonical upload fixture");
            let expected: Value = serde_json::from_str(fixture).expect("fixture JSON");
            assert_eq!(request.schema_version(), TRACE_SCHEMA_VERSION);
            assert_eq!(
                request.runs().len(),
                expected["runs"].as_array().unwrap().len()
            );
            assert_eq!(
                request.events().len(),
                expected["events"].as_array().unwrap().len()
            );
            assert_eq!(
                request.output_chunks().len(),
                expected["output_chunks"].as_array().unwrap().len()
            );
        }
    }

    #[test]
    fn canonical_ack_fixture_parses_validates_and_round_trips_exactly() {
        let ack = TraceUploadResponseV2::from_json_slice(ACK.as_bytes())
            .expect("canonical acknowledgement fixture");
        let expected: Value = serde_json::from_str(ACK).expect("fixture JSON");
        let actual = serde_json::to_value(ack).expect("serialize acknowledgement");
        assert_eq!(actual, expected);
    }

    #[test]
    fn strict_wire_shape_rejects_unknown_and_missing_fields() {
        let mut unknown = success_value();
        unknown["runs"][0]["environment"] = json!({"SECRET": "must never be captured"});
        assert!(TraceUploadRequestV2::from_json_slice(&encode(&unknown)).is_err());

        let mut command_unknown = success_value();
        command_unknown["events"][1]["command"]["headers"] =
            json!({"Authorization": "Bearer secret"});
        assert!(TraceUploadRequestV2::from_json_slice(&encode(&command_unknown)).is_err());

        let mut missing_nullable = success_value();
        missing_nullable["events"][0]
            .as_object_mut()
            .expect("event object")
            .remove("verification");
        assert!(TraceUploadRequestV2::from_json_slice(&encode(&missing_nullable)).is_err());
    }

    #[test]
    fn closed_enums_reject_aliases_and_unknown_values() {
        let cases = [
            (vec!["runs", "0", "outcome"], "completed"),
            (vec!["events", "0", "kind"], "log"),
            (vec!["events", "0", "status"], "complete"),
            (vec!["output_chunks", "0", "stream"], "console"),
        ];

        for (path, replacement) in cases {
            let mut value = success_value();
            let mut cursor = &mut value;
            for segment in path {
                cursor = if let Ok(index) = segment.parse::<usize>() {
                    &mut cursor[index]
                } else {
                    &mut cursor[segment]
                };
            }
            *cursor = json!(replacement);
            assert!(TraceUploadRequestV2::from_json_slice(&encode(&value)).is_err());
        }
    }

    #[test]
    fn redaction_kind_is_open_but_utf8_bounded() {
        let mut value = success_value();
        value["events"][0]["credential_redactions"] = json!([{
            "kind": "future-v3-classifier",
            "count": 1
        }]);
        let request = TraceUploadRequestV2::from_json_slice(&encode(&value))
            .expect("future redaction kinds remain forward compatible");
        assert_eq!(
            request.events()[0].credential_redactions()[0]
                .kind()
                .as_str(),
            "future-v3-classifier"
        );

        value["events"][0]["credential_redactions"][0]["kind"] =
            json!("x".repeat(TRACE_SHORT_TEXT_MAX_BYTES + 1));
        assert!(TraceUploadRequestV2::from_json_slice(&encode(&value)).is_err());
    }

    #[test]
    fn every_wire_id_must_be_canonical_lowercase_uuid_v7() {
        for (path, replacement) in [
            (vec!["upload_id"], "019D9C40-7B3C-7000-8000-000000000001"),
            (
                vec!["runs", "0", "run_id"],
                "019d9c40-7b3c-4000-8000-000000000002",
            ),
            (vec!["events", "0", "event_id"], "not-a-uuid"),
            (
                vec!["output_chunks", "0", "chunk_id"],
                "019d9c40-7b3c-7000-7000-000000000006",
            ),
        ] {
            let mut value = success_value();
            let mut cursor = &mut value;
            for segment in path {
                cursor = if let Ok(index) = segment.parse::<usize>() {
                    &mut cursor[index]
                } else {
                    &mut cursor[segment]
                };
            }
            *cursor = json!(replacement);
            assert!(TraceUploadRequestV2::from_json_slice(&encode(&value)).is_err());
        }
    }

    #[test]
    fn request_limits_are_utf8_byte_and_item_limits() {
        let mut too_many_runs = success_value();
        let run = too_many_runs["runs"][0].clone();
        too_many_runs["runs"] = Value::Array(vec![run; TRACE_UPLOAD_MAX_RUNS + 1]);
        let error = decode_validated(&encode(&too_many_runs))
            .expect_err("run count above the request limit");
        assert!(error.to_string().contains("runs"));

        let mut too_many_events = success_value();
        let event = too_many_events["events"][0].clone();
        too_many_events["events"] = Value::Array(vec![event; TRACE_UPLOAD_MAX_EVENTS + 1]);
        let error = decode_validated(&encode(&too_many_events))
            .expect_err("event count above the request limit");
        assert!(error.to_string().contains("events"));

        let mut too_many_chunks = success_value();
        let chunk = too_many_chunks["output_chunks"][0].clone();
        too_many_chunks["output_chunks"] =
            Value::Array(vec![chunk; TRACE_UPLOAD_MAX_OUTPUT_CHUNKS + 1]);
        let error = decode_validated(&encode(&too_many_chunks))
            .expect_err("chunk count above the request limit");
        assert!(error.to_string().contains("output_chunks"));

        let mut oversized_chunk = success_value();
        oversized_chunk["output_chunks"][0]["text"] = json!("界".repeat(10_923));
        oversized_chunk["output_chunks"][0]["byte_count"] = json!(32_769);
        let error =
            decode_validated(&encode(&oversized_chunk)).expect_err("32 KiB is a UTF-8 byte limit");
        assert!(error.to_string().contains("32 KiB"));

        let oversized_body = vec![b' '; TRACE_UPLOAD_MAX_BODY_BYTES + 1];
        assert_eq!(
            TraceUploadRequestV2::from_json_slice(&oversized_body),
            Err(TraceContractError::BodyTooLarge {
                actual: TRACE_UPLOAD_MAX_BODY_BYTES + 1,
                maximum: TRACE_UPLOAD_MAX_BODY_BYTES,
            })
        );
    }

    #[test]
    fn scalar_list_and_logical_run_metadata_limits_are_enforced() {
        let mut oversized_short_text = success_value();
        oversized_short_text["runs"][0]["operation_kind"] = json!("界".repeat(342));
        assert!(decode_validated(&encode(&oversized_short_text)).is_err());

        let mut oversized_text = success_value();
        oversized_text["runs"][0]["title"] = json!("界".repeat(5_462));
        assert!(decode_validated(&encode(&oversized_text)).is_err());

        let mut oversized_list = success_value();
        oversized_list["runs"][0]["source_paths"] = json!(vec!["x"; 101]);
        assert!(decode_validated(&encode(&oversized_list)).is_err());

        let mut unsafe_integer = success_value();
        unsafe_integer["runs"][0]["started_at_ms"] = json!(TRACE_SAFE_INTEGER_MAX + 1);
        assert!(decode_validated(&encode(&unsafe_integer)).is_err());

        let mut request = TraceUploadRequestV2::from_json_slice(SUCCESS.as_bytes())
            .expect("canonical upload fixture");
        let mut event = request.events[0].clone();
        event.command = Some(TraceCommandV2 {
            program: String::new(),
            argv: vec!["x".repeat(TRACE_TEXT_MAX_BYTES); 6],
            display_command: String::new(),
            working_directory: None,
            paths: Vec::new(),
            urls: Vec::new(),
            serial: None,
            redaction_counts: Vec::new(),
        });
        request.events = (1..=TRACE_RUN_MAX_EVENTS)
            .map(|sequence| {
                let mut event = event.clone();
                event.event_id = TraceId::try_new_v7().expect("system UUIDv7 generation");
                event.sequence = sequence as u64;
                event
            })
            .collect();
        let error = request
            .validate()
            .expect_err("logical run metadata above 8 MiB");
        assert!(error.to_string().contains("8,388,608"));
    }

    #[test]
    fn duplicate_item_ids_and_natural_tuples_are_rejected() {
        let mut duplicate_run = success_value();
        duplicate_run["runs"] = json!([
            duplicate_run["runs"][0].clone(),
            duplicate_run["runs"][0].clone()
        ]);
        assert!(decode_validated(&encode(&duplicate_run))
            .expect_err("duplicate run id")
            .to_string()
            .contains("duplicate run_id"));

        let mut duplicate_sequence = success_value();
        let mut event = duplicate_sequence["events"][1].clone();
        event["event_id"] = json!("019d9c40-7b3c-7000-8000-000000000099");
        event["sequence"] = duplicate_sequence["events"][0]["sequence"].clone();
        duplicate_sequence["events"] = json!([duplicate_sequence["events"][0].clone(), event]);
        assert!(decode_validated(&encode(&duplicate_sequence))
            .expect_err("duplicate event natural tuple")
            .to_string()
            .contains("run_id, sequence"));

        let mut duplicate_chunk_tuple = success_value();
        let mut chunk = duplicate_chunk_tuple["output_chunks"][0].clone();
        chunk["chunk_id"] = json!("019d9c40-7b3c-7000-8000-000000000099");
        duplicate_chunk_tuple["output_chunks"] =
            json!([duplicate_chunk_tuple["output_chunks"][0].clone(), chunk]);
        assert!(decode_validated(&encode(&duplicate_chunk_tuple))
            .expect_err("duplicate output natural tuple")
            .to_string()
            .contains("event_id, stream, chunk_index"));
    }

    #[test]
    fn chunks_require_exact_utf8_byte_count_and_lowercase_sha256() {
        for field in ["byte_count", "sha256"] {
            let mut value = success_value();
            value["output_chunks"][0][field] = if field == "byte_count" {
                json!(19)
            } else {
                json!("0".repeat(64))
            };
            assert!(decode_validated(&encode(&value))
                .expect_err("chunk integrity mismatch")
                .to_string()
                .contains(field));
        }

        let mut uppercase = success_value();
        uppercase["output_chunks"][0]["sha256"] =
            json!("71977B278C69313B52F81C96E75D91CE5DF771FFC3D24E4D24EF8C0C8CAE4B6F");
        assert!(decode_validated(&encode(&uppercase)).is_err());
    }

    #[test]
    fn basic_completion_and_timestamp_invariants_are_enforced() {
        let cases = [
            ("outcome", json!("running")),
            ("final_sequence", Value::Null),
            ("trace_loss_reason", json!("lost")),
        ];
        for (field, replacement) in cases {
            let mut value = success_value();
            value["runs"][0][field] = replacement;
            assert!(decode_validated(&encode(&value)).is_err());
        }

        let mut mismatched_duration = success_value();
        mismatched_duration["events"][0]["duration_ms"] = json!(99);
        assert!(decode_validated(&encode(&mismatched_duration)).is_err());
    }

    #[test]
    fn raw_debug_never_exposes_trace_content() {
        let request = TraceUploadRequestV2::from_json_slice(SUCCESS.as_bytes())
            .expect("canonical upload fixture");
        let raw_debug = format!("{request:?}");
        for sentinel in [
            "fastboot.exe",
            "boot.img",
            "Sending boot_a",
            "9A7F23BC10D4",
            "Bearer authentication accepted",
        ] {
            assert!(!raw_debug.contains(sentinel), "raw Debug leaked {sentinel}");
        }
    }

    #[test]
    fn api_response_distinguishes_success_from_frozen_error_envelopes() {
        let success = TraceApiResponseV2::from_json_slice(ACK.as_bytes())
            .expect("canonical acknowledgement fixture");
        assert!(matches!(success, TraceApiResponseV2::Success(_)));

        for code in [
            "TRACE_INVALID",
            "TRACE_UNAUTHORIZED",
            "TRACE_FORBIDDEN",
            "TRACE_OWNERSHIP_CONFLICT",
            "TRACE_BODY_TOO_LARGE",
            "TRACE_INCOMPLETE",
            "TRACE_INTERNAL",
        ] {
            let response = json!({
                "ok": false,
                "error": {
                    "code": code,
                    "message": "safe message",
                    "request_id": "f7eaa6e0-6a5f-4a4d-8c46-5ebebd192f24"
                }
            });
            let parsed = TraceApiResponseV2::from_json_slice(&encode(&response))
                .expect("frozen API error code");
            assert!(matches!(parsed, TraceApiResponseV2::Error(_)));
        }

        let invalid_code = json!({
            "ok": false,
            "error": {
                "code": "UPDATE_REQUIRED",
                "message": "legacy 426 is not V2",
                "request_id": "f7eaa6e0-6a5f-4a4d-8c46-5ebebd192f24"
            }
        });
        assert!(TraceApiResponseV2::from_json_slice(&encode(&invalid_code)).is_err());
    }

    #[test]
    fn api_response_enforces_ack_error_and_body_bounds() {
        let mut too_many_accepted = serde_json::from_str::<Value>(ACK).expect("ack fixture JSON");
        too_many_accepted["accepted"]["runs"] =
            json!(vec!["019d9c40-7b3c-7000-8000-000000000002"; 21]);
        assert!(TraceApiResponseV2::from_json_slice(&encode(&too_many_accepted)).is_err());

        let detail = json!({
            "entity": "run",
            "id": "019d9c40-7b3c-7000-8000-000000000002",
            "code": "incomplete_trace",
            "message": "incomplete"
        });
        let too_many_details = json!({
            "ok": false,
            "error": {
                "code": "TRACE_INCOMPLETE",
                "message": "safe message",
                "request_id": "f7eaa6e0-6a5f-4a4d-8c46-5ebebd192f24",
                "details": vec![detail; TRACE_API_MAX_REJECTED_ITEMS + 1]
            }
        });
        assert!(TraceApiResponseV2::from_json_slice(&encode(&too_many_details)).is_err());

        let null_details = json!({
            "ok": false,
            "error": {
                "code": "TRACE_INTERNAL",
                "message": "safe message",
                "request_id": "f7eaa6e0-6a5f-4a4d-8c46-5ebebd192f24",
                "details": null
            }
        });
        assert!(TraceApiResponseV2::from_json_slice(&encode(&null_details)).is_err());

        let oversized_message = json!({
            "ok": false,
            "error": {
                "code": "TRACE_INTERNAL",
                "message": "x".repeat(TRACE_TEXT_MAX_BYTES + 1),
                "request_id": "f7eaa6e0-6a5f-4a4d-8c46-5ebebd192f24"
            }
        });
        assert!(TraceApiResponseV2::from_json_slice(&encode(&oversized_message)).is_err());

        let malformed_request_id = json!({
            "ok": false,
            "error": {
                "code": "TRACE_INTERNAL",
                "message": "safe message",
                "request_id": "F7EAA6E0-6A5F-4A4D-8C46-5EBEBD192F24"
            }
        });
        assert!(TraceApiResponseV2::from_json_slice(&encode(&malformed_request_id)).is_err());

        let oversized_body = vec![b' '; TRACE_UPLOAD_MAX_BODY_BYTES + 1];
        assert!(matches!(
            TraceApiResponseV2::from_json_slice(&oversized_body),
            Err(TraceContractError::BodyTooLarge { .. })
        ));
    }

    #[test]
    fn ack_rejects_duplicates_overlap_and_incomplete_success() {
        let accepted_id = "019d9c40-7b3c-7000-8000-000000000003";
        let mut duplicate: Value = serde_json::from_str(ACK).expect("ack JSON");
        duplicate["accepted"]["events"] = json!([accepted_id, accepted_id]);
        assert!(TraceUploadResponseV2::from_json_slice(&encode(&duplicate)).is_err());

        let mut overlap: Value = serde_json::from_str(ACK).expect("ack JSON");
        overlap["rejected"] = json!([{
            "entity": "event",
            "id": accepted_id,
            "code": "invalid",
            "message": "safe"
        }]);
        assert!(TraceUploadResponseV2::from_json_slice(&encode(&overlap)).is_err());

        let mut duplicate_rejected: Value = serde_json::from_str(ACK).expect("ack JSON");
        let rejected = json!({
            "entity": "event",
            "id": "019d9c40-7b3c-7000-8000-000000000099",
            "code": "invalid",
            "message": "safe"
        });
        duplicate_rejected["rejected"] = json!([rejected.clone(), rejected]);
        assert!(TraceUploadResponseV2::from_json_slice(&encode(&duplicate_rejected)).is_err());

        let mut incomplete: Value = serde_json::from_str(ACK).expect("ack JSON");
        incomplete["rejected"] = json!([{
            "entity": "run",
            "id": "019d9c40-7b3c-7000-8000-000000000099",
            "code": "incomplete_trace",
            "message": "safe"
        }]);
        assert!(TraceUploadResponseV2::from_json_slice(&encode(&incomplete)).is_err());
    }

    #[test]
    fn serde_errors_never_echo_attacker_controlled_values() {
        let secret = "unknown-enum-secret-991122";
        let mut value = success_value();
        value["runs"][0]["outcome"] = json!(secret);
        let error =
            TraceUploadRequestV2::from_json_slice(&encode(&value)).expect_err("unknown outcome");
        assert!(!format!("{error}").contains(secret));
        assert!(!format!("{error:?}").contains(secret));
    }
}
