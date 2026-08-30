use std::{cmp::Reverse, fmt, io, io::Write};

use nwflash_domain::{
    TraceEventKindV2, TraceEventStatusV2, TraceId, TraceOutcomeV2, TraceOutputStreamV2,
    TraceUploadRequestV2, TRACE_OUTPUT_MAX_BYTES, TRACE_SCHEMA_VERSION, TRACE_SHORT_TEXT_MAX_BYTES,
    TRACE_TEXT_LIST_MAX_ITEMS, TRACE_TEXT_MAX_BYTES, TRACE_UPLOAD_MAX_BODY_BYTES,
    TRACE_UPLOAD_MAX_EVENTS, TRACE_UPLOAD_MAX_OUTPUT_CHUNKS, TRACE_UPLOAD_MAX_RUNS,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

use crate::{begin_trace_credential_sentinel, end_marker};

pub const MAX_TRACE_REDACTION_CARRY_BYTES: usize = 64 * 1024;
pub const MAX_TRACE_REDACTED_OUTPUT_BYTES: usize =
    TRACE_OUTPUT_MAX_BYTES * TRACE_UPLOAD_MAX_OUTPUT_CHUNKS;
pub const MAX_TRACE_EXACT_SECRETS: usize = 32;
pub const MAX_TRACE_EXACT_SECRET_BYTES: usize = 4 * 1024;

const MIN_TRACE_EXACT_SECRET_BYTES: usize = 6;

const REDACTED: &str = "[REDACTED]";
const PRIVATE_KEY_REMOVED: &str = "[CREDENTIAL_REMOVED:PRIVATE_KEY]";
const HIGH_RISK_REMOVED: &str = "[CREDENTIAL_REMOVED:HIGH_RISK]";
const KIND_COUNT: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum CredentialKind {
    Authorization,
    Bearer,
    Cookie,
    Password,
    Token,
    ApiKey,
    Secret,
    Signature,
    UrlUserinfo,
    PrivateKey,
    Exact,
    HighRisk,
}

impl CredentialKind {
    const ALL: [Self; KIND_COUNT] = [
        Self::Authorization,
        Self::Bearer,
        Self::Cookie,
        Self::Password,
        Self::Token,
        Self::ApiKey,
        Self::Secret,
        Self::Signature,
        Self::UrlUserinfo,
        Self::PrivateKey,
        Self::Exact,
        Self::HighRisk,
    ];

    const fn index(self) -> usize {
        self as usize
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authorization => "authorization",
            Self::Bearer => "bearer",
            Self::Cookie => "cookie",
            Self::Password => "password",
            Self::Token => "token",
            Self::ApiKey => "api-key",
            Self::Secret => "secret",
            Self::Signature => "signature",
            Self::UrlUserinfo => "url-userinfo",
            Self::PrivateKey => "private-key",
            Self::Exact => "exact",
            Self::HighRisk => "high-risk",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialRedactionCount {
    pub kind: CredentialKind,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionSummary {
    counts: [u64; KIND_COUNT],
}

impl RedactionSummary {
    fn empty() -> Self {
        Self {
            counts: [0; KIND_COUNT],
        }
    }

    fn add(&mut self, kind: CredentialKind) {
        let count = &mut self.counts[kind.index()];
        *count = count.saturating_add(1);
    }

    fn merge(&mut self, other: &Self) {
        for kind in CredentialKind::ALL {
            self.counts[kind.index()] =
                self.counts[kind.index()].saturating_add(other.counts[kind.index()]);
        }
    }

    pub const fn count(&self, kind: CredentialKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().copied().fold(0_u64, u64::saturating_add)
    }

    pub fn counts(&self) -> impl Iterator<Item = CredentialRedactionCount> + '_ {
        CredentialKind::ALL.into_iter().filter_map(|kind| {
            let count = self.count(kind);
            (count > 0).then_some(CredentialRedactionCount { kind, count })
        })
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RedactedTraceText {
    text: Zeroizing<String>,
    summary: RedactionSummary,
}

impl RedactedTraceText {
    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }

    pub const fn summary(&self) -> &RedactionSummary {
        &self.summary
    }
}

impl fmt::Debug for RedactedTraceText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedTraceText")
            .field("utf8_bytes", &self.text.len())
            .field("summary", &self.summary)
            .finish()
    }
}

impl Zeroize for RedactedTraceText {
    fn zeroize(&mut self) {
        self.text.zeroize();
    }
}

/// One complete stdout/stderr logical stream after a single scanner session
/// has observed every input byte. It is the only source of output chunks.
pub struct RedactedLogicalStream {
    text: RedactedTraceText,
}

impl RedactedLogicalStream {
    pub fn as_str(&self) -> &str {
        self.text.as_str()
    }

    pub const fn summary(&self) -> &RedactionSummary {
        self.text.summary()
    }

    fn into_output_chunks(
        self,
        event_id: TraceId,
        stream: TraceOutputStreamV2,
        first_chunk_index: u64,
    ) -> Result<Vec<RedactedOutputChunk>, TraceRedactionError> {
        let ranges = utf8_chunk_ranges(self.text.as_str(), TRACE_OUTPUT_MAX_BYTES);
        if ranges.len() > TRACE_UPLOAD_MAX_OUTPUT_CHUNKS {
            return Err(TraceRedactionError::TooManyOutputChunks);
        }
        let mut chunks = Vec::with_capacity(ranges.len());
        for (offset, (start, end)) in ranges.into_iter().enumerate() {
            let chunk_id = TraceId::try_new_v7().map_err(|_| TraceRedactionError::IdGeneration)?;
            let text = Zeroizing::new(self.text.as_str()[start..end].to_owned());
            let bytes = text.as_bytes();
            chunks.push(RedactedOutputChunk {
                chunk_id,
                event_id,
                stream,
                chunk_index: first_chunk_index
                    .checked_add(offset as u64)
                    .ok_or(TraceRedactionError::InvalidChunkIndex)?,
                byte_count: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(bytes)),
                text,
            });
        }
        Ok(chunks)
    }
}

impl fmt::Debug for RedactedLogicalStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedLogicalStream")
            .field("utf8_bytes", &self.text.as_str().len())
            .field("summary", self.text.summary())
            .finish()
    }
}

/// An output chunk can only be produced by consuming one complete
/// [`RedactedLogicalStream`]. Per-fragment proof construction is impossible.
///
/// ```compile_fail
/// # use nwflash_domain::{TraceId, TraceOutputStreamV2};
/// # use nwflash_protection::RedactedOutputChunk;
/// let chunk = RedactedOutputChunk {
///     chunk_id: TraceId::try_new_v7().unwrap(),
///     event_id: TraceId::try_new_v7().unwrap(),
///     stream: TraceOutputStreamV2::Stdout,
///     chunk_index: 0,
///     text: "raw-token".into(),
///     byte_count: 9,
///     sha256: String::new(),
/// };
/// ```
pub struct RedactedOutputChunk {
    chunk_id: TraceId,
    event_id: TraceId,
    stream: TraceOutputStreamV2,
    chunk_index: u64,
    text: Zeroizing<String>,
    byte_count: u64,
    sha256: String,
}

impl RedactedOutputChunk {
    pub const fn chunk_id(&self) -> TraceId {
        self.chunk_id
    }

    pub const fn event_id(&self) -> TraceId {
        self.event_id
    }

    pub const fn stream(&self) -> TraceOutputStreamV2 {
        self.stream
    }

    pub const fn chunk_index(&self) -> u64 {
        self.chunk_index
    }

    pub fn text(&self) -> &str {
        self.text.as_str()
    }

    pub const fn byte_count(&self) -> u64 {
        self.byte_count
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

impl fmt::Debug for RedactedOutputChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedOutputChunk")
            .field("chunk_id", &self.chunk_id)
            .field("event_id", &self.event_id)
            .field("stream", &self.stream)
            .field("chunk_index", &self.chunk_index)
            .field("byte_count", &self.byte_count)
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// Owns one process output reader until EOF, performs the only complete-stream
/// credential scan, and is the only public path that can create output chunks.
///
/// The session deliberately consumes a reader rather than accepting arbitrary
/// text fragments.  A caller therefore cannot independently seal the two halves
/// of a credential and later join them into a trace upload.
pub struct TraceOutputSession {
    output_chunks: Vec<RedactedOutputChunk>,
    redaction_summary: RedactionSummary,
}

impl TraceOutputSession {
    /// ```compile_fail
    /// use nwflash_protection::TraceCredentialScanner;
    ///
    /// let mut scanner = TraceCredentialScanner::new();
    /// scanner.push(b"first-half-of-a-secret");
    /// let _ = scanner.finish(); // private: only TraceOutputSession may finish
    /// ```
    ///
    /// Callers must use [`Self::from_reader`], which keeps the scanner private
    /// until the input reader has reached EOF.
    /// Reads `reader` to EOF before any chunk is created. Stable event and
    /// chunk IDs are retained across the bounded upload attempts returned by
    /// [`Self::into_upload_attempts`]. If the redacted output budget saturates,
    /// this returns immediately with [`TraceRedactionError::OutputTooLarge`]
    /// without reading another source buffer. The process bridge remains
    /// responsible for terminating, draining, and reaping the child process.
    pub fn from_reader<R: io::Read>(
        event_id: TraceId,
        stream: TraceOutputStreamV2,
        reader: &mut R,
        secrets: &ExactSecretSet,
    ) -> Result<Self, TraceRedactionError> {
        let mut scanner = TraceCredentialScanner::with_exact_secrets(secrets);
        let mut buffer = Zeroizing::new([0_u8; 8 * 1024]);
        loop {
            let read = reader
                .read(&mut *buffer)
                .map_err(|_| TraceRedactionError::SourceRead)?;
            if read == 0 {
                break;
            }
            scanner.push(&buffer[..read]);
            if scanner.output_saturated {
                return Err(TraceRedactionError::OutputTooLarge);
            }
        }

        let logical_stream = scanner.finish()?;
        let redaction_summary = logical_stream.summary().clone();
        let output_chunks = logical_stream.into_output_chunks(event_id, stream, 0)?;
        Ok(Self {
            output_chunks,
            redaction_summary,
        })
    }

    /// Partitions one complete logical stream into independently bounded wire
    /// attempts.  Each attempt has a fresh upload ID; chunks retain the event,
    /// chunk, stream and index identities assigned after EOF.
    pub fn into_upload_attempts(self) -> Result<Vec<SealedTraceUpload>, TraceRedactionError> {
        if self.output_chunks.is_empty() {
            return Err(TraceRedactionError::EmptyUpload);
        }
        partition_output_chunks(self.output_chunks, None)?
            .into_iter()
            .map(|chunks| {
                let upload_id =
                    TraceId::try_new_v7().map_err(|_| TraceRedactionError::IdGeneration)?;
                SealedTraceUpload::from_output_chunks(upload_id, chunks)
            })
            .collect()
    }

    /// Seals the event manifest and splits its complete stream across bounded
    /// attempts. The first attempt carries the event manifest; later attempts
    /// carry only additional stable chunks for that persisted event.
    pub fn into_event_upload_attempts(
        self,
        input: TraceEventText<'_>,
        secrets: &ExactSecretSet,
    ) -> Result<Vec<SealedTraceUpload>, TraceRedactionError> {
        let event = RedactedTraceEvent::from_session(input, secrets, self)?;
        SealedTraceUpload::from_event_attempts(event)
    }
}

impl fmt::Debug for TraceOutputSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceOutputSession")
            .field("output_chunk_count", &self.output_chunks.len())
            .finish()
    }
}

#[derive(Clone, Copy)]
pub struct TraceRunText<'a> {
    pub run_id: TraceId,
    pub operation_kind: &'a str,
    pub title: &'a str,
    pub outcome: TraceOutcomeV2,
    pub device_serial: Option<&'a str>,
    pub source_paths: &'a [&'a str],
    pub source_urls: &'a [&'a str],
    pub client_version: &'a str,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub error_class: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
    pub final_sequence: Option<u64>,
    pub trace_complete: bool,
    pub trace_loss_reason: Option<&'a str>,
}

impl fmt::Debug for TraceRunText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceRunText")
            .field("run_id", &self.run_id)
            .field("outcome", &self.outcome)
            .field("source_path_count", &self.source_paths.len())
            .field("source_url_count", &self.source_urls.len())
            .field("trace_complete", &self.trace_complete)
            .finish_non_exhaustive()
    }
}

pub struct RedactedTraceRun {
    run_id: TraceId,
    operation_kind: RedactedTraceText,
    title: RedactedTraceText,
    outcome: TraceOutcomeV2,
    device_serial: Option<RedactedTraceText>,
    source_paths: Vec<RedactedTraceText>,
    source_urls: Vec<RedactedTraceText>,
    client_version: RedactedTraceText,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    error_class: Option<RedactedTraceText>,
    error_code: Option<RedactedTraceText>,
    error_message: Option<RedactedTraceText>,
    final_sequence: Option<u64>,
    trace_complete: bool,
    trace_loss_reason: Option<RedactedTraceText>,
}

impl RedactedTraceRun {
    pub fn try_new(
        input: TraceRunText<'_>,
        secrets: &ExactSecretSet,
    ) -> Result<Self, TraceRedactionError> {
        validate_short(input.operation_kind)?;
        validate_text(input.title)?;
        validate_optional_short(input.device_serial)?;
        validate_text_list(input.source_paths)?;
        validate_text_list(input.source_urls)?;
        validate_short(input.client_version)?;
        validate_optional_short(input.error_class)?;
        validate_optional_short(input.error_code)?;
        validate_optional_text(input.error_message)?;
        validate_optional_text(input.trace_loss_reason)?;
        let sealed = Self {
            run_id: input.run_id,
            operation_kind: redact_field(input.operation_kind, secrets),
            title: redact_field(input.title, secrets),
            outcome: input.outcome,
            device_serial: redact_optional_field(input.device_serial, secrets),
            source_paths: redact_field_list(input.source_paths, secrets),
            source_urls: redact_field_list(input.source_urls, secrets),
            client_version: redact_field(input.client_version, secrets),
            started_at_ms: input.started_at_ms,
            ended_at_ms: input.ended_at_ms,
            duration_ms: input.duration_ms,
            error_class: redact_optional_field(input.error_class, secrets),
            error_code: redact_optional_field(input.error_code, secrets),
            error_message: redact_optional_field(input.error_message, secrets),
            final_sequence: input.final_sequence,
            trace_complete: input.trace_complete,
            trace_loss_reason: redact_optional_field(input.trace_loss_reason, secrets),
        };
        if sealed.redaction_summary().count(CredentialKind::HighRisk) > 0 {
            return Err(TraceRedactionError::HighRisk);
        }
        Ok(sealed)
    }

