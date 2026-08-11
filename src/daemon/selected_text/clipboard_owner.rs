use super::clipboard_transaction::ClipboardOwnerToken;
use x11rb::protocol::xproto::{Atom, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

pub(super) struct X11ClipboardOwnerProbe {
    connection: RustConnection,
    clipboard_atom: Atom,
}

impl X11ClipboardOwnerProbe {
    pub(super) fn try_new() -> Option<Self> {
        let (connection, _) = RustConnection::connect(None).ok()?;
        let clipboard_atom = connection
            .intern_atom(false, b"CLIPBOARD")
            .ok()?
            .reply()
            .ok()?
            .atom;

        Some(Self {
            connection,
            clipboard_atom,
        })
    }

    pub(super) fn current_owner(&self) -> Option<ClipboardOwnerToken> {
        self.connection
            .get_selection_owner(self.clipboard_atom)
            .ok()?
            .reply()
            .ok()
            .map(|reply| ClipboardOwnerToken(reply.owner))
    }
}
