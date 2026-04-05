#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackendCapabilities {
    pub can_list_layouts: bool,
    pub can_read_current_layout: bool,
    pub can_switch_to_target: bool,
    pub can_switch_next: bool,
    pub can_observe_layout_changes: bool,
    pub can_map_layouts_to_app_kinds: bool,
}
