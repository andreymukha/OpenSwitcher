use crate::dbus::OpenSwitcherProxyBlocking;
use crate::error::SettingsClientError;
use crate::model::{Settings, SettingsDto, UpdateSettingsResult};
use zbus::blocking::Connection;

#[derive(Clone, Debug, Default)]
pub struct SettingsDbusClient;

impl SettingsDbusClient {
    pub fn load_settings(&self) -> Result<Settings, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        let settings = proxy.get_settings().map_err(SettingsClientError::Daemon)?;
        Settings::try_from(settings).map_err(SettingsClientError::from)
    }

    pub fn save_settings(
        &self,
        settings: Settings,
    ) -> Result<UpdateSettingsResult, SettingsClientError> {
        let connection = Connection::session().map_err(SettingsClientError::Connection)?;
        let proxy =
            OpenSwitcherProxyBlocking::new(&connection).map_err(SettingsClientError::Proxy)?;
        proxy
            .update_settings(SettingsDto::from(settings))
            .map_err(SettingsClientError::Daemon)
    }
}
