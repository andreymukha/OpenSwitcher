mod backend;
mod backends;
mod capabilities;
mod compat;
mod detection;
mod error;
mod model;
mod policy;
mod registry;

pub use backend::{LayoutBackend, LayoutStateSink};
pub use backends::legacy_backend_factory;
pub use capabilities::BackendCapabilities;
pub use compat::{
    legacy_current_layout_bool, legacy_layout_state_from_bool, LEGACY_LAYOUT_FALLBACK_IS_ENGLISH,
};
pub(crate) use detection::detect_gnome_setup_from_sources;
pub use detection::{
    current_layout_from_gnome_sources, current_layout_from_group, detect_layout_setup,
    LayoutSetupDetection,
};
pub use error::{LayoutBackendError, LayoutBackendOperation, LayoutCodeNormalizationError};
pub use model::{
    AppLayoutKind, CurrentLayoutState, LayoutCode, LayoutCompatibility, LayoutSetup,
    NormalizedLayoutCode, SystemLayout,
};
pub use policy::{compatibility_from_setup, feature_availability_for, FeatureAvailability};
pub use registry::{BackendFactory, LayoutBackendRegistry, LayoutBackendRegistryResult};
