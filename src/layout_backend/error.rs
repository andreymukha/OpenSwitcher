use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutBackendOperation {
    DetectSetup,
    CurrentLayoutSnapshot,
    SwitchTo,
    SwitchNext,
    StartMonitoring,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("Invalid normalized layout code: {value}")]
pub struct LayoutCodeNormalizationError {
    pub value: String,
}

#[derive(Debug, Error)]
pub enum LayoutBackendError {
    #[error("Operation is not supported by backend `{backend}`: {operation:?}")]
    UnsupportedOperation {
        backend: &'static str,
        operation: LayoutBackendOperation,
    },

    #[error("Backend `{backend}` failed during `{operation:?}`")]
    RuntimeFailure {
        backend: &'static str,
        operation: LayoutBackendOperation,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl LayoutBackendError {
    pub fn unsupported(backend: &'static str, operation: LayoutBackendOperation) -> Self {
        Self::UnsupportedOperation { backend, operation }
    }

    pub fn runtime<E>(backend: &'static str, operation: LayoutBackendOperation, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::RuntimeFailure {
            backend,
            operation,
            source: Box::new(source),
        }
    }
}
