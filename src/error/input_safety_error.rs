use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum InputSafetyError {
    #[error("Synthetic input safety invariant failed: {context}")]
    Invariant { context: &'static str },
    #[error("Synthetic input operation {operation_id} violated its protocol: {context}")]
    ProtocolViolation {
        operation_id: u64,
        context: &'static str,
    },
    #[error("Synthetic modifier generation {generation} violated its protocol: {context}")]
    SessionModifierProtocolViolation {
        generation: u64,
        context: &'static str,
    },
    #[error("Synthetic input operation {operation_id} was cancelled")]
    OperationCancelled { operation_id: u64 },
    #[error("Synthetic input operation {operation_id} exceeded its deadline")]
    OperationTimedOut { operation_id: u64 },
    #[error(
        "Synthetic input operation {operation_id} could not be reconciled ({remaining} pending)"
    )]
    Reconciliation { operation_id: u64, remaining: usize },
    #[error("XTEST guardian is unavailable: {context}")]
    GuardianUnavailable { context: &'static str },
    #[error("XTEST guardian request for operation {operation_id} exceeded its local deadline")]
    GuardianRequestTimedOut { operation_id: u64 },
    #[error("XTEST guardian rejected the session: {context}")]
    GuardianProtocol { context: &'static str },
    #[error("XTEST guardian emergency cleanup exceeded its deadline ({remaining} pending)")]
    GuardianEmergencyTimedOut { remaining: usize },
    #[error("XTEST guardian frame size {actual} exceeds the limit {maximum}")]
    OversizedFrame { actual: usize, maximum: usize },
}
