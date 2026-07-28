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
    #[error("Synthetic input operation {operation_id} was cancelled")]
    OperationCancelled { operation_id: u64 },
    #[error("Synthetic input operation {operation_id} exceeded its deadline")]
    OperationTimedOut { operation_id: u64 },
    #[error(
        "Synthetic input operation {operation_id} could not be reconciled ({remaining} pending)"
    )]
    Reconciliation { operation_id: u64, remaining: usize },
}
