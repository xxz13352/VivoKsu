use crate::{begin_operation_admission, end_marker};

/// A closed selector set for the protection dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ProtectionSelector {
    Login = 1,
    Heartbeat = 2,
    LocalOperation = 3,
}

/// The closed wire index for the operation classification consulted by the
/// runtime permission gates. Domain-level `OperationKind` values must map to
/// this stable discriminant table before any high-risk recheck decision, so a
/// patched-out local classification cannot silently route flash/write paths
/// past the admission leaf. Values are assigned to match
/// `nwflash_domain::OperationKind` declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ProtectedOperationKind {
    Idle = 0,
    Discovering = 1,
    Rebooting = 2,
    Installing = 3,
    Transferring = 4,
    Hashing = 5,
    Flashing = 6,
    Mirroring = 7,
    Completed = 8,
    Canceled = 9,
    Failed = 10,
}

impl ProtectedOperationKind {
    /// Maps a domain operation kind onto the closed classification table by
    /// exhaustive structural match. Adding a domain variant without extending
    /// the protected wire table is a compile error here, so the protected
    /// classification can never fall out of sync with the domain enum.
    pub const fn from_domain(kind: nwflash_domain::OperationKind) -> Option<Self> {
        match kind {
            nwflash_domain::OperationKind::Idle => Some(Self::Idle),
            nwflash_domain::OperationKind::Discovering => Some(Self::Discovering),
            nwflash_domain::OperationKind::Rebooting => Some(Self::Rebooting),
            nwflash_domain::OperationKind::Installing => Some(Self::Installing),
            nwflash_domain::OperationKind::Transferring => Some(Self::Transferring),
            nwflash_domain::OperationKind::Hashing => Some(Self::Hashing),
            nwflash_domain::OperationKind::Flashing => Some(Self::Flashing),
            nwflash_domain::OperationKind::Mirroring => Some(Self::Mirroring),
            nwflash_domain::OperationKind::Completed => Some(Self::Completed),
            nwflash_domain::OperationKind::Canceled => Some(Self::Canceled),
            nwflash_domain::OperationKind::Failed => Some(Self::Failed),
        }
    }

    /// Decodes a raw wire index back onto the closed table. The index is a
    /// stable discriminant, not a trust anchor: unknown indices must be
    /// treated as high-risk (fail-closed) by every consumer.
    pub const fn from_wire_index(index: u32) -> Option<Self> {
        match index {
            index if index == Self::Idle as u32 => Some(Self::Idle),
            index if index == Self::Discovering as u32 => Some(Self::Discovering),
            index if index == Self::Rebooting as u32 => Some(Self::Rebooting),
            index if index == Self::Installing as u32 => Some(Self::Installing),
            index if index == Self::Transferring as u32 => Some(Self::Transferring),
            index if index == Self::Hashing as u32 => Some(Self::Hashing),
            index if index == Self::Flashing as u32 => Some(Self::Flashing),
            index if index == Self::Mirroring as u32 => Some(Self::Mirroring),
            index if index == Self::Completed as u32 => Some(Self::Completed),
            index if index == Self::Canceled as u32 => Some(Self::Canceled),
            index if index == Self::Failed as u32 => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Fail-closed outcome of a protected operation admission check. `Deny` never
/// carries a reason: the dispatch site must deny unconditionally, not format
/// or branch on an attacker-controlled message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedAdmission {
    Allow,
    Deny,
}

/// The protected operation classification consulted at dispatch: answers
/// whether an operation kind must pass through the protected local admission
/// path (signed-lease recheck) before running. The high-risk table lives only
/// inside this leaf, so the unprotected dispatcher cannot be one-line patched
/// into routing flash/write paths around the admission leaf. Unknown wire
/// indices are treated as high-risk (fail-closed).
///
/// See `decision_matrix.rs` for the protocol-level fixtures.
#[inline(never)]
#[export_name = "nwflash_protection_requires_protected_recheck"]
pub fn requires_protected_recheck(wire_index: u32) -> bool {
    begin_operation_admission();
    let required = match ProtectedOperationKind::from_wire_index(wire_index) {
        Some(
            ProtectedOperationKind::Rebooting
            | ProtectedOperationKind::Installing
            | ProtectedOperationKind::Transferring
            | ProtectedOperationKind::Flashing
            | ProtectedOperationKind::Mirroring,
        ) => true,
        Some(
            ProtectedOperationKind::Idle
            | ProtectedOperationKind::Discovering
            | ProtectedOperationKind::Hashing
            | ProtectedOperationKind::Completed
            | ProtectedOperationKind::Canceled
            | ProtectedOperationKind::Failed,
        ) => false,
        None => true,
    };
    end_marker();
    required
}

const SELECTOR_MASK: u32 = 0x6e57_f1a5;

/// Encodes a selector so callers do not pass raw branch indices.
pub const fn encoded_selector(selector: ProtectionSelector) -> u32 {
    (selector as u32) ^ SELECTOR_MASK
}

/// Normalized inputs only; credentials and bearer tokens are deliberately absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionInput {
    Login {
        signature_valid: bool,
        claims_bound: bool,
    },
    Heartbeat {
        signature_valid: bool,
        claims_bound: bool,
        sequence_advanced: bool,
    },
    LocalOperation {
        session_active: bool,
        lease_current: bool,
        build_id_matches: bool,
        process_nonce_matches: bool,
        sequence_current: bool,
    },
}

/// Fail-closed outcomes exposed by the decision dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionDecision {
    Allow,
    Deny(ProtectionFailure),
}