    pub const fn run_id(&self) -> TraceId {
        self.run_id
    }

    fn redaction_summary(&self) -> RedactionSummary {
        let mut summary = RedactionSummary::empty();
        summary.merge(self.operation_kind.summary());
        summary.merge(self.title.summary());
        if let Some(value) = &self.device_serial {
            summary.merge(value.summary());
        }
        for value in self.source_paths.iter().chain(&self.source_urls) {
            summary.merge(value.summary());
        }
        summary.merge(self.client_version.summary());
        for value in [
            self.error_class.as_ref(),
            self.error_code.as_ref(),
            self.error_message.as_ref(),
            self.trace_loss_reason.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            summary.merge(value.summary());
        }
        summary
    }
}

impl fmt::Debug for RedactedTraceRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedTraceRun")
            .field("run_id", &self.run_id)
            .field("outcome", &self.outcome)
            .field("source_path_count", &self.source_paths.len())
            .field("source_url_count", &self.source_urls.len())
            .field("trace_complete", &self.trace_complete)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
pub struct TraceEventText<'a> {
    pub event_id: TraceId,
    pub run_id: TraceId,
    pub sequence: u64,
    pub kind: TraceEventKindV2,
    pub step_name: &'a str,
    pub partition_name: Option<&'a str>,
    pub status: TraceEventStatusV2,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub command: Option<TraceCommandText<'a>>,
    pub exit_code: Option<i32>,
    pub verification: Option<&'a str>,
    pub device_state: Option<&'a str>,
    pub retry_safe: Option<bool>,
    pub remedies: &'a [&'a str],
    pub error_class: Option<&'a str>,
    pub error_code: Option<&'a str>,
    pub error_message: Option<&'a str>,
}

impl fmt::Debug for TraceEventText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceEventText")
            .field("event_id", &self.event_id)
            .field("run_id", &self.run_id)
            .field("sequence", &self.sequence)
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("has_command", &self.command.is_some())
            .field("remedy_count", &self.remedies.len())
            .finish_non_exhaustive()
    }
}

pub struct RedactedTraceEvent {
    event_id: TraceId,
    run_id: TraceId,
    sequence: u64,
    kind: TraceEventKindV2,
    step_name: RedactedTraceText,
    partition_name: Option<RedactedTraceText>,
    status: TraceEventStatusV2,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    command: Option<RedactedTraceCommand>,
    exit_code: Option<i32>,
    stdout_chunks: u64,
    stderr_chunks: u64,
    verification: Option<RedactedTraceText>,
    device_state: Option<RedactedTraceText>,
    retry_safe: Option<bool>,
    remedies: Vec<RedactedTraceText>,
    error_class: Option<RedactedTraceText>,
    error_code: Option<RedactedTraceText>,
    error_message: Option<RedactedTraceText>,
    redaction_summary: RedactionSummary,
    output_chunks: Vec<RedactedOutputChunk>,
}

impl RedactedTraceEvent {
    pub fn try_new(
        input: TraceEventText<'_>,
        secrets: &ExactSecretSet,
        stdout: Option<RedactedLogicalStream>,
        stderr: Option<RedactedLogicalStream>,
    ) -> Result<Self, TraceRedactionError> {
        validate_short(input.step_name)?;
        validate_optional_short(input.partition_name)?;
        validate_optional_text(input.verification)?;
        validate_optional_short(input.device_state)?;
        validate_text_list(input.remedies)?;
        validate_optional_short(input.error_class)?;
        validate_optional_short(input.error_code)?;
        validate_optional_text(input.error_message)?;
        if let Some(command) = &input.command {
            validate_command_bounds(command)?;
        }
        validate_event_item_bound(&input)?;

        let command = input
            .command
            .as_ref()
            .map(|command| redact_trace_command(command, secrets))
            .transpose()?;
        let step_name = redact_field(input.step_name, secrets);
        let partition_name = redact_optional_field(input.partition_name, secrets);
        let verification = redact_optional_field(input.verification, secrets);
        let device_state = redact_optional_field(input.device_state, secrets);
        let remedies = redact_field_list(input.remedies, secrets);
        let error_class = redact_optional_field(input.error_class, secrets);
        let error_code = redact_optional_field(input.error_code, secrets);
        let error_message = redact_optional_field(input.error_message, secrets);

        let mut redaction_summary = RedactionSummary::empty();
        redaction_summary.merge(step_name.summary());
        if let Some(value) = &partition_name {
            redaction_summary.merge(value.summary());
        }
        if let Some(value) = &verification {
            redaction_summary.merge(value.summary());
        }
        if let Some(value) = &device_state {
            redaction_summary.merge(value.summary());
        }
        for value in &remedies {
            redaction_summary.merge(value.summary());
        }
        if let Some(value) = &error_class {
            redaction_summary.merge(value.summary());
        }
        if let Some(value) = &error_code {
            redaction_summary.merge(value.summary());
        }
        if let Some(value) = &error_message {
            redaction_summary.merge(value.summary());
        }
        if let Some(command) = &command {
            redaction_summary.merge(&command.redaction_summary());
        }

        let mut output_chunks = Vec::new();
        let stdout_chunks = if let Some(stream) = stdout {
            redaction_summary.merge(stream.summary());
            let chunks =
                stream.into_output_chunks(input.event_id, TraceOutputStreamV2::Stdout, 0)?;
            let count = chunks.len() as u64;
            output_chunks.extend(chunks);
            count
        } else {
            0
        };
        let stderr_chunks = if let Some(stream) = stderr {
            redaction_summary.merge(stream.summary());
            let chunks =
                stream.into_output_chunks(input.event_id, TraceOutputStreamV2::Stderr, 0)?;
            let count = chunks.len() as u64;
            output_chunks.extend(chunks);
            count
        } else {
            0
        };
        if output_chunks.len() > TRACE_UPLOAD_MAX_OUTPUT_CHUNKS {
            return Err(TraceRedactionError::TooManyOutputChunks);
        }

        if redaction_summary.count(CredentialKind::HighRisk) > 0 {
            return Err(TraceRedactionError::HighRisk);
        }

        Ok(Self {
            event_id: input.event_id,
            run_id: input.run_id,
            sequence: input.sequence,
            kind: input.kind,
            step_name,
            partition_name,
            status: input.status,
            started_at_ms: input.started_at_ms,
            ended_at_ms: input.ended_at_ms,
            duration_ms: input.duration_ms,
            command,
            exit_code: input.exit_code,
            stdout_chunks,
            stderr_chunks,
            verification,
            device_state,
            retry_safe: input.retry_safe,
            remedies,
            error_class,
            error_code,
            error_message,
            redaction_summary,
            output_chunks,
        })
    }

    fn from_session(
        input: TraceEventText<'_>,
        secrets: &ExactSecretSet,
        session: TraceOutputSession,
    ) -> Result<Self, TraceRedactionError> {
        validate_short(input.step_name)?;
        validate_optional_short(input.partition_name)?;
        validate_optional_text(input.verification)?;
        validate_optional_short(input.device_state)?;
        validate_text_list(input.remedies)?;
        validate_optional_short(input.error_class)?;
        validate_optional_short(input.error_code)?;
        validate_optional_text(input.error_message)?;
        if let Some(command) = &input.command {
            validate_command_bounds(command)?;
        }
        validate_event_item_bound(&input)?;

        let command = input
            .command
            .as_ref()
            .map(|command| redact_trace_command(command, secrets))
            .transpose()?;
        let step_name = redact_field(input.step_name, secrets);
        let partition_name = redact_optional_field(input.partition_name, secrets);
        let verification = redact_optional_field(input.verification, secrets);
        let device_state = redact_optional_field(input.device_state, secrets);
        let remedies = redact_field_list(input.remedies, secrets);
        let error_class = redact_optional_field(input.error_class, secrets);
        let error_code = redact_optional_field(input.error_code, secrets);
        let error_message = redact_optional_field(input.error_message, secrets);

        let mut redaction_summary = RedactionSummary::empty();
        for value in [
            Some(&step_name),
            partition_name.as_ref(),
            verification.as_ref(),
            device_state.as_ref(),
            error_class.as_ref(),
            error_code.as_ref(),
            error_message.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            redaction_summary.merge(value.summary());
        }
        for remedy in &remedies {
            redaction_summary.merge(remedy.summary());
        }
        if let Some(command) = &command {
            redaction_summary.merge(&command.redaction_summary());
        }
        redaction_summary.merge(&session.redaction_summary);

        let mut stdout_chunks = 0_u64;
        let mut stderr_chunks = 0_u64;
        for chunk in &session.output_chunks {
            if chunk.event_id != input.event_id {
                return Err(TraceRedactionError::Contract);
            }
            match chunk.stream {
                TraceOutputStreamV2::Stdout => stdout_chunks = stdout_chunks.saturating_add(1),
                TraceOutputStreamV2::Stderr => stderr_chunks = stderr_chunks.saturating_add(1),
            }
        }
        if redaction_summary.count(CredentialKind::HighRisk) > 0 {
            return Err(TraceRedactionError::HighRisk);
        }

        Ok(Self {
            event_id: input.event_id,
            run_id: input.run_id,
            sequence: input.sequence,
            kind: input.kind,
            step_name,
            partition_name,
            status: input.status,
            started_at_ms: input.started_at_ms,
            ended_at_ms: input.ended_at_ms,
            duration_ms: input.duration_ms,
            command,
            exit_code: input.exit_code,
            stdout_chunks,
            stderr_chunks,
            verification,
            device_state,
            retry_safe: input.retry_safe,
            remedies,
            error_class,
            error_code,
            error_message,
            redaction_summary,
            output_chunks: session.output_chunks,
        })
    }

    pub const fn event_id(&self) -> TraceId {
        self.event_id
    }

    pub const fn stdout_chunks(&self) -> u64 {
        self.stdout_chunks
    }

    pub const fn stderr_chunks(&self) -> u64 {
        self.stderr_chunks
    }

    pub const fn redaction_summary(&self) -> &RedactionSummary {
        &self.redaction_summary
    }
}

impl fmt::Debug for RedactedTraceEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedTraceEvent")
            .field("event_id", &self.event_id)
            .field("run_id", &self.run_id)
            .field("sequence", &self.sequence)
            .field("kind", &self.kind)
            .field("status", &self.status)
            .field("stdout_chunks", &self.stdout_chunks)
            .field("stderr_chunks", &self.stderr_chunks)
            .field("redaction_summary", &self.redaction_summary)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize)]
struct SealedUploadWire<'a> {
    schema_version: u8,
    upload_id: TraceId,
    runs: Vec<SealedRunWire<'a>>,
    events: Vec<SealedEventWire<'a>>,
    output_chunks: Vec<SealedChunkWire<'a>>,
}

#[derive(Serialize)]
struct SealedRunWire<'a> {
    run_id: TraceId,
    operation_kind: &'a str,
    title: &'a str,
    outcome: TraceOutcomeV2,
    device_serial: Option<&'a str>,
    source_paths: Vec<&'a str>,
    source_urls: Vec<&'a str>,
    client_version: &'a str,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    error_class: Option<&'a str>,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    final_sequence: Option<u64>,
    trace_complete: bool,
    trace_loss_reason: Option<&'a str>,
}

#[derive(Serialize)]
struct SealedCommandWire<'a> {
    program: &'a str,
    argv: Vec<&'a str>,
    display_command: &'a str,
    working_directory: Option<&'a str>,
    paths: Vec<&'a str>,
    urls: Vec<&'a str>,
    serial: Option<&'a str>,
}

#[derive(Serialize)]
struct SealedRedactionWire {
    kind: &'static str,
    count: u64,
}

#[derive(Serialize)]
struct SealedEventWire<'a> {
    event_id: TraceId,
    run_id: TraceId,
    sequence: u64,
    kind: TraceEventKindV2,
    step_name: &'a str,
    partition_name: Option<&'a str>,
    status: TraceEventStatusV2,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    duration_ms: Option<u64>,
    command: Option<SealedCommandWire<'a>>,
    exit_code: Option<i32>,
    stdout_chunks: u64,
    stderr_chunks: u64,
    verification: Option<&'a str>,
    device_state: Option<&'a str>,
    retry_safe: Option<bool>,
    remedies: Vec<&'a str>,
    error_class: Option<&'a str>,
    error_code: Option<&'a str>,
    error_message: Option<&'a str>,
    credential_redactions: Vec<SealedRedactionWire>,
}

#[derive(Serialize)]
struct SealedChunkWire<'a> {
    chunk_id: TraceId,
    event_id: TraceId,
    stream: TraceOutputStreamV2,
    chunk_index: u64,
    text: &'a str,
    byte_count: u64,
    sha256: &'a str,
}

/// A bounded upload body whose only text originated from sealed chunks.
/// Metadata sealing is intentionally not widened here; Wave2 must add opaque
/// run/event types rather than accepting raw domain DTOs.
///
/// ```compile_fail
/// use nwflash_domain::TraceId;
/// use nwflash_protection::SealedTraceUpload;
///
/// // Arbitrary chunk vectors cannot bypass TraceOutputSession's EOF boundary.
/// let _ = SealedTraceUpload::from_output_chunks(
///     TraceId::try_new_v7().unwrap(),
///     Vec::new(),
/// );
/// ```
pub struct SealedTraceUpload {
    upload_id: TraceId,
    runs: Vec<RedactedTraceRun>,
    events: Vec<RedactedTraceEvent>,
    output_chunks: Vec<RedactedOutputChunk>,
}

/// Non-text identity metadata used to bind a sealed event manifest to the
/// producer run and event sequence that will persist it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedTraceEventBinding {
    pub event_id: TraceId,
    pub run_id: TraceId,
    pub sequence: u64,
    pub stdout_chunks: u64,
    pub stderr_chunks: u64,
}

