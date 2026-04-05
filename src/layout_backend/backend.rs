use super::{
    BackendCapabilities, CurrentLayoutState, LayoutBackendError, LayoutSetup, SystemLayout,
};
use std::sync::mpsc;

pub type LayoutStateSink = mpsc::Sender<CurrentLayoutState>;

pub trait LayoutBackend: Send {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> BackendCapabilities;

    fn detect_setup(&self) -> Result<LayoutSetup, LayoutBackendError>;
    fn current_layout_snapshot(&self) -> Result<CurrentLayoutState, LayoutBackendError>;

    fn switch_to(&mut self, target: &SystemLayout) -> Result<(), LayoutBackendError>;
    fn switch_next(&mut self) -> Result<(), LayoutBackendError>;

    fn start_monitoring(&mut self, sink: LayoutStateSink) -> Result<(), LayoutBackendError>;
}