/// The finite reasons a protection decision is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectionFailure {
    IllegalSelector,
    InvalidInput,
    InvalidLease,
    BindingMismatch,
    SequenceRollback,
    SessionInactive,
    LeaseExpired,
    BuildIdentityMismatch,
    ProcessNonceMismatch,
    SequenceMismatch,
}

/// Dispatches a normalized protection decision and denies malformed routes.
/// This is a pure protocol utility, not an additional VMP release leaf.
pub fn dispatch_protection_decision(selector: u32, input: DecisionInput) -> ProtectionDecision {
    match (decode_selector(selector), input) {
        (
            Some(ProtectionSelector::Login),
            DecisionInput::Login {
                signature_valid: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::InvalidLease),
        (
            Some(ProtectionSelector::Login),
            DecisionInput::Login {
                claims_bound: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::BindingMismatch),
        (Some(ProtectionSelector::Login), DecisionInput::Login { .. }) => ProtectionDecision::Allow,

        (
            Some(ProtectionSelector::Heartbeat),
            DecisionInput::Heartbeat {
                signature_valid: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::InvalidLease),
        (
            Some(ProtectionSelector::Heartbeat),
            DecisionInput::Heartbeat {
                claims_bound: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::BindingMismatch),
        (
            Some(ProtectionSelector::Heartbeat),
            DecisionInput::Heartbeat {
                sequence_advanced: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::SequenceRollback),
        (Some(ProtectionSelector::Heartbeat), DecisionInput::Heartbeat { .. }) => {
            ProtectionDecision::Allow
        }

        (
            Some(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                session_active: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::SessionInactive),
        (
            Some(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                lease_current: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::LeaseExpired),
        (
            Some(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                build_id_matches: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::BuildIdentityMismatch),
        (
            Some(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                process_nonce_matches: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::ProcessNonceMismatch),
        (
            Some(ProtectionSelector::LocalOperation),
            DecisionInput::LocalOperation {
                sequence_current: false,
                ..
            },
        ) => ProtectionDecision::Deny(ProtectionFailure::SequenceMismatch),
        (Some(ProtectionSelector::LocalOperation), DecisionInput::LocalOperation { .. }) => {
            ProtectionDecision::Allow
        }

        (None, _) => ProtectionDecision::Deny(ProtectionFailure::IllegalSelector),
        (Some(_), _) => ProtectionDecision::Deny(ProtectionFailure::InvalidInput),
    }
}

fn decode_selector(encoded: u32) -> Option<ProtectionSelector> {
    match encoded ^ SELECTOR_MASK {
        1 => Some(ProtectionSelector::Login),
        2 => Some(ProtectionSelector::Heartbeat),
        3 => Some(ProtectionSelector::LocalOperation),
        _ => None,
    }
}