impl SealedTraceUpload {
    pub fn new(
        upload_id: TraceId,
        runs: Vec<RedactedTraceRun>,
        mut events: Vec<RedactedTraceEvent>,
    ) -> Result<Self, TraceRedactionError> {
        if runs.len() > TRACE_UPLOAD_MAX_RUNS || events.len() > TRACE_UPLOAD_MAX_EVENTS {
            return Err(TraceRedactionError::TooManyItems);
        }
        let output_count = events
            .iter()
            .map(|event| event.output_chunks.len())
            .sum::<usize>();
        if output_count > TRACE_UPLOAD_MAX_OUTPUT_CHUNKS {
            return Err(TraceRedactionError::TooManyOutputChunks);
        }
        let mut output_chunks = Vec::with_capacity(output_count);
        for event in &mut events {
            output_chunks.append(&mut event.output_chunks);
        }
        if runs.is_empty() && events.is_empty() && output_chunks.is_empty() {
            return Err(TraceRedactionError::EmptyUpload);
        }
        Ok(Self {
            upload_id,
            runs,
            events,
            output_chunks,
        })
    }

    /// ```compile_fail
    /// use nwflash_domain::TraceId;
    /// use nwflash_protection::SealedTraceUpload;
    ///
    /// // Chunk vectors have no public route to a wire upload. They must come
    /// // from a TraceOutputSession that has consumed its reader to EOF.
    /// let _ = SealedTraceUpload::from_output_chunks(
    ///     TraceId::try_new_v7().unwrap(),
    ///     Vec::new(),
    /// );
    /// ```
    fn from_output_chunks(
        upload_id: TraceId,
        output_chunks: Vec<RedactedOutputChunk>,
    ) -> Result<Self, TraceRedactionError> {
        if output_chunks.is_empty() {
            return Err(TraceRedactionError::EmptyUpload);
        }
        if output_chunks.len() > TRACE_UPLOAD_MAX_OUTPUT_CHUNKS {
            return Err(TraceRedactionError::TooManyOutputChunks);
        }
        Ok(Self {
            upload_id,
            runs: Vec::new(),
            events: Vec::new(),
            output_chunks,
        })
    }

    fn from_event_attempts(
        mut event: RedactedTraceEvent,
    ) -> Result<Vec<Self>, TraceRedactionError> {
        let chunks = std::mem::take(&mut event.output_chunks);
        let mut groups = partition_output_chunks(chunks, Some(&event))?.into_iter();
        event.output_chunks = groups.next().unwrap_or_default();

        let first_upload_id =
            TraceId::try_new_v7().map_err(|_| TraceRedactionError::IdGeneration)?;
        let first = Self::new(first_upload_id, Vec::new(), vec![event])?;
        first.to_json_body()?;

        let mut attempts = vec![first];
        for chunks in groups {
            let upload_id = TraceId::try_new_v7().map_err(|_| TraceRedactionError::IdGeneration)?;
            let attempt = Self::from_output_chunks(upload_id, chunks)?;
            attempts.push(attempt);
        }
        Ok(attempts)
    }

    pub const fn upload_id(&self) -> TraceId {
        self.upload_id
    }

    pub fn output_chunks(&self) -> &[RedactedOutputChunk] {
        &self.output_chunks
    }

    /// Event batches must never smuggle run-level records through the event
    /// sequencing path. The count exposes no redacted text.
    pub const fn run_count(&self) -> usize {
        self.runs.len()
    }

    /// Returns the stable logical event identities represented by this sealed
    /// attempt. Application-level sequencing uses this safe metadata to ensure
    /// one event sequence cannot mix attempts from different events.
    pub fn event_ids(&self) -> Vec<TraceId> {
        let mut event_ids = self
            .events
            .iter()
            .map(RedactedTraceEvent::event_id)
            .chain(self.output_chunks.iter().map(RedactedOutputChunk::event_id))
            .collect::<Vec<_>>();
        event_ids.sort_unstable();
        event_ids.dedup();
        event_ids
    }

    /// Returns event manifest bindings without exposing any redacted text.
    /// Output-only continuation attempts intentionally return no bindings; a
    /// complete event batch must include exactly one manifest-bearing attempt.
    pub fn event_bindings(&self) -> Vec<SealedTraceEventBinding> {
        self.events
            .iter()
            .map(|event| SealedTraceEventBinding {
                event_id: event.event_id,
                run_id: event.run_id,
                sequence: event.sequence,
                stdout_chunks: event.stdout_chunks,
                stderr_chunks: event.stderr_chunks,
            })
            .collect()
    }

    pub fn to_json_body(&self) -> Result<Zeroizing<Vec<u8>>, TraceRedactionError> {
        sealed_upload_body(
            self.upload_id,
            &self.runs,
            &self.events,
            &self.output_chunks,
        )
    }

    pub fn write_json<W: Write>(&self, writer: &mut W) -> Result<usize, TraceRedactionError> {
        let body = self.to_json_body()?;
        writer
            .write_all(&body)
            .map_err(|_| TraceRedactionError::SinkWrite)?;
        Ok(body.len())
    }
}

impl fmt::Debug for SealedTraceUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedTraceUpload")
            .field("upload_id", &self.upload_id)
            .field("run_count", &self.runs.len())
            .field("event_count", &self.events.len())
            .field("output_chunk_count", &self.output_chunks.len())
            .finish()
    }
}

fn partition_output_chunks(
    chunks: Vec<RedactedOutputChunk>,
    first_event: Option<&RedactedTraceEvent>,
) -> Result<Vec<Vec<RedactedOutputChunk>>, TraceRedactionError> {
    if chunks.is_empty() {
        return Ok(vec![Vec::new()]);
    }

    let mut groups = Vec::new();
    let mut current = Vec::new();
    for chunk in chunks {
        current.push(chunk);
        let event_manifest = if groups.is_empty() {
            first_event.map(std::slice::from_ref).unwrap_or_default()
        } else {
            &[]
        };
        let probe_id = current[0].chunk_id;
        match sealed_upload_body(probe_id, &[], event_manifest, &current) {
            Ok(_) => {}
            Err(TraceRedactionError::RequestTooLarge) => {
                let overflow = current.pop().expect("chunk was just pushed");
                if current.is_empty() {
                    return Err(TraceRedactionError::RequestTooLarge);
                }
                groups.push(std::mem::take(&mut current));
                current.push(overflow);
            }
            Err(error) => return Err(error),
        }
    }
    if !current.is_empty() {
        groups.push(current);
    }
    Ok(groups)
}

fn sealed_upload_body(
    upload_id: TraceId,
    runs: &[RedactedTraceRun],
    events: &[RedactedTraceEvent],
    chunks: &[RedactedOutputChunk],
) -> Result<Zeroizing<Vec<u8>>, TraceRedactionError> {
    let runs = runs.iter().map(sealed_run_wire).collect();
    let events = events.iter().map(sealed_event_wire).collect();
    let output_chunks = chunks
        .iter()
        .map(|chunk| SealedChunkWire {
            chunk_id: chunk.chunk_id,
            event_id: chunk.event_id,
            stream: chunk.stream,
            chunk_index: chunk.chunk_index,
            text: chunk.text.as_str(),
            byte_count: chunk.byte_count,
            sha256: &chunk.sha256,
        })
        .collect();
    let wire = SealedUploadWire {
        schema_version: TRACE_SCHEMA_VERSION,
        upload_id,
        runs,
        events,
        output_chunks,
    };
    let mut body = Zeroizing::new(Vec::with_capacity(16 * 1024));
    let mut bounded = BoundedVecWriter::new(&mut body, TRACE_UPLOAD_MAX_BODY_BYTES);
    serde_json::to_writer(&mut bounded, &wire).map_err(|_| {
        if bounded.overflowed {
            TraceRedactionError::RequestTooLarge
        } else {
            TraceRedactionError::JsonEncoding
        }
    })?;
    TraceUploadRequestV2::from_json_slice(&body).map_err(|_| TraceRedactionError::Contract)?;
    Ok(body)
}

fn sealed_run_wire(run: &RedactedTraceRun) -> SealedRunWire<'_> {
    SealedRunWire {
        run_id: run.run_id,
        operation_kind: run.operation_kind.as_str(),
        title: run.title.as_str(),
        outcome: run.outcome,
        device_serial: run.device_serial.as_ref().map(RedactedTraceText::as_str),
        source_paths: run
            .source_paths
            .iter()
            .map(RedactedTraceText::as_str)
            .collect(),
        source_urls: run
            .source_urls
            .iter()
            .map(RedactedTraceText::as_str)
            .collect(),
        client_version: run.client_version.as_str(),
        started_at_ms: run.started_at_ms,
        ended_at_ms: run.ended_at_ms,
        duration_ms: run.duration_ms,
        error_class: run.error_class.as_ref().map(RedactedTraceText::as_str),
        error_code: run.error_code.as_ref().map(RedactedTraceText::as_str),
        error_message: run.error_message.as_ref().map(RedactedTraceText::as_str),
        final_sequence: run.final_sequence,
        trace_complete: run.trace_complete,
        trace_loss_reason: run
            .trace_loss_reason
            .as_ref()
            .map(RedactedTraceText::as_str),
    }
}

fn sealed_command_wire(command: &RedactedTraceCommand) -> SealedCommandWire<'_> {
    SealedCommandWire {
        program: command.program.as_str(),
        argv: command.argv.iter().map(RedactedTraceText::as_str).collect(),
        display_command: command.display_command.as_str(),
        working_directory: command
            .working_directory
            .as_ref()
            .map(RedactedTraceText::as_str),
        paths: command
            .paths
            .iter()
            .map(RedactedTraceText::as_str)
            .collect(),
        urls: command.urls.iter().map(RedactedTraceText::as_str).collect(),
        serial: command.serial.as_ref().map(RedactedTraceText::as_str),
    }
}

fn sealed_event_wire(event: &RedactedTraceEvent) -> SealedEventWire<'_> {
    SealedEventWire {
        event_id: event.event_id,
        run_id: event.run_id,
        sequence: event.sequence,
        kind: event.kind,
        step_name: event.step_name.as_str(),
        partition_name: event.partition_name.as_ref().map(RedactedTraceText::as_str),
        status: event.status,
        started_at_ms: event.started_at_ms,
        ended_at_ms: event.ended_at_ms,
        duration_ms: event.duration_ms,
        command: event.command.as_ref().map(sealed_command_wire),
        exit_code: event.exit_code,
        stdout_chunks: event.stdout_chunks,
        stderr_chunks: event.stderr_chunks,
        verification: event.verification.as_ref().map(RedactedTraceText::as_str),
        device_state: event.device_state.as_ref().map(RedactedTraceText::as_str),
        retry_safe: event.retry_safe,
        remedies: event
            .remedies
            .iter()
            .map(RedactedTraceText::as_str)
            .collect(),
        error_class: event.error_class.as_ref().map(RedactedTraceText::as_str),
        error_code: event.error_code.as_ref().map(RedactedTraceText::as_str),
        error_message: event.error_message.as_ref().map(RedactedTraceText::as_str),
        credential_redactions: event
            .redaction_summary
            .counts()
            .map(|count| SealedRedactionWire {
                kind: count.kind.as_str(),
                count: count.count,
            })
            .collect(),
    }
}

struct BoundedVecWriter<'a> {
    inner: &'a mut Vec<u8>,
    maximum: usize,
    overflowed: bool,
}

impl<'a> BoundedVecWriter<'a> {
    fn new(inner: &'a mut Vec<u8>, maximum: usize) -> Self {
        Self {
            inner,
            maximum,
            overflowed: false,
        }
    }
}

impl Write for BoundedVecWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.inner.len().saturating_add(bytes.len()) > self.maximum {
            self.overflowed = true;
            return Err(io::Error::other("sealed trace upload exceeds one MiB"));
        }
        self.inner.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn utf8_chunk_ranges(text: &str, maximum: usize) -> Vec<(usize, usize)> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = (start + maximum).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        debug_assert!(
            end > start,
            "UTF-8 scalar is always shorter than chunk maximum"
        );
        ranges.push((start, end));
        start = end;
    }
    ranges
}

struct ExactSecret {
    value: Zeroizing<Vec<u8>>,
}

/// Validated, bounded exact secrets used by a scanner session.
///
/// There is deliberately no mutating registration API on the scanner:
/// configuration must succeed atomically before any raw trace bytes are read.
///
/// ```compile_fail
/// # use nwflash_protection::{ExactSecretSet, TraceCredentialScanner};
/// let secrets = ExactSecretSet::try_new([b"valid-secret".as_slice()]).unwrap();
/// let mut scanner = TraceCredentialScanner::with_exact_secrets(&secrets);
/// scanner.register_exact_secret(b"late-secret");
/// ```
pub struct ExactSecretSet {
    secrets: Vec<ExactSecret>,
    total_bytes: usize,
}

impl ExactSecretSet {
    pub fn empty() -> Self {
        Self {
            secrets: Vec::new(),
            total_bytes: 0,
        }
    }

    pub fn try_new<I, S>(secrets: I) -> Result<Self, TraceRedactionError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<[u8]>,
    {
        let mut result = Self::empty();
        for secret in secrets {
            let secret = secret.as_ref();
            if result
                .secrets
                .iter()
                .any(|registered| registered.value.as_slice() == secret)
            {
                continue;
            }
            if secret.len() < MIN_TRACE_EXACT_SECRET_BYTES
                || secret.contains(&b'\r')
                || secret.contains(&b'\n')
                || result.secrets.len() >= MAX_TRACE_EXACT_SECRETS
                || result
                    .total_bytes
                    .checked_add(secret.len())
                    .is_none_or(|total| total > MAX_TRACE_EXACT_SECRET_BYTES)
                || secret_conflicts_with_marker(secret)
            {
                return Err(TraceRedactionError::InvalidExactSecret);
            }
            result.total_bytes += secret.len();
            result.secrets.push(ExactSecret {
                value: Zeroizing::new(secret.to_vec()),
            });
        }
        result
            .secrets
            .sort_by_key(|secret| Reverse(secret.value.len()));
        Ok(result)
    }

    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }
}

impl fmt::Debug for ExactSecretSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactSecretSet")
            .field("secret_count", &self.secrets.len())
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

impl fmt::Debug for ExactSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExactSecret")
            .field("bytes", &self.value.len())
            .finish()
    }
}

