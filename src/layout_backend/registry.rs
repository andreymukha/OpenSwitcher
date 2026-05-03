use super::{LayoutBackend, LayoutBackendError};

pub type BackendFactory = fn() -> Result<Box<dyn LayoutBackend>, LayoutBackendError>;

pub enum LayoutBackendRegistryResult {
    Backend(Box<dyn LayoutBackend>),
    Unsupported { reason: String },
}

#[derive(Default)]
pub struct LayoutBackendRegistry {
    factories: Vec<BackendFactory>,
}

impl LayoutBackendRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_factory(&mut self, factory: BackendFactory) {
        self.factories.push(factory);
    }

    pub fn pick_backend(&self) -> LayoutBackendRegistryResult {
        for factory in &self.factories {
            match factory() {
                Ok(backend) => return LayoutBackendRegistryResult::Backend(backend),
                Err(LayoutBackendError::UnsupportedOperation { .. }) => continue,
                Err(error) => {
                    return LayoutBackendRegistryResult::Unsupported {
                        reason: error.to_string(),
                    };
                }
            }
        }

        LayoutBackendRegistryResult::Unsupported {
            reason: "No compatible layout backend is registered.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout_backend::{
        BackendCapabilities, CurrentLayoutState, LayoutBackendOperation, LayoutSetup,
    };
    use std::io;

    struct TestBackend;

    impl LayoutBackend for TestBackend {
        fn id(&self) -> &'static str {
            "test"
        }

        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }

        fn detect_setup(&self) -> Result<LayoutSetup, LayoutBackendError> {
            unreachable!()
        }

        fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError> {
            unreachable!()
        }

        fn switch_to(
            &mut self,
            _target: &crate::layout_backend::SystemLayout,
        ) -> Result<(), LayoutBackendError> {
            unreachable!()
        }

        fn switch_next(&mut self) -> Result<(), LayoutBackendError> {
            unreachable!()
        }

        fn start_monitoring(
            &mut self,
            _sink: crate::layout_backend::LayoutStateSink,
        ) -> Result<(), LayoutBackendError> {
            unreachable!()
        }
    }

    fn unsupported_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
        Err(LayoutBackendError::unsupported(
            "unsupported",
            LayoutBackendOperation::DetectSetup,
        ))
    }

    fn working_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
        Ok(Box::new(TestBackend))
    }

    fn runtime_error_factory() -> Result<Box<dyn LayoutBackend>, LayoutBackendError> {
        Err(LayoutBackendError::runtime(
            "broken",
            LayoutBackendOperation::DetectSetup,
            io::Error::other("boom"),
        ))
    }

    #[test]
    fn registry_skips_unsupported_factories_and_picks_first_working_backend() {
        let mut registry = LayoutBackendRegistry::new();
        registry.register_factory(unsupported_factory);
        registry.register_factory(working_factory);

        let result = registry.pick_backend();

        match result {
            LayoutBackendRegistryResult::Backend(backend) => assert_eq!(backend.id(), "test"),
            LayoutBackendRegistryResult::Unsupported { reason } => {
                panic!("expected backend, got unsupported: {reason}")
            }
        }
    }

    #[test]
    fn registry_stops_on_runtime_error_and_reports_reason() {
        let mut registry = LayoutBackendRegistry::new();
        registry.register_factory(runtime_error_factory);
        registry.register_factory(working_factory);

        let result = registry.pick_backend();

        match result {
            LayoutBackendRegistryResult::Unsupported { reason } => {
                assert!(reason.contains("broken"));
                assert!(reason.contains("DetectSetup"));
            }
            LayoutBackendRegistryResult::Backend(backend) => {
                panic!("expected runtime error, got backend: {}", backend.id())
            }
        }
    }
}
