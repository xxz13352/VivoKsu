/// A closed selector set for the protection dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ProtectionSelector {
    Login = 1,
    Heartbeat = 2,
    LocalOperation = 3,
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
#[inline(never)]
#[export_name = "nwflash_protection_dispatch_decision"]
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