struct PendingPrivateKey {
    prefix: Zeroizing<String>,
    expected_end: Zeroizing<String>,
}

pub struct TraceCredentialScanner<'a> {
    carry: Zeroizing<Vec<u8>>,
    output: Zeroizing<String>,
    exact_secrets: &'a [ExactSecret],
    summary: RedactionSummary,
    private_key: Option<PendingPrivateKey>,
    output_saturated: bool,
    discarding_oversized_line: bool,
    discarding_private_line: bool,
}

impl Default for TraceCredentialScanner<'static> {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TraceCredentialScanner<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceCredentialScanner")
            .field("carry_bytes", &self.carry.len())
            .field("redacted_output_bytes", &self.output.len())
            .field("registered_exact_secrets", &self.exact_secrets.len())
            .field("inside_private_key", &self.private_key.is_some())
            .field("summary", &self.summary)
            .finish()
    }
}

impl TraceCredentialScanner<'static> {
    pub fn new() -> Self {
        Self {
            carry: Zeroizing::new(Vec::new()),
            output: Zeroizing::new(String::new()),
            exact_secrets: &[],
            summary: RedactionSummary::empty(),
            private_key: None,
            output_saturated: false,
            discarding_oversized_line: false,
            discarding_private_line: false,
        }
    }
}

impl<'a> TraceCredentialScanner<'a> {
    pub fn with_exact_secrets(secrets: &'a ExactSecretSet) -> Self {
        Self {
            carry: Zeroizing::new(Vec::new()),
            output: Zeroizing::new(String::new()),
            exact_secrets: &secrets.secrets,
            summary: RedactionSummary::empty(),
            private_key: None,
            output_saturated: false,
            discarding_oversized_line: false,
            discarding_private_line: false,
        }
    }

    fn append_redacted(&mut self, text: &str) {
        if self.output_saturated {
            return;
        }
        let Some(new_len) = self.output.len().checked_add(text.len()) else {
            self.saturate_output();
            return;
        };
        if new_len > MAX_TRACE_REDACTED_OUTPUT_BYTES {
            self.saturate_output();
            return;
        }
        self.output.push_str(text);
    }

    fn saturate_output(&mut self) {
        self.output.zeroize();
        self.output.clear();
        self.output.push_str(HIGH_RISK_REMOVED);
        self.summary.add(CredentialKind::HighRisk);
        self.output_saturated = true;
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.discarding_oversized_line {
                if byte == b'\n' {
                    self.append_redacted("[CREDENTIAL_REMOVED:HIGH_RISK]\n");
                    self.summary.add(CredentialKind::HighRisk);
                    self.discarding_oversized_line = false;
                }
                continue;
            }
            if self.discarding_private_line {
                if byte == b'\n' {
                    self.discarding_private_line = false;
                }
                continue;
            }

            self.carry.push(byte);
            if byte == b'\n' {
                self.process_complete_line();
            } else if self.carry.len() > MAX_TRACE_REDACTION_CARRY_BYTES {
                self.carry.zeroize();
                self.carry.clear();
                if self.private_key.is_some() {
                    self.discarding_private_line = true;
                } else {
                    self.discarding_oversized_line = true;
                }
            }
        }
    }

    fn finish(self) -> Result<RedactedLogicalStream, TraceRedactionError> {
        let saturated = self.output_saturated;
        let text = self.finalize_text();
        if saturated {
            Err(TraceRedactionError::OutputTooLarge)
        } else if text.summary().count(CredentialKind::HighRisk) > 0 {
            Err(TraceRedactionError::HighRisk)
        } else {
            Ok(RedactedLogicalStream { text })
        }
    }

    fn finalize_text(mut self) -> RedactedTraceText {
        if self.discarding_oversized_line {
            self.append_redacted(HIGH_RISK_REMOVED);
            self.summary.add(CredentialKind::HighRisk);
            self.discarding_oversized_line = false;
        } else if !self.carry.is_empty() && !self.discarding_private_line {
            self.process_complete_line();
        }

        if let Some(private_key) = self.private_key.take() {
            let prefix = redact_line(&private_key.prefix, self.exact_secrets, &mut self.summary)
                .unwrap_or_else(|()| Zeroizing::new(HIGH_RISK_REMOVED.to_owned()));
            let mut safe = Zeroizing::new(String::with_capacity(
                prefix.len() + HIGH_RISK_REMOVED.len(),
            ));
            safe.push_str(&prefix);
            safe.push_str(HIGH_RISK_REMOVED);
            self.append_redacted(&safe);
            self.summary.add(CredentialKind::HighRisk);
        }

        let text = std::mem::take(&mut self.output);
        let summary = std::mem::replace(&mut self.summary, RedactionSummary::empty());
        RedactedTraceText { text, summary }
    }

    fn process_complete_line(&mut self) {
        let mut bytes = std::mem::take(&mut self.carry);
        let had_newline = bytes.last() == Some(&b'\n');
        let raw = std::mem::take(&mut *bytes);
        let line = match String::from_utf8(raw) {
            Ok(line) => Zeroizing::new(line),
            Err(error) => {
                let mut invalid = error.into_bytes();
                invalid.zeroize();
                if self.private_key.is_none() {
                    self.append_redacted(HIGH_RISK_REMOVED);
                    if had_newline {
                        self.append_redacted("\n");
                    }
                    self.summary.add(CredentialKind::HighRisk);
                }
                return;
            }
        };

        if let Some(private_key) = &self.private_key {
            let without_newline = line.strip_suffix('\n').unwrap_or(&line);
            let without_cr = without_newline
                .strip_suffix('\r')
                .unwrap_or(without_newline);
            if let Some(end_start) = without_cr.find(private_key.expected_end.as_str()) {
                let end = end_start + private_key.expected_end.len();
                let suffix = &without_cr[end..];
                let private_key = self.private_key.take().expect("private key state exists");
                let prefix =
                    redact_line(&private_key.prefix, self.exact_secrets, &mut self.summary)
                        .unwrap_or_else(|()| Zeroizing::new(HIGH_RISK_REMOVED.to_owned()));
                let suffix = redact_line(suffix, self.exact_secrets, &mut self.summary)
                    .unwrap_or_else(|()| Zeroizing::new(HIGH_RISK_REMOVED.to_owned()));
                let mut safe = Zeroizing::new(String::with_capacity(
                    prefix.len() + PRIVATE_KEY_REMOVED.len() + suffix.len() + 2,
                ));
                safe.push_str(&prefix);
                safe.push_str(PRIVATE_KEY_REMOVED);
                safe.push_str(&suffix);
                safe.push_str(line_ending(&line));
                self.append_redacted(&safe);
                self.summary.add(CredentialKind::PrivateKey);
            }
            return;
        }

        if let Some(begin) = private_key_begin(&line) {
            if let Some(end_start) = line[begin.content_start..].find(begin.expected_end.as_str()) {
                let end = begin.content_start + end_start + begin.expected_end.len();
                let without_newline = line.strip_suffix('\n').unwrap_or(&line);
                let without_cr = without_newline
                    .strip_suffix('\r')
                    .unwrap_or(without_newline);
                let prefix = redact_line(&begin.prefix, self.exact_secrets, &mut self.summary)
                    .unwrap_or_else(|()| Zeroizing::new(HIGH_RISK_REMOVED.to_owned()));
                let suffix = redact_line(&without_cr[end..], self.exact_secrets, &mut self.summary)
                    .unwrap_or_else(|()| Zeroizing::new(HIGH_RISK_REMOVED.to_owned()));
                let mut safe = Zeroizing::new(String::with_capacity(
                    prefix.len() + PRIVATE_KEY_REMOVED.len() + suffix.len() + 2,
                ));
                safe.push_str(&prefix);
                safe.push_str(PRIVATE_KEY_REMOVED);
                safe.push_str(&suffix);
                safe.push_str(line_ending(&line));
                self.append_redacted(&safe);
                self.summary.add(CredentialKind::PrivateKey);
            } else {
                self.private_key = Some(PendingPrivateKey {
                    prefix: begin.prefix,
                    expected_end: begin.expected_end,
                });
            }
            return;
        }

        match redact_line(&line, self.exact_secrets, &mut self.summary) {
            Ok(redacted) => self.append_redacted(&redacted),
            Err(()) => {
                self.append_redacted(HIGH_RISK_REMOVED);
                if line.ends_with('\n') {
                    self.append_redacted("\n");
                }
                self.summary.add(CredentialKind::HighRisk);
            }
        }
    }
}

/// Borrowed command fields at the mandatory pre-spool redaction boundary.
/// Debug output intentionally exposes only collection sizes.
#[derive(Clone, Copy)]
pub struct TraceCommandText<'a> {
    pub program: &'a str,
    pub argv: &'a [&'a str],
    pub display_command: &'a str,
    pub working_directory: Option<&'a str>,
    pub paths: &'a [&'a str],
    pub urls: &'a [&'a str],
    pub serial: Option<&'a str>,
}

impl fmt::Debug for TraceCommandText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraceCommandText")
            .field("argv_items", &self.argv.len())
            .field("path_items", &self.paths.len())
            .field("url_items", &self.urls.len())
            .field("has_working_directory", &self.working_directory.is_some())
            .field("has_serial", &self.serial.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RedactedTraceCommand {
    program: RedactedTraceText,
    argv: Vec<RedactedTraceText>,
    display_command: RedactedTraceText,
    working_directory: Option<RedactedTraceText>,
    paths: Vec<RedactedTraceText>,
    urls: Vec<RedactedTraceText>,
    serial: Option<RedactedTraceText>,
}

impl RedactedTraceCommand {
    fn redaction_summary(&self) -> RedactionSummary {
        let mut summary = RedactionSummary::empty();
        summary.merge(self.program.summary());
        for value in &self.argv {
            summary.merge(value.summary());
        }
        summary.merge(self.display_command.summary());
        if let Some(value) = &self.working_directory {
            summary.merge(value.summary());
        }
        for value in &self.paths {
            summary.merge(value.summary());
        }
        for value in &self.urls {
            summary.merge(value.summary());
        }
        if let Some(value) = &self.serial {
            summary.merge(value.summary());
        }
        summary
    }

    pub fn program(&self) -> &str {
        self.program.as_str()
    }

    pub fn argv(&self) -> Vec<&str> {
        self.argv.iter().map(RedactedTraceText::as_str).collect()
    }

    pub fn display_command(&self) -> &str {
        self.display_command.as_str()
    }

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory
            .as_ref()
            .map(RedactedTraceText::as_str)
    }

    pub fn paths(&self) -> Vec<&str> {
        self.paths.iter().map(RedactedTraceText::as_str).collect()
    }

    pub fn urls(&self) -> Vec<&str> {
        self.urls.iter().map(RedactedTraceText::as_str).collect()
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_ref().map(RedactedTraceText::as_str)
    }
}

impl fmt::Debug for RedactedTraceCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedactedTraceCommand")
            .field("program", &self.program)
            .field("argv", &self.argv)
            .field("display_command", &self.display_command)
            .field("working_directory", &self.working_directory)
            .field("paths", &self.paths)
            .field("urls", &self.urls)
            .field("serial", &self.serial)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceRedactionError {
    InvalidExactSecret,
    TextBounds,
    MetadataTooLarge,
    TooManyItems,
    OutputTooLarge,
    TooManyOutputChunks,
    InvalidChunkIndex,
    IdGeneration,
    EmptyUpload,
    RequestTooLarge,
    JsonEncoding,
    SinkWrite,
    SourceRead,
    HighRisk,
    Contract,
}

/// Redacts a complete structured command before it can enter the trace spool.
/// A credential flag and its value are treated as one semantic unit even when
/// they occupy separate argv elements.
pub fn redact_trace_command(
    command: &TraceCommandText<'_>,
    exact_secrets: &ExactSecretSet,
) -> Result<RedactedTraceCommand, TraceRedactionError> {
    validate_command_bounds(command)?;
    let program = redact_field(command.program, exact_secrets);
    let display_command = redact_field(command.display_command, exact_secrets);
    let working_directory = command
        .working_directory
        .map(|value| redact_field(value, exact_secrets));
    let paths = command
        .paths
        .iter()
        .map(|value| redact_field(value, exact_secrets))
        .collect();
    let urls = command
        .urls
        .iter()
        .map(|value| redact_field(value, exact_secrets))
        .collect();
    let serial = command
        .serial
        .map(|value| redact_field(value, exact_secrets));

    let mut argv = Vec::with_capacity(command.argv.len());
    let mut pending_kind = None;
    for argument in command.argv {
        if let Some(kind) = pending_kind.take() {
            if let Some(next_kind) = cli_flag_kind(argument) {
                argv.push(redact_field(argument, exact_secrets));
                pending_kind = Some(next_kind);
                continue;
            }
            if is_recognized_noncredential_option(argument) {
                argv.push(redact_field(argument, exact_secrets));
                continue;
            }
            argv.push(redacted_explicit_value(argument, kind));
            continue;
        }
        let redacted = redact_field(argument, exact_secrets);
        pending_kind = cli_flag_kind(argument);
        argv.push(redacted);
    }

    let redacted = RedactedTraceCommand {
        program,
        argv,
        display_command,
        working_directory,
        paths,
        urls,
        serial,
    };
    if redacted.redaction_summary().count(CredentialKind::HighRisk) > 0 {
        return Err(TraceRedactionError::HighRisk);
    }
    Ok(redacted)
}

fn is_recognized_noncredential_option(value: &str) -> bool {
    matches!(
        value.to_ascii_lowercase().as_str(),
        "--label"
            | "--mode"
            | "--slot"
            | "--partition"
            | "--input"
            | "--output"
            | "--verbose"
            | "--quiet"
            | "--force"
            | "--dry-run"
            | "--help"
            | "--version"
    )
}

fn validate_short(value: &str) -> Result<(), TraceRedactionError> {
    (value.len() <= TRACE_SHORT_TEXT_MAX_BYTES)
        .then_some(())
        .ok_or(TraceRedactionError::TextBounds)
}

fn validate_text(value: &str) -> Result<(), TraceRedactionError> {
    (value.len() <= TRACE_TEXT_MAX_BYTES)
        .then_some(())
        .ok_or(TraceRedactionError::TextBounds)
}

fn validate_optional_short(value: Option<&str>) -> Result<(), TraceRedactionError> {
    value.map(validate_short).transpose().map(|_| ())
}

