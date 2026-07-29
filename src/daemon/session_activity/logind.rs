use super::{SessionRecord, SessionSource, SessionSourceEvent};
use crate::error::SwitcherError;
use futures_util::{
    future::{select, Either},
    pin_mut, StreamExt,
};
use std::time::Duration;
use zbus::{
    zvariant::OwnedObjectPath, CacheProperties, Connection, MatchRule, MessageStream, MessageType,
    Proxy, ProxyBuilder,
};

const LOGIN1_DESTINATION: &str = "org.freedesktop.login1";
const LOGIN1_PATH: &str = "/org/freedesktop/login1";
const LOGIN1_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN1_SESSION: &str = "org.freedesktop.login1.Session";
const DBUS_PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const LOGIN1_SESSION_PATH_NAMESPACE: &str = "/org/freedesktop/login1/session";

type ListedSession = (String, u32, String, String, OwnedObjectPath);

pub(super) struct LogindSessionSource {
    connection: Option<Connection>,
    manager_signals: Option<MessageStream>,
    property_signals: Option<MessageStream>,
}

impl LogindSessionSource {
    pub(super) fn new() -> Self {
        Self {
            connection: None,
            manager_signals: None,
            property_signals: None,
        }
    }
}

impl SessionSource for LogindSessionSource {
    fn subscribe(&mut self) -> Result<(), SwitcherError> {
        let (connection, manager_signals, property_signals) = async_io::block_on(async {
            let connection = Connection::system().await?;

            // A single manager stream covers SessionNew and SessionRemoved. Other manager
            // signals can only cause an extra authoritative refresh.
            let manager_rule = MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(LOGIN1_DESTINATION)?
                .path(LOGIN1_PATH)?
                .interface(LOGIN1_MANAGER)?
                .build();
            let manager_signals =
                MessageStream::for_match_rule(manager_rule, &connection, Some(16)).await?;

            let property_rule = MatchRule::builder()
                .msg_type(MessageType::Signal)
                .sender(LOGIN1_DESTINATION)?
                .path_namespace(LOGIN1_SESSION_PATH_NAMESPACE)?
                .interface(DBUS_PROPERTIES)?
                .member("PropertiesChanged")?
                .build();
            let property_signals =
                MessageStream::for_match_rule(property_rule, &connection, Some(64)).await?;

            Ok::<_, zbus::Error>((connection, manager_signals, property_signals))
        })?;

        self.connection = Some(connection);
        self.manager_signals = Some(manager_signals);
        self.property_signals = Some(property_signals);
        Ok(())
    }

    fn snapshot(&mut self, uid: u32) -> Result<Vec<SessionRecord>, SwitcherError> {
        let connection = self.connection.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "logind session source is not subscribed",
            )
        })?;

        async_io::block_on(async {
            let manager = uncached_proxy(connection, LOGIN1_PATH, LOGIN1_MANAGER).await?;
            let sessions: Vec<ListedSession> = manager.call("ListSessions", &()).await?;
            let mut records = Vec::new();

            for (id, session_uid, _user_name, seat, object_path) in sessions {
                if session_uid != uid {
                    continue;
                }

                let session =
                    uncached_proxy(connection, object_path.as_str(), LOGIN1_SESSION).await?;
                records.push(SessionRecord {
                    id,
                    uid: session_uid,
                    seat,
                    session_type: session.get_property("Type").await?,
                    class: session.get_property("Class").await?,
                    active: session.get_property("Active").await?,
                    remote: session.get_property("Remote").await?,
                });
            }

            Ok::<_, zbus::Error>(records)
        })
        .map_err(Into::into)
    }

    fn wait_for_change(&mut self, timeout: Duration) -> Result<SessionSourceEvent, SwitcherError> {
        let manager_signals = self.manager_signals.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "logind manager signal stream is unavailable",
            )
        })?;
        let property_signals = self.property_signals.as_mut().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "logind property signal stream is unavailable",
            )
        })?;

        async_io::block_on(wait_for_signal(manager_signals, property_signals, timeout))
            .map_err(Into::into)
    }
}

async fn uncached_proxy<'a>(
    connection: &'a Connection,
    path: &'a str,
    interface: &'a str,
) -> zbus::Result<Proxy<'a>> {
    ProxyBuilder::<Proxy<'a>>::new_bare(connection)
        .destination(LOGIN1_DESTINATION)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()
        .await
}

async fn wait_for_signal(
    manager_signals: &mut MessageStream,
    property_signals: &mut MessageStream,
    timeout: Duration,
) -> zbus::Result<SessionSourceEvent> {
    let manager_next = manager_signals.next();
    let property_next = property_signals.next();
    pin_mut!(manager_next, property_next);
    let next_signal = select(manager_next, property_next);
    pin_mut!(next_signal);

    let timer = async_io::Timer::after(timeout);
    pin_mut!(timer);

    match select(next_signal, timer).await {
        Either::Right((_elapsed, _pending_signal)) => Ok(SessionSourceEvent::Timeout),
        Either::Left((signal, _timer)) => {
            let message = match signal {
                Either::Left((message, _property_next)) => message,
                Either::Right((message, _manager_next)) => message,
            };
            match message {
                Some(Ok(_)) => Ok(SessionSourceEvent::Changed),
                Some(Err(error)) => Err(error),
                None => Err(zbus::Error::Failure(
                    "logind signal stream disconnected".to_owned(),
                )),
            }
        }
    }
}