fn validate_optional_text(value: Option<&str>) -> Result<(), TraceRedactionError> {
    value.map(validate_text).transpose().map(|_| ())
}

fn validate_text_list(values: &[&str]) -> Result<(), TraceRedactionError> {
    if values.len() > TRACE_TEXT_LIST_MAX_ITEMS {
        return Err(TraceRedactionError::TextBounds);
    }
    for value in values {
        validate_text(value)?;
    }
    Ok(())
}

fn validate_command_bounds(command: &TraceCommandText<'_>) -> Result<(), TraceRedactionError> {
    validate_text(command.program)?;
    validate_text_list(command.argv)?;
    validate_text(command.display_command)?;
    validate_optional_text(command.working_directory)?;
    validate_text_list(command.paths)?;
    validate_text_list(command.urls)?;
    validate_optional_short(command.serial)
}

fn validate_event_item_bound(event: &TraceEventText<'_>) -> Result<(), TraceRedactionError> {
    let mut bytes = 1024usize;
    let mut add = |value: &str| -> Result<(), TraceRedactionError> {
        bytes = bytes
            .checked_add(value.len())
            .ok_or(TraceRedactionError::MetadataTooLarge)?;
        Ok(())
    };
    add(event.step_name)?;
    for value in [
        event.partition_name,
        event.verification,
        event.device_state,
        event.error_class,
        event.error_code,
        event.error_message,
    ]
    .into_iter()
    .flatten()
    {
        add(value)?;
    }
    for value in event.remedies {
        add(value)?;
    }
    if let Some(command) = &event.command {
        add(command.program)?;
        add(command.display_command)?;
        for value in command.argv.iter().chain(command.paths).chain(command.urls) {
            add(value)?;
        }
        for value in [command.working_directory, command.serial]
            .into_iter()
            .flatten()
        {
            add(value)?;
        }
    }
    (bytes <= TRACE_UPLOAD_MAX_BODY_BYTES)
        .then_some(())
        .ok_or(TraceRedactionError::MetadataTooLarge)
}

fn redact_optional_field(
    value: Option<&str>,
    exact_secrets: &ExactSecretSet,
) -> Option<RedactedTraceText> {
    value.map(|value| redact_field(value, exact_secrets))
}

fn redact_field_list(values: &[&str], exact_secrets: &ExactSecretSet) -> Vec<RedactedTraceText> {
    values
        .iter()
        .map(|value| redact_field(value, exact_secrets))
        .collect()
}

fn redact_field(value: &str, exact_secrets: &ExactSecretSet) -> RedactedTraceText {
    let mut scanner = TraceCredentialScanner::with_exact_secrets(exact_secrets);
    scanner.push(value.as_bytes());
    scanner.finalize_text()
}

fn redacted_explicit_value(value: &str, kind: CredentialKind) -> RedactedTraceText {
    let mut summary = RedactionSummary::empty();
    summary.add(kind);
    RedactedTraceText {
        text: Zeroizing::new(safe_credential_marker(value)),
        summary,
    }
}

fn cli_flag_kind(value: &str) -> Option<CredentialKind> {
    value
        .strip_prefix("--")
        .filter(|key| !key.is_empty() && key.bytes().all(is_key_byte))
        .and_then(credential_kind)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceCredentialSentinel {
    redaction_count: u64,
    high_risk: bool,
}

impl TraceCredentialSentinel {
    pub const fn redaction_count(self) -> u64 {
        self.redaction_count
    }

    pub const fn high_risk(self) -> bool {
        self.high_risk
    }
}

#[inline(never)]
#[export_name = "nwflash_protection_trace_credential_sentinel"]
pub fn trace_credential_sentinel(session: &TraceOutputSession) -> TraceCredentialSentinel {
    begin_trace_credential_sentinel();
    let sentinel = TraceCredentialSentinel {
        redaction_count: session.redaction_summary.total(),
        high_risk: session.redaction_summary.count(CredentialKind::HighRisk) > 0,
    };
    end_marker();
    sentinel
}

struct PrivateKeyBegin {
    prefix: Zeroizing<String>,
    expected_end: Zeroizing<String>,
    content_start: usize,
}

fn private_key_begin(line: &str) -> Option<PrivateKeyBegin> {
    let without_newline = line.strip_suffix('\n').unwrap_or(line);
    let without_cr = without_newline
        .strip_suffix('\r')
        .unwrap_or(without_newline);
    let mut search_from = 0;
    while let Some(relative_begin) = without_cr[search_from..].find("-----BEGIN ") {
        let begin = search_from + relative_begin;
        let label_start = begin + "-----BEGIN ".len();
        let Some(relative_end) = without_cr[label_start..].find("-----") else {
            search_from = label_start;
            continue;
        };
        let label_end = label_start + relative_end;
        let label = &without_cr[label_start..label_end];
        if label.ends_with("PRIVATE KEY")
            && !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ')
        {
            let mut expected_end = Zeroizing::new(String::from("-----END "));
            expected_end.push_str(label);
            expected_end.push_str("-----");
            return Some(PrivateKeyBegin {
                prefix: Zeroizing::new(without_cr[..begin].to_owned()),
                expected_end,
                content_start: label_end + "-----".len(),
            });
        }
        search_from = label_end + "-----".len();
    }
    None
}

fn redact_line(
    line: &str,
    exact_secrets: &[ExactSecret],
    summary: &mut RedactionSummary,
) -> Result<Zeroizing<String>, ()> {
    let mut output = Zeroizing::new(line.to_owned());
    redact_authorization(&mut output, summary);
    redact_cookies(&mut output, summary)?;
    redact_urls(&mut output, summary)?;
    redact_cli_assignments(&mut output, summary)?;
    redact_assignments(&mut output, summary)?;
    redact_exact_secrets(&mut output, exact_secrets, summary);
    Ok(output)
}

fn redact_authorization(text: &mut String, summary: &mut RedactionSummary) {
    let mut search_from = 0;
    while let Some(header_start) = find_ascii_case_insensitive(text, "authorization", search_from) {
        let after_name = header_start + "authorization".len();
        if !is_word_boundary(text.as_bytes(), header_start, after_name) {
            search_from = after_name;
            continue;
        }
        let mut cursor = skip_ascii_space(text.as_bytes(), after_name);
        if text.as_bytes().get(cursor) == Some(&b'"') {
            cursor = skip_ascii_space(text.as_bytes(), cursor + 1);
        }
        if text.as_bytes().get(cursor) == Some(&b':') {
            cursor = skip_ascii_space(text.as_bytes(), cursor + 1);
            let (value_start, end) =
                quoted_value_bounds(text, cursor).unwrap_or((cursor, trim_line_end(text, cursor)));
            if cursor < end {
                let (credential_start, kind) =
                    authorization_credential_start(text, value_start, end);
                if credential_start < end && !is_redacted(&text[credential_start..end]) {
                    let replacement = safe_credential_marker(&text[credential_start..end]);
                    text.replace_range(credential_start..end, &replacement);
                    summary.add(kind);
                    search_from = credential_start + replacement.len();
                    continue;
                }
            }
        }
        search_from = after_name;
    }

    let mut cursor = 0;
    while let Some(index) = find_ascii_case_insensitive(text, "bearer", cursor) {
        let after = index + "bearer".len();
        if !is_word_boundary(text.as_bytes(), index, after)
            || !text
                .as_bytes()
                .get(after)
                .is_some_and(u8::is_ascii_whitespace)
        {
            cursor = after;
            continue;
        }
        let start = skip_ascii_space(text.as_bytes(), after);
        if ["authentication", "authorization"].into_iter().any(|word| {
            ascii_prefix(text, start, word)
                .is_some_and(|word_end| is_word_boundary(text.as_bytes(), start, word_end))
        }) {
            cursor = after;
            continue;
        }
        let mut end = start;
        while text
            .as_bytes()
            .get(end)
            .is_some_and(|byte| is_bearer_byte(*byte))
        {
            end += 1;
        }
        if end.saturating_sub(start) >= 6 && !is_redacted(&text[start..end]) {
            let replacement = safe_credential_marker(&text[start..end]);
            text.replace_range(start..end, &replacement);
            summary.add(CredentialKind::Bearer);
            cursor = start + replacement.len();
        } else {
            cursor = after;
        }
    }
}

fn quoted_value_bounds(text: &str, cursor: usize) -> Option<(usize, usize)> {
    if text.as_bytes().get(cursor) != Some(&b'"') {
        return None;
    }
    let start = cursor + 1;
    let bytes = text.as_bytes();
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate().skip(start) {
        let byte = *byte;
        if escaped {
            escaped = false;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'"' {
            return Some((start, index));
        }
    }
    None
}

fn authorization_credential_start(text: &str, start: usize, end: usize) -> (usize, CredentialKind) {
    for (scheme, kind) in [
        ("bearer", CredentialKind::Bearer),
        ("basic", CredentialKind::Authorization),
        ("digest", CredentialKind::Authorization),
    ] {
        if let Some(after) = ascii_prefix(text, start, scheme) {
            if after < end && text.as_bytes()[after].is_ascii_whitespace() {
                return (skip_ascii_space(text.as_bytes(), after), kind);
            }
        }
    }
    (start, CredentialKind::Authorization)
}

fn redact_cookies(text: &mut String, summary: &mut RedactionSummary) -> Result<(), ()> {
    let mut search_from = 0;
    loop {
        let set = find_cookie_header(text, "set-cookie", search_from);
        let plain = find_cookie_header(text, "cookie", search_from);
        let (header_start, set_cookie, name_len) = match (set, plain) {
            (Some(set), Some(plain)) if set <= plain => (set, true, "set-cookie".len()),
            (_, Some(plain)) => (plain, false, "cookie".len()),
            (Some(set), None) => (set, true, "set-cookie".len()),
            (None, None) => return Ok(()),
        };
        let after_name = header_start + name_len;
        let mut cursor = skip_ascii_space(text.as_bytes(), after_name);
        if text.as_bytes().get(cursor) == Some(&b'"') {
            cursor = skip_ascii_space(text.as_bytes(), cursor + 1);
        }
        if text.as_bytes().get(cursor) != Some(&b':') {
            search_from = after_name;
            continue;
        }
        cursor = skip_ascii_space(text.as_bytes(), cursor + 1);
        let (cookie_start, quoted_end) = quoted_value_bounds(text, cursor)
            .map(|(start, end)| (start, Some(end)))
            .unwrap_or((cursor, None));
        if quoted_end.is_some() {
            cursor = cookie_start;
        }
        let next_set = find_cookie_header(text, "set-cookie", cursor);
        let next_plain = find_cookie_header(text, "cookie", cursor);
        let end = quoted_end.unwrap_or_else(|| {
            [next_set, next_plain]
                .into_iter()
                .flatten()
                .min()
                .unwrap_or_else(|| trim_line_end(text, cursor))
        });
        if cursor >= end {
            search_from = after_name;
            continue;
        }

        let mut replacements = Vec::new();
        if set_cookie {
            let equals = text[cursor..end]
                .find('=')
                .map(|offset| cursor + offset)
                .ok_or(())?;
            let value_start = equals + 1;
            let value_end = text[value_start..end]
                .find(';')
                .map(|offset| value_start + offset)
                .unwrap_or(end);
            if value_start < value_end && !is_redacted(&text[value_start..value_end]) {
                replacements.push((value_start, value_end));
            }
        } else {
            let mut segment_start = cursor;
            while segment_start < end {
                let segment_end = text[segment_start..end]
                    .find(';')
                    .map(|offset| segment_start + offset)
                    .unwrap_or(end);
                let equals = text[segment_start..segment_end]
                    .find('=')
                    .map(|offset| segment_start + offset)
                    .ok_or(())?;
                let value_start = equals + 1;
                if value_start < segment_end && !is_redacted(&text[value_start..segment_end]) {
                    replacements.push((value_start, segment_end));
                }
                segment_start = segment_end.saturating_add(1);
            }
        }

        let mut next_search = end;
        for (value_start, value_end) in replacements.into_iter().rev() {
            let replacement = safe_credential_marker(&text[value_start..value_end]);
            text.replace_range(value_start..value_end, &replacement);
            summary.add(CredentialKind::Cookie);
            if value_end <= next_search {
                next_search = next_search - (value_end - value_start) + replacement.len();
            }
        }
        search_from = next_search.max(after_name);
    }
}

fn find_cookie_header(text: &str, name: &str, mut from: usize) -> Option<usize> {
    while let Some(start) = find_ascii_case_insensitive(text, name, from) {
        let end = start + name.len();
        if is_word_boundary(text.as_bytes(), start, end) {
            let mut colon = skip_ascii_space(text.as_bytes(), end);
            if text.as_bytes().get(colon) == Some(&b'"') {
                colon = skip_ascii_space(text.as_bytes(), colon + 1);
            }
            if text.as_bytes().get(colon) == Some(&b':') {
                return Some(start);
            }
        }
        from = end;
    }
    None
}

fn redact_urls(text: &mut String, summary: &mut RedactionSummary) -> Result<(), ()> {
    let mut cursor = 0;
    loop {
        let http = find_ascii_case_insensitive(text, "http://", cursor);
        let https = find_ascii_case_insensitive(text, "https://", cursor);
        let Some(start) = [http, https].into_iter().flatten().min() else {
            return Ok(());
        };
        let scheme_end = text[start..]
            .find("//")
            .map(|offset| start + offset + 2)
            .expect("matched URL has a scheme");
        let mut end = scheme_end;
        while text.as_bytes().get(end).is_some_and(|byte| {
            !byte.is_ascii_whitespace() && !matches!(*byte, b'"' | b'\'' | b'<' | b'>')
        }) {
            end += 1;
        }
        let authority_end = text[scheme_end..end]
            .find(['/', '?', '#'])
            .map(|offset| scheme_end + offset)
            .unwrap_or(end);
        if let Some(at_offset) = text[scheme_end..authority_end].rfind('@') {
            let at = scheme_end + at_offset;
            if scheme_end == at {
                return Err(());
            }
            if !is_redacted(&text[scheme_end..at]) {
                let replacement = safe_credential_marker(&text[scheme_end..at]);
                text.replace_range(scheme_end..at, &replacement);
                summary.add(CredentialKind::UrlUserinfo);
                cursor = scheme_end + replacement.len();
                continue;
            }
        }
        cursor = end;
    }
}

fn redact_cli_assignments(text: &mut String, summary: &mut RedactionSummary) -> Result<(), ()> {
    let mut cursor = 0;
    while cursor + 2 <= text.len() {
        let Some(relative) = text[cursor..].find("--") else {
            break;
        };
        let start = cursor + relative;
        if start > 0 && is_key_byte(text.as_bytes()[start - 1]) {
            cursor = start + 2;
            continue;
        }
        let key_start = start + 2;
        let mut key_end = key_start;
        while text
            .as_bytes()
            .get(key_end)
            .is_some_and(|byte| is_key_byte(*byte))
        {
            key_end += 1;
        }
        let Some(kind) = credential_kind(&text[key_start..key_end]) else {
            cursor = key_end.max(start + 2);
            continue;
        };
        let separator_start = key_end;
        let after_space = skip_ascii_space(text.as_bytes(), key_end);
        let value_start = if text.as_bytes().get(after_space) == Some(&b'=') {
            skip_ascii_space(text.as_bytes(), after_space + 1)
        } else if after_space > separator_start {
            after_space
        } else {
            cursor = key_end;
            continue;
        };
        if text[value_start..].starts_with('-') {
            let following_end = text[value_start..]
                .find(|character: char| {
                    character.is_ascii_whitespace() || matches!(character, ',' | ';' | '&')
                })
                .map(|offset| value_start + offset)
                .unwrap_or(text.len());
            let following = &text[value_start..following_end];
            if (following.starts_with("--")
                && credential_kind(following.trim_start_matches('-')).is_some())
                || is_recognized_noncredential_option(following)
            {
                cursor = value_start;
                continue;
            }
        }
        let Some((value_end, _quote)) = credential_value_end(text, value_start)? else {
            cursor = value_start;
            continue;
        };
        if !is_redacted(&text[value_start..value_end]) {
            let replacement_len = replace_credential_value(text, value_start, value_end);
            summary.add(kind);
            cursor = value_start + replacement_len;
        } else {
            cursor = value_end;
        }
    }
    Ok(())
}

fn redact_assignments(text: &mut String, summary: &mut RedactionSummary) -> Result<(), ()> {
    let mut cursor = 0;
    while cursor < text.len() {
        if !is_key_byte(text.as_bytes()[cursor])
            || (cursor > 0 && is_key_byte(text.as_bytes()[cursor - 1]))
        {
            cursor += text[cursor..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let key_start = cursor;
        let mut key_end = key_start;
        while text
            .as_bytes()
            .get(key_end)
            .is_some_and(|byte| is_key_byte(*byte))
        {
            key_end += 1;
        }
        let Some(kind) = credential_kind(&text[key_start..key_end]) else {
            cursor = key_end;
            continue;
        };
        let mut separator = key_end;
        if text.as_bytes().get(separator) == Some(&b'"')
            && key_start > 0
            && text.as_bytes()[key_start - 1] == b'"'
        {
            separator += 1;
        }
        separator = skip_ascii_space(text.as_bytes(), separator);
        if !matches!(text.as_bytes().get(separator), Some(b':') | Some(b'=')) {
            cursor = key_end;
            continue;
        }
        let value_start = skip_ascii_space(text.as_bytes(), separator + 1);
        let Some((value_end, _quote)) = credential_value_end(text, value_start)? else {
            cursor = value_start;
            continue;
        };
        if !is_redacted(&text[value_start..value_end]) {
            let replacement_len = replace_credential_value(text, value_start, value_end);
            summary.add(kind);
            cursor = value_start + replacement_len;
        } else {
            cursor = value_end;
        }
    }
    Ok(())
}

fn credential_value_end(text: &str, start: usize) -> Result<Option<(usize, Option<u8>)>, ()> {
    let Some(&first) = text.as_bytes().get(start) else {
        return Ok(None);
    };
    if matches!(first, b'"' | b'\'') {
        let mut cursor = start + 1;
        while cursor < text.len() {
            match text.as_bytes()[cursor] {
                b'\\' => cursor = (cursor + 2).min(text.len()),
                byte if byte == first => return Ok(Some((cursor + 1, Some(first)))),
                _ => cursor += 1,
            }
        }
        return Err(());
    }
    let mut end = start;
    while text
        .as_bytes()
        .get(end)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b',' | b';' | b'&'))
    {
        end += 1;
    }
    Ok((end > start).then_some((end, None)))
}

fn replace_credential_value(text: &mut String, start: usize, end: usize) -> usize {
    let bytes = text.as_bytes();
    if end > start + 1 && matches!(bytes[start], b'"' | b'\'') && bytes[end - 1] == bytes[start] {
        let quote = bytes[start] as char;
        let replacement = safe_credential_marker(&text[start + 1..end - 1]);
        let mut quoted = String::with_capacity(replacement.len() + 2);
        quoted.push(quote);
        quoted.push_str(&replacement);
        quoted.push(quote);
        let length = quoted.len();
        text.replace_range(start..end, &quoted);
        length
    } else {
        let replacement = safe_credential_marker(&text[start..end]);
        let length = replacement.len();
        text.replace_range(start..end, &replacement);
        length
    }
}

fn redact_exact_secrets(
    text: &mut String,
    exact_secrets: &[ExactSecret],
    summary: &mut RedactionSummary,
) {
    for exact in exact_secrets {
        let Ok(secret) = std::str::from_utf8(exact.value.as_slice()) else {
            continue;
        };
        let mut cursor = 0;
        while let Some(offset) = text[cursor..].find(secret) {
            let start = cursor + offset;
            let replacement = safe_credential_marker(secret);
            text.replace_range(start..start + secret.len(), &replacement);
            summary.add(CredentialKind::Exact);
            cursor = start + replacement.len();
        }
    }
}

fn credential_kind(key: &str) -> Option<CredentialKind> {
    let normalized = Zeroizing::new(key.to_ascii_lowercase().replace('_', "-"));
    let has_suffix =
        |suffix: &str| normalized.as_str() == suffix || normalized.ends_with(&format!("-{suffix}"));
    if has_suffix("password") || has_suffix("passwd") || has_suffix("pwd") {
        Some(CredentialKind::Password)
    } else if has_suffix("token") {
        Some(CredentialKind::Token)
    } else if has_suffix("api-key") || has_suffix("apikey") {
        Some(CredentialKind::ApiKey)
    } else if has_suffix("secret") || has_suffix("secret-access-key") {
        Some(CredentialKind::Secret)
    } else if has_suffix("signature") || has_suffix("sig") {
        Some(CredentialKind::Signature)
    } else {
        None
    }
}

fn is_redacted(value: &str) -> bool {
    let trimmed = value.trim_matches(['"', '\'', ' ', '\t']);
    trimmed == REDACTED
        || (trimmed.starts_with("[CREDENTIAL_REMOVED:") && trimmed.ends_with(']'))
        || (!trimmed.is_empty() && trimmed.bytes().all(|byte| byte == b'*'))
}

fn safe_credential_marker(value: &str) -> String {
    if value.len() >= REDACTED.len() {
        REDACTED.to_owned()
    } else {
        "*".repeat(value.len().max(1))
    }
}

fn secret_conflicts_with_marker(secret: &[u8]) -> bool {
    let Ok(secret) = std::str::from_utf8(secret) else {
        return true;
    };
    if secret.bytes().all(|byte| byte == b'*') {
        return true;
    }
    [REDACTED, PRIVATE_KEY_REMOVED, HIGH_RISK_REMOVED]
        .into_iter()
        .any(|marker| marker.contains(secret) || secret.contains(marker))
}

fn line_ending(line: &str) -> &'static str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        ""
    }
}

fn trim_line_end(text: &str, start: usize) -> usize {
    let mut end = text.len();
    while end > start && matches!(text.as_bytes()[end - 1], b'\r' | b'\n') {
        end -= 1;
    }
    end
}

fn ascii_prefix(text: &str, start: usize, prefix: &str) -> Option<usize> {
    let end = start.checked_add(prefix.len())?;
    text.get(start..end)
        .filter(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .map(|_| end)
}

fn find_ascii_case_insensitive(text: &str, needle: &str, from: usize) -> Option<usize> {
    let needle = needle.as_bytes();
    text.as_bytes()[from..]
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
        .map(|offset| from + offset)
}

fn skip_ascii_space(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(*byte, b' ' | b'\t'))
    {
        cursor += 1;
    }
    cursor
}

fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn is_word_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    (start == 0 || !is_key_byte(bytes[start - 1]))
        && (end == bytes.len() || !is_key_byte(bytes[end]))
}

fn is_bearer_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
}

#[cfg(test)]
mod tests {
    use std::mem;

    use zeroize::Zeroize as _;

    use super::*;

    struct BudgetedInfiniteReader {
        read_bytes: usize,
        error_after: usize,
    }

    impl io::Read for BudgetedInfiniteReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.read_bytes >= self.error_after {
                return Err(io::Error::other("test reader must not reach EOF"));
            }
            let count = buffer.len().min(self.error_after - self.read_bytes);
            for (offset, byte) in buffer[..count].iter_mut().enumerate() {
                *byte = if (self.read_bytes + offset + 1).is_multiple_of(1024) {
                    b'\n'
                } else {
                    b'x'
                };
            }
            self.read_bytes += count;
            Ok(count)
        }
    }

    const SENTINELS: &[&str] = &[
        "bearer-sentinel-100001",
        "basic-sentinel-100002",
        "cookie-sentinel-100003",
        "set-cookie-sentinel-100004",
        "password-sentinel-100005",
        "passwd-sentinel-100006",
        "token-sentinel-100007",
        "api-key-sentinel-100008",
        "secret-sentinel-100009",
        "signature-sentinel-100010",
        "cli-sentinel-100011",
        "url-password-sentinel-100012",
        "query-sentinel-100013",
        "pem-sentinel-100014",
        "openssh-sentinel-100015",
        "exact-sentinel-100016",
    ];

    fn hostile_fixture() -> String {
        [
            "Authorization: Bearer bearer-sentinel-100001\n",
            "Authorization: Basic basic-sentinel-100002\n",
            "Bearer bearer-sentinel-100001\n",
            "Cookie: session=cookie-sentinel-100003; theme=dark\n",
            "Set-Cookie: session=set-cookie-sentinel-100004; Secure; HttpOnly\n",
            "password=password-sentinel-100005 passwd: 'passwd-sentinel-100006'\n",
            "token=token-sentinel-100007 api_key=api-key-sentinel-100008\n",
            "client-secret=secret-sentinel-100009 signature=signature-sentinel-100010\n",
            "tool --password cli-sentinel-100011 --api-key=api-key-sentinel-100008\n",
            "https://operator:url-password-sentinel-100012@example.test/path?token=query-sentinel-100013&mode=flash\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "pem-sentinel-100014\n",
            "-----END PRIVATE KEY-----\n",
            "-----BEGIN OPENSSH PRIVATE KEY-----\n",
            "openssh-sentinel-100015\n",
            "-----END OPENSSH PRIVATE KEY-----\n",
            "registered=exact-sentinel-100016\n",
        ]
        .concat()
    }

    fn scan_with_splits(input: &[u8], splits: &[usize]) -> RedactedTraceText {
        let secrets = ExactSecretSet::try_new([SENTINELS.last().unwrap().as_bytes()])
            .expect("valid exact secret set");
        let mut scanner = TraceCredentialScanner::with_exact_secrets(&secrets);
        let mut start = 0;
        for &end in splits {
            scanner.push(&input[start..end]);
            start = end;
        }
        scanner.push(&input[start..]);
        scanner.finalize_text()
    }

    fn command_contains(command: &RedactedTraceCommand, needle: &str) -> bool {
        command.program().contains(needle)
            || command.argv().iter().any(|value| value.contains(needle))
            || command.display_command().contains(needle)
            || command
                .working_directory()
                .is_some_and(|value| value.contains(needle))
            || command.paths().iter().any(|value| value.contains(needle))
            || command.urls().iter().any(|value| value.contains(needle))
            || command.serial().is_some_and(|value| value.contains(needle))
    }

    #[test]
    fn redacts_supported_credentials_and_preserves_operational_context() {
        let redacted = scan_with_splits(hostile_fixture().as_bytes(), &[]);
        for secret in SENTINELS {
            assert!(!redacted.as_str().contains(secret), "leaked {secret}");
        }
        assert!(redacted
            .as_str()
            .contains("Authorization: Bearer [REDACTED]"));
        assert!(redacted
            .as_str()
            .contains("Cookie: session=[REDACTED]; theme=****"));
        assert!(redacted
            .as_str()
            .contains("https://[REDACTED]@example.test/path"));
        assert!(redacted.as_str().contains("&mode=flash"));
        assert!(redacted.as_str().contains(PRIVATE_KEY_REMOVED));
        assert!(!redacted.as_str().contains("-----BEGIN PRIVATE KEY-----"));
        assert!(!redacted.as_str().contains("-----END PRIVATE KEY-----"));
        assert!(redacted.summary().total() >= SENTINELS.len() as u64);
        assert_eq!(redacted.summary().count(CredentialKind::Exact), 1);
        assert_eq!(redacted.summary().count(CredentialKind::PrivateKey), 2);
    }

    #[test]
    fn every_single_split_matches_one_shot_and_leaks_no_sentinel() {
        let input = hostile_fixture();
        let expected = scan_with_splits(input.as_bytes(), &[]);
        for split in 0..=input.len() {
            let actual = scan_with_splits(input.as_bytes(), &[split]);
            assert_eq!(actual.as_str(), expected.as_str(), "split {split}");
            assert_eq!(actual.summary(), expected.summary(), "split {split}");
            for secret in SENTINELS {
                assert!(
                    !actual.as_str().contains(secret),
                    "split {split} leaked {secret}"
                );
            }
        }
    }

    #[test]
    fn byte_at_a_time_and_future_32k_chunks_preserve_utf8_and_redaction() {
        let mut input = "开始🚀\n".to_owned();
        let header = "Authorization: Bearer ";
        let secret = "bearer-sentinel-100001";
        let target_secret_start = 32_764;
        let padding = target_secret_start - input.len() - header.len();
        input.push_str(&"x\n".repeat(padding / 2));
        if padding % 2 == 1 {
            input.push('\n');
        }
        input.push_str(header);
        let secret_start = input.len();
        input.push_str(secret);
        input.push_str("\n结束✅");
        assert!(secret_start < 32_768 && secret_start + secret.len() > 32_768);
        let bytes = input.as_bytes();
        let one_shot = scan_with_splits(bytes, &[]);
        let byte_splits: Vec<_> = (1..bytes.len()).collect();
        let bytewise = scan_with_splits(bytes, &byte_splits);
        let chunk_splits: Vec<_> = (32_768..bytes.len()).step_by(32_768).collect();
        let chunked = scan_with_splits(bytes, &chunk_splits);

        assert_eq!(bytewise, one_shot);
        assert_eq!(chunked, one_shot);
        assert!(one_shot.as_str().contains("开始🚀"));
        assert!(!one_shot.as_str().contains("bearer-sentinel-100001"));
    }

    #[test]
    fn safe_cli_option_boundaries_and_existing_markers_are_not_consumed() {
        let input = [
            "tool --token --label nightly\n",
            "Bearer authentication failed\n",
            "password=[REDACTED]\n",
            "Cookie: a=[REDACTED]; b=cookie-sentinel-100003\n",
        ]
        .concat();
        let redacted = scan_with_splits(input.as_bytes(), &[3, 14, 31]);

        assert!(redacted.as_str().contains("tool --token --label nightly"));
        assert!(redacted.as_str().contains("Bearer authentication failed"));
        assert!(redacted.as_str().contains("password=[REDACTED]"));
        assert!(redacted
            .as_str()
            .contains("Cookie: a=[REDACTED]; b=[REDACTED]"));
        assert_eq!(redacted.summary().count(CredentialKind::Cookie), 1);
        assert_eq!(redacted.summary().count(CredentialKind::Token), 0);
    }

    #[test]
    fn private_key_delimiters_keep_the_original_crlf_line_endings() {
        let input = b"before\r\n-----BEGIN RSA PRIVATE KEY-----\r\npem-sentinel-100014\r\n-----END RSA PRIVATE KEY-----\r\nafter\r\n";
        let redacted = scan_with_splits(input, &[17, 41, 72]);
        assert_eq!(
            redacted.as_str(),
            "before\r\n[CREDENTIAL_REMOVED:PRIVATE_KEY]\r\nafter\r\n"
        );
    }

    #[test]
    fn malformed_utf8_oversized_lines_and_unterminated_keys_fail_closed() {
        let mut invalid = b"prefix password=".to_vec();
        invalid.extend_from_slice(&[0xff, 0xfe]);
        invalid.extend_from_slice(b"secret-bytes\n");
        let invalid = scan_with_splits(&invalid, &[8, 17]);
        assert_eq!(invalid.as_str(), "[CREDENTIAL_REMOVED:HIGH_RISK]\n");

        let oversized_secret = "x".repeat(MAX_TRACE_REDACTION_CARRY_BYTES + 1);
        let oversized = scan_with_splits(
            format!("password={oversized_secret}\nafter\n").as_bytes(),
            &[32_768],
        );
        assert!(!oversized.as_str().contains(&oversized_secret));
        assert!(oversized
            .as_str()
            .starts_with("[CREDENTIAL_REMOVED:HIGH_RISK]\n"));

        let unterminated = scan_with_splits(
            b"before\n-----BEGIN OPENSSH PRIVATE KEY-----\nprivate-secret",
            &[13, 32],
        );
        assert_eq!(
            unterminated.as_str(),
            "before\n[CREDENTIAL_REMOVED:HIGH_RISK]"
        );
        assert_eq!(unterminated.summary().count(CredentialKind::HighRisk), 1);
    }

    #[test]
    fn exact_secret_storage_is_zeroized_and_debug_never_exposes_secret_material() {
        let secret = b"exact-debug-sentinel-441122";
        let mut secrets = ExactSecretSet::try_new([secret.as_slice()]).expect("valid secret set");
        let mut scanner = TraceCredentialScanner::with_exact_secrets(&secrets);
        assert!(!format!("{scanner:?}").contains("exact-debug-sentinel"));
        scanner.push(b"value=exact-debug-sentinel-441122");
        let mut redacted = scanner.finalize_text();
        assert!(!format!("{redacted:?}").contains("exact-debug-sentinel"));
        assert!(!redacted.as_str().contains("exact-debug-sentinel"));
        redacted.zeroize();
        assert!(redacted.as_str().is_empty());

        secrets.secrets[0].value.zeroize();
        assert!(secrets.secrets[0].value.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn sealed_finalization_is_idempotent_and_sentinel_reports_only_counts() {
        let mut reader = std::io::Cursor::new(b"token=token-sentinel-100007".as_slice());
        let session = TraceOutputSession::from_reader(
            TraceId::try_new_v7().expect("event id"),
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("bounded logical stream");
        let sentinel = trace_credential_sentinel(&session);
        assert_eq!(sentinel.redaction_count(), 1);
        assert!(!sentinel.high_risk());
        assert!(!format!("{sentinel:?}").contains("token-sentinel"));
        assert_eq!(session.redaction_summary.count(CredentialKind::Token), 1);

        let storage_size = mem::size_of::<RedactedTraceText>();
        assert!(storage_size > 0);
    }

    #[test]
    fn redacts_prefixed_http_headers_and_whole_private_key_blocks() {
        let input = concat!(
            "[http] Authorization: Basic basic-prefixed-secret\n",
            "json={\"line\":\"Authorization: Digest digest-prefixed-secret\"}\n",
            "[http] Cookie: session=cookie-prefixed-secret; mode=safe\n",
            "json={\"line\":\"Set-Cookie: sid=set-cookie-prefixed-secret; Secure\"}\n",
            "stderr token=pem-prefix-secret -----BEGIN PRIVATE KEY-----\n",
            "private-key-prefixed-secret\n",
            "-----END PRIVATE KEY----- token=pem-suffix-secret\n",
        );
        let redacted = scan_with_splits(input.as_bytes(), &[7, 55, 101, 167]);

        for secret in [
            "basic-prefixed-secret",
            "digest-prefixed-secret",
            "cookie-prefixed-secret",
            "set-cookie-prefixed-secret",
            "pem-prefix-secret",
            "pem-suffix-secret",
            "private-key-prefixed-secret",
        ] {
            assert!(
                !redacted.as_str().contains(secret),
                "leaked {secret}: {}",
                redacted.as_str()
            );
        }
        assert!(redacted
            .as_str()
            .contains("stderr token=[REDACTED] [CREDENTIAL_REMOVED:PRIVATE_KEY] token=[REDACTED]"));
        assert!(!redacted.as_str().contains("-----BEGIN PRIVATE KEY-----"));
        assert!(!redacted.as_str().contains("-----END PRIVATE KEY-----"));
    }

    #[test]
    fn redacts_json_quoted_credential_keys_without_leaking_values() {
        let redacted = scan_with_splits(
            br#"{"Authorization":"Basic json-basic-secret","Cookie":"sid=json-cookie-secret; mode=safe","Set-Cookie":"token=json-set-cookie-secret; Secure","message":"ok"}"#,
            &[17, 61, 109],
        );
        for secret in [
            "json-basic-secret",
            "json-cookie-secret",
            "json-set-cookie-secret",
        ] {
            assert!(!redacted.as_str().contains(secret), "leaked {secret}");
        }
        assert!(redacted.as_str().contains("[REDACTED]"));
        assert!(redacted.as_str().contains("\"message\":\"ok\""));
    }

    #[test]
    fn quoted_json_headers_remain_valid_after_sealed_event_serialization() {
        let raw_output = br#"{"Authorization":"Basic json-basic-secret","Cookie":"sid=json-cookie-secret; mode=safe","Set-Cookie":"token=json-set-cookie-secret; Secure","message":"ok"}"#;
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let mut reader = io::Cursor::new(raw_output.to_vec());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("scanner reaches EOF before sealing")
        .into_event_upload_attempts(
            TraceEventText {
                event_id,
                run_id,
                sequence: 1,
                kind: TraceEventKindV2::Command,
                step_name: "quoted-json-output",
                partition_name: None,
                status: TraceEventStatusV2::Success,
                started_at_ms: 1,
                ended_at_ms: Some(2),
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
        .expect("sealed event upload");

        assert_eq!(attempts.len(), 1);
        let body = attempts[0].to_json_body().expect("sealed JSON body");
        let body_text = std::str::from_utf8(&body).expect("UTF-8 JSON body");
        for secret in [
            "json-basic-secret",
            "json-cookie-secret",
            "json-set-cookie-secret",
        ] {
            assert!(!body_text.contains(secret), "sealed body leaked {secret}");
        }

        let outer: serde_json::Value =
            serde_json::from_slice(&body).expect("outer JSON remains valid");
        let output_text = outer["output_chunks"][0]["text"]
            .as_str()
            .expect("sealed output text");
        let inner: serde_json::Value =
            serde_json::from_str(output_text).expect("redacted JSON output remains valid");
        assert_eq!(inner["Authorization"], "Basic [REDACTED]");
        assert_eq!(inner["Cookie"], "sid=[REDACTED]; mode=****");
        assert_eq!(inner["Set-Cookie"], "token=[REDACTED]; Secure");
        assert_eq!(inner["message"], "ok");
    }

    #[test]
    fn removes_entire_url_userinfo_including_username() {
        let redacted = scan_with_splits(
            b"request=https://username-secret:password-secret@example.test/path\n",
            &[],
        );
        assert!(!redacted.as_str().contains("username-secret"));
        assert!(!redacted.as_str().contains("password-secret"));
        assert!(redacted
            .as_str()
            .contains("https://[REDACTED]@example.test/path"));
    }

    #[test]
    fn exact_secret_registration_is_bounded_and_markers_are_stable() {
        assert!(matches!(
            ExactSecretSet::try_new([b"short".as_slice()]),
            Err(TraceRedactionError::InvalidExactSecret)
        ));
        assert!(matches!(
            ExactSecretSet::try_new([b"DACTED".as_slice()]),
            Err(TraceRedactionError::InvalidExactSecret)
        ));
        let secrets: Vec<_> = (0..MAX_TRACE_EXACT_SECRETS)
            .map(|index| format!("secret-{index:02}-value"))
            .collect();
        let set = ExactSecretSet::try_new(secrets.iter().map(String::as_bytes))
            .expect("bounded secret set");
        assert_eq!(set.len(), MAX_TRACE_EXACT_SECRETS);
        let mut over_count = secrets;
        over_count.push("secret-over-limit".to_string());
        assert!(matches!(
            ExactSecretSet::try_new(over_count.iter().map(String::as_bytes)),
            Err(TraceRedactionError::InvalidExactSecret)
        ));
        assert!(matches!(
            ExactSecretSet::try_new([vec![b'x'; MAX_TRACE_EXACT_SECRET_BYTES + 1]]),
            Err(TraceRedactionError::InvalidExactSecret)
        ));
        for secret in [b"line-one\nline-two".as_slice(), b"line-one\rline-two"] {
            assert!(matches!(
                ExactSecretSet::try_new([secret]),
                Err(TraceRedactionError::InvalidExactSecret)
            ));
        }
    }

    #[test]
    fn private_key_decoy_does_not_hide_a_later_real_begin_marker() {
        let redacted = scan_with_splits(
            b"INFO -----BEGIN PUBLIC KEY----- decoy -----BEGIN PRIVATE KEY-----\nprivate-key-secret\n-----END PRIVATE KEY-----\n",
            &[11, 43, 68],
        );
        assert!(!redacted.as_str().contains("private-key-secret"));
        assert!(!redacted.as_str().contains("-----BEGIN PRIVATE KEY-----"));
        assert_eq!(redacted.summary().count(CredentialKind::PrivateKey), 1);
    }

    #[test]
    fn assignment_hash_suffix_is_part_of_the_credential_value() {
        let redacted = scan_with_splits(
            b"password=abc#def --token opaque#suffix next=value\n",
            &[7, 19, 31],
        );
        for secret in ["abc", "def", "opaque", "suffix"] {
            assert!(!redacted.as_str().contains(secret), "leaked {secret}");
        }
        assert!(redacted.as_str().contains("next=value"));
    }

    #[test]
    fn output_above_two_hundred_chunks_fails_without_a_logical_stream_proof() {
        let mut scanner = TraceCredentialScanner::new();
        scanner.push(
            "x\n"
                .repeat(MAX_TRACE_REDACTED_OUTPUT_BYTES / 2 + 1)
                .as_bytes(),
        );
        assert!(matches!(
            scanner.finish(),
            Err(TraceRedactionError::OutputTooLarge)
        ));
    }

    #[test]
    fn second_scan_is_byte_for_byte_idempotent() {
        let first = scan_with_splits(
            b"password=abc\ntoken=long-token-secret\nhttps://user:pass@example.test\n",
            &[],
        );
        let second = scan_with_splits(first.as_str().as_bytes(), &[3, 17]);
        assert_eq!(second.as_str(), first.as_str());
        assert_eq!(second.summary().total(), 0);
    }

    #[test]
    fn structured_command_redaction_covers_cross_argument_secrets_and_all_fields() {
        let command = TraceCommandText {
            program: "runner?token=program-secret",
            argv: &["--api-key", "argv-secret", "--mode", "safe"],
            display_command: "runner --password display-secret",
            working_directory: Some("C:/token=working-secret"),
            paths: &["C:/password=path-secret"],
            urls: &["https://url-user:url-password@example.test"],
            serial: Some("token=serial-secret"),
        };
        let redacted = redact_trace_command(&command, &ExactSecretSet::empty())
            .expect("command redaction succeeds");
        let debug = format!("{redacted:?}");
        for secret in [
            "program-secret",
            "argv-secret",
            "display-secret",
            "working-secret",
            "path-secret",
            "url-user",
            "url-password",
            "serial-secret",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}");
            assert!(
                !command_contains(&redacted, secret),
                "redacted command leaked {secret}"
            );
        }
        assert_eq!(redacted.argv()[0], "--api-key");
        assert_eq!(redacted.argv()[1], "[REDACTED]");
    }

    #[test]
    fn consecutive_secret_flags_bind_only_the_final_value() {
        let command = TraceCommandText {
            program: "runner",
            argv: &["--token", "--api-key", "real-secret-value"],
            display_command: "runner",
            working_directory: None,
            paths: &[],
            urls: &[],
            serial: None,
        };
        let redacted = redact_trace_command(&command, &ExactSecretSet::empty())
            .expect("command redaction succeeds");
        assert_eq!(redacted.argv()[0], "--token");
        assert_eq!(redacted.argv()[1], "--api-key");
        assert_eq!(redacted.argv()[2], "[REDACTED]");
        assert!(!command_contains(&redacted, "real-secret-value"));
    }

    #[test]
    fn every_cookie_occurrence_and_dash_prefixed_credential_value_is_redacted() {
        let cookies = scan_with_splits(
            b"[a] Set-Cookie: a=first-cookie-secret; Secure | [b] Set-Cookie: b=second-cookie-secret; HttpOnly\n",
            &[9, 47],
        );
        assert!(!cookies.as_str().contains("first-cookie-secret"));
        assert!(!cookies.as_str().contains("second-cookie-secret"));
        assert_eq!(cookies.summary().count(CredentialKind::Cookie), 2);

        let command = TraceCommandText {
            program: "runner",
            argv: &["--token", "-opaque-secret", "--label", "nightly"],
            display_command: "runner --token -display-secret",
            working_directory: None,
            paths: &[],
            urls: &[],
            serial: None,
        };
        let redacted =
            redact_trace_command(&command, &ExactSecretSet::empty()).expect("bounded command");
        assert_eq!(redacted.argv()[1], "[REDACTED]");
        assert_eq!(redacted.argv()[2], "--label");
        assert_eq!(redacted.argv()[3], "nightly");
        assert!(!redacted.display_command().contains("display-secret"));
    }

    #[test]
    fn high_risk_scanner_state_cannot_be_finalized_into_a_sealed_stream() {
        let mut scanner = TraceCredentialScanner::new();
        scanner.push(&[0xff, 0xfe]);
        assert!(matches!(
            scanner.finish(),
            Err(TraceRedactionError::HighRisk)
        ));
    }

    #[test]
    fn chunking_is_utf8_safe_exact_and_bounded() {
        let mut scanner = TraceCredentialScanner::new();
        scanner.push(format!("{}🚀", "x".repeat(32_767)).as_bytes());
        let chunks = scanner
            .finish()
            .expect("safe logical stream")
            .into_output_chunks(
                TraceId::try_new_v7().expect("UUIDv7"),
                TraceOutputStreamV2::Stdout,
                0,
            )
            .expect("UTF-8 chunks");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].byte_count(), 32_767);
        assert_eq!(chunks[1].text(), "🚀");
        assert_eq!(
            chunks
                .iter()
                .map(RedactedOutputChunk::text)
                .collect::<String>(),
            format!("{}🚀", "x".repeat(32_767))
        );

        let mut exact = TraceCredentialScanner::new();
        exact.push("x".repeat(32_768).as_bytes());
        let exact = exact
            .finish()
            .expect("exact chunk")
            .into_output_chunks(
                TraceId::try_new_v7().expect("UUIDv7"),
                TraceOutputStreamV2::Stdout,
                0,
            )
            .expect("one exact chunk");
        assert_eq!(exact.len(), 1);
    }

    #[test]
    fn output_session_reads_a_complete_stream_before_it_creates_chunks() {
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let mut reader = io::Cursor::new(b"before bearer session-eof-secret after".to_vec());
        let session = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete reader is sealed before chunking");

        let attempts = session
            .into_upload_attempts()
            .expect("bounded attempts after EOF");
        assert_eq!(attempts.len(), 1);
        let body = attempts[0].to_json_body().expect("sealed JSON");
        assert!(!std::str::from_utf8(&body)
            .expect("UTF-8 JSON")
            .contains("session-eof-secret"));
    }

    #[test]
    fn output_session_stops_reading_after_redacted_output_saturates() {
        let mut reader = BudgetedInfiniteReader {
            read_bytes: 0,
            error_after: MAX_TRACE_REDACTED_OUTPUT_BYTES + 16 * 1024,
        };
        let result = TraceOutputSession::from_reader(
            TraceId::try_new_v7().expect("UUIDv7"),
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        );

        assert!(matches!(result, Err(TraceRedactionError::OutputTooLarge)));
        assert!(reader.read_bytes <= MAX_TRACE_REDACTED_OUTPUT_BYTES + 8 * 1024);
    }

    #[test]
    fn output_session_accepts_a_bounded_complete_reader() {
        let input = format!("{}\n", "safe".repeat(1024)).repeat(4);
        let mut reader = io::Cursor::new(input.into_bytes());
        let session = TraceOutputSession::from_reader(
            TraceId::try_new_v7().expect("UUIDv7"),
            TraceOutputStreamV2::Stderr,
            &mut reader,
            &ExactSecretSet::empty(),
        );
        assert!(session.is_ok());
    }

    #[test]
    fn output_session_partitions_large_streams_into_fresh_bounded_attempts() {
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let input = format!("{}\n", "x".repeat(TRACE_OUTPUT_MAX_BYTES - 1)).repeat(40);
        let mut reader = io::Cursor::new(input.into_bytes());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stderr,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete logical stream")
        .into_upload_attempts()
        .expect("bounded attempts");

        assert!(attempts.len() > 1);
        let first_upload_id = attempts[0].upload_id();
        assert!(attempts
            .iter()
            .skip(1)
            .all(|attempt| attempt.upload_id() != first_upload_id));
        let chunks: Vec<_> = attempts
            .iter()
            .flat_map(SealedTraceUpload::output_chunks)
            .collect();
        assert_eq!(chunks.len(), 40);
        assert!(chunks.iter().enumerate().all(|(index, chunk)| {
            chunk.event_id() == event_id
                && chunk.stream() == TraceOutputStreamV2::Stderr
                && chunk.chunk_index() == index as u64
        }));
        for attempt in attempts {
            assert!(
                attempt.to_json_body().expect("bounded JSON").len() <= TRACE_UPLOAD_MAX_BODY_BYTES
            );
        }
    }

    #[test]
    fn output_session_sizes_attempts_from_escaped_wire_bytes_not_fixed_chunk_counts() {
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        // Newlines are legal output but double in JSON. Twenty full chunks no
        // longer fit below the HTTP limit, so packing must use the actual body.
        let input = format!("{}\n", "\t".repeat(TRACE_OUTPUT_MAX_BYTES - 1)).repeat(20);
        let mut reader = io::Cursor::new(input.into_bytes());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete stream")
        .into_upload_attempts()
        .expect("escape-aware attempts");

        assert!(attempts.len() > 1);
        assert_eq!(
            attempts
                .iter()
                .flat_map(SealedTraceUpload::output_chunks)
                .count(),
            20
        );
        for attempt in attempts {
            assert!(
                attempt.to_json_body().expect("bounded JSON").len() <= TRACE_UPLOAD_MAX_BODY_BYTES
            );
        }
    }

    #[test]
    fn output_session_puts_its_event_manifest_only_in_the_first_attempt() {
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let input = format!("{}\n", "\t".repeat(TRACE_OUTPUT_MAX_BYTES - 1)).repeat(21);
        let mut reader = io::Cursor::new(input.into_bytes());
        let attempts = TraceOutputSession::from_reader(
            event_id,
            TraceOutputStreamV2::Stdout,
            &mut reader,
            &ExactSecretSet::empty(),
        )
        .expect("complete stream")
        .into_event_upload_attempts(
            TraceEventText {
                event_id,
                run_id,
                sequence: 1,
                kind: TraceEventKindV2::Command,
                step_name: "capture",
                partition_name: None,
                status: TraceEventStatusV2::Success,
                started_at_ms: 1,
                ended_at_ms: Some(2),
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
        .expect("event attempts");

        assert!(attempts.len() > 1);
        for (index, attempt) in attempts.iter().enumerate() {
            assert!(attempts
                .iter()
                .skip(index + 1)
                .all(|other| other.upload_id() != attempt.upload_id()));
        }
        assert_eq!(
            attempts
                .iter()
                .flat_map(SealedTraceUpload::output_chunks)
                .count(),
            21
        );
        let first_body = attempts[0].to_json_body().expect("first body");
        let later_body = attempts[1].to_json_body().expect("later body");
        let first = std::str::from_utf8(&first_body).expect("UTF-8");
        let later = std::str::from_utf8(&later_body).expect("UTF-8");
        assert!(first.contains("\"events\":[{"));
        assert!(later.contains("\"events\":[]"));
        assert!(first.contains(&event_id.to_string()));
        assert!(later.contains(&event_id.to_string()));
        for attempt in attempts {
            assert!(
                attempt.to_json_body().expect("bounded JSON").len() <= TRACE_UPLOAD_MAX_BODY_BYTES
            );
        }
    }

    #[test]
    fn oversized_metadata_and_http_body_fail_before_external_sink_write() {
        let argv: Vec<String> = (0..65).map(|_| "x".repeat(16_384)).collect();
        let argv_refs: Vec<_> = argv.iter().map(String::as_str).collect();
        let event = TraceEventText {
            event_id: TraceId::try_new_v7().expect("UUIDv7"),
            run_id: TraceId::try_new_v7().expect("UUIDv7"),
            sequence: 1,
            kind: TraceEventKindV2::Command,
            step_name: "oversized",
            partition_name: None,
            status: TraceEventStatusV2::Started,
            started_at_ms: 1,
            ended_at_ms: None,
            duration_ms: None,
            command: Some(TraceCommandText {
                program: "runner",
                argv: &argv_refs,
                display_command: "runner",
                working_directory: None,
                paths: &[],
                urls: &[],
                serial: None,
            }),
            exit_code: None,
            verification: None,
            device_state: None,
            retry_safe: None,
            remedies: &[],
            error_class: None,
            error_code: None,
            error_message: None,
        };
        assert!(matches!(
            RedactedTraceEvent::try_new(event, &ExactSecretSet::empty(), None, None),
            Err(TraceRedactionError::MetadataTooLarge)
        ));

        let mut scanner = TraceCredentialScanner::new();
        scanner.push("safe-line\n".repeat(100_000).as_bytes());
        let chunks = scanner
            .finish()
            .expect("multi-request logical stream")
            .into_output_chunks(
                TraceId::try_new_v7().expect("UUIDv7"),
                TraceOutputStreamV2::Stdout,
                0,
            )
            .expect("bounded chunks");
        let upload =
            SealedTraceUpload::from_output_chunks(TraceId::try_new_v7().expect("UUIDv7"), chunks)
                .expect("sealed chunks");
        let mut sink = Vec::new();
        assert!(matches!(
            upload.write_json(&mut sink),
            Err(TraceRedactionError::RequestTooLarge)
        ));
        assert!(sink.is_empty());
    }

    #[test]
    fn sealed_metadata_and_complete_streams_are_the_only_full_upload_path() {
        let secrets =
            ExactSecretSet::try_new([b"exact-upload-secret".as_slice()]).expect("valid secret set");
        let run_id = TraceId::try_new_v7().expect("UUIDv7");
        let event_id = TraceId::try_new_v7().expect("UUIDv7");
        let source_paths = ["C:/token=wire-secret"];
        let source_urls = ["https://user:wire-secret@example.test"];
        let run = RedactedTraceRun::try_new(
            TraceRunText {
                run_id,
                operation_kind: "token=wire-secret",
                title: "token=wire-secret",
                outcome: nwflash_domain::TraceOutcomeV2::Failed,
                device_serial: Some("token=wire-secret"),
                source_paths: &source_paths,
                source_urls: &source_urls,
                client_version: "token=wire-secret",
                started_at_ms: 1,
                ended_at_ms: Some(2),
                duration_ms: Some(1),
                error_class: Some("token=wire-secret"),
                error_code: Some("token=wire-secret"),
                error_message: Some("token=wire-secret"),
                final_sequence: Some(1),
                trace_complete: true,
                trace_loss_reason: None,
            },
            &secrets,
        )
        .expect("sealed run");
        let mut stdout = TraceCredentialScanner::with_exact_secrets(&secrets);
        stdout.push(b"Authorization: Bearer stream-upload-secret\n");
        let stdout = stdout.finish().expect("sealed stream");
        let argv = ["--token", "wire-secret"];
        let command_paths = ["C:/token=wire-secret"];
        let command_urls = ["https://user:wire-secret@example.test"];
        let remedies = ["token=wire-secret"];
        let event = RedactedTraceEvent::try_new(
            TraceEventText {
                event_id,
                run_id,
                sequence: 1,
                kind: nwflash_domain::TraceEventKindV2::Command,
                step_name: "token=wire-secret",
                partition_name: Some("token=wire-secret"),
                status: nwflash_domain::TraceEventStatusV2::Failed,
                started_at_ms: 1,
                ended_at_ms: Some(2),
                duration_ms: Some(1),
                command: Some(TraceCommandText {
                    program: "token=wire-secret",
                    argv: &argv,
                    display_command: "runner --token wire-secret",
                    working_directory: Some("C:/token=wire-secret"),
                    paths: &command_paths,
                    urls: &command_urls,
                    serial: Some("token=wire-secret"),
                }),
                exit_code: Some(1),
                verification: Some("token=wire-secret"),
                device_state: Some("token=wire-secret"),
                retry_safe: Some(true),
                remedies: &remedies,
                error_class: Some("token=wire-secret"),
                error_code: Some("token=wire-secret"),
                error_message: Some("token=wire-secret"),
            },
            &secrets,
            Some(stdout),
            None,
        )
        .expect("sealed event");
        assert_eq!(event.stdout_chunks(), 1);
        assert_eq!(event.stderr_chunks(), 0);
        assert!(event.redaction_summary().total() >= 10);

        let upload = SealedTraceUpload::new(
            TraceId::try_new_v7().expect("UUIDv7"),
            vec![run],
            vec![event],
        )
        .expect("sealed upload");
        let body = upload.to_json_body().expect("bounded body");
        let body = std::str::from_utf8(&body).expect("JSON UTF-8");
        for secret in ["wire-secret", "stream-upload-secret", "exact-upload-secret"] {
            assert!(!body.contains(secret), "sealed body leaked {secret}");
        }
        assert!(body.contains("\"runs\":[{"));
        assert!(body.contains("\"events\":[{"));
        assert!(body.contains("\"output_chunks\":[{"));
    }
}
