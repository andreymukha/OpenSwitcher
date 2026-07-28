use crate::daemon::xtest_guardian::protocol::MAX_FRAME_BYTES;
use crate::error::{InputSafetyError, SwitcherError};
use nix::errno::Errno;
#[cfg(test)]
use nix::sys::socket::socketpair;
use nix::sys::socket::{
    accept4, connect, getsockname, getsockopt, recvmsg, send, setsockopt, socket, sockopt,
    AddressFamily, ControlMessageOwned, MsgFlags, SockFlag, SockType, UnixAddr, UnixCredentials,
};
use nix::unistd::getuid;
use std::fs;
use std::io::IoSliceMut;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const SYSTEMD_LISTEN_FD: RawFd = 3;
const LISTEN_PID: &str = "LISTEN_PID";
const LISTEN_FDS: &str = "LISTEN_FDS";
const LISTEN_FDNAMES: &str = "LISTEN_FDNAMES";

fn nix_error(error: Errno) -> SwitcherError {
    std::io::Error::from_raw_os_error(error as i32).into()
}

fn oversized_frame(actual: usize) -> SwitcherError {
    InputSafetyError::OversizedFrame {
        actual,
        maximum: MAX_FRAME_BYTES,
    }
    .into()
}

pub(crate) struct Seqpacket {
    fd: OwnedFd,
    receive_authentication: ReceiveAuthentication,
}

#[derive(Clone, Copy, Debug)]
enum ReceiveAuthentication {
    /// The accepted peer was authenticated before any request is processed.
    PinnedPeer,
    /// With socket activation, `SO_PEERCRED` on the client identifies the
    /// process that called `listen(2)` (systemd), not the later acceptor.
    SenderCredentials { own_executable: ExecutableIdentity },
}

impl Seqpacket {
    pub(crate) fn connect(path: &Path) -> Result<Self, SwitcherError> {
        let raw_fd = socket(
            AddressFamily::Unix,
            SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
        .map_err(nix_error)?;
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        enable_sender_credentials(fd.as_raw_fd())?;
        let address = UnixAddr::new(path).map_err(nix_error)?;
        connect(fd.as_raw_fd(), &address).map_err(nix_error)?;
        // The listener belongs to the user manager.  Its UID is still a useful
        // early boundary, while the real guardian is authenticated from
        // SCM_CREDENTIALS attached by the kernel to every response.
        authenticate_listener_uid(fd.as_raw_fd())?;
        Ok(Self {
            fd,
            receive_authentication: ReceiveAuthentication::SenderCredentials {
                own_executable: executable_identity(Path::new("/proc/self/exe"))?,
            },
        })
    }

    #[cfg(test)]
    pub(crate) fn pair() -> Result<(Self, Self), SwitcherError> {
        let (left, right) = socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::SOCK_CLOEXEC,
        )
        .map_err(nix_error)?;
        let left = unsafe { OwnedFd::from_raw_fd(left) };
        let right = unsafe { OwnedFd::from_raw_fd(right) };
        enable_sender_credentials(left.as_raw_fd())?;
        enable_sender_credentials(right.as_raw_fd())?;
        let authentication = ReceiveAuthentication::SenderCredentials {
            own_executable: executable_identity(Path::new("/proc/self/exe"))?,
        };
        Ok((
            Self {
                fd: left,
                receive_authentication: authentication,
            },
            Self {
                fd: right,
                receive_authentication: authentication,
            },
        ))
    }

    pub(crate) fn send_frame(&self, frame: &[u8]) -> Result<(), SwitcherError> {
        if frame.len() > MAX_FRAME_BYTES {
            return Err(oversized_frame(frame.len()));
        }
        self.send_datagram(frame)
    }

    #[cfg(test)]
    fn send_unchecked(&self, frame: &[u8]) -> Result<(), SwitcherError> {
        self.send_datagram(frame)
    }

    fn send_datagram(&self, frame: &[u8]) -> Result<(), SwitcherError> {
        let sent = send(self.fd.as_raw_fd(), frame, MsgFlags::MSG_NOSIGNAL).map_err(nix_error)?;
        if sent != frame.len() {
            return Err(SwitcherError::input_safety(
                "XTEST guardian transport performed a partial packet send",
            ));
        }
        Ok(())
    }

    pub(crate) fn recv_frame(&self) -> Result<Vec<u8>, SwitcherError> {
        let mut buffer = [0u8; MAX_FRAME_BYTES + 1];
        let mut iov = [IoSliceMut::new(&mut buffer)];
        let mut control_space = nix::cmsg_space!(UnixCredentials, [RawFd; 4]);
        let (received, flags, credentials, unexpected_control) = {
            let message = recvmsg::<()>(
                self.fd.as_raw_fd(),
                &mut iov,
                Some(&mut control_space),
                MsgFlags::MSG_TRUNC | MsgFlags::MSG_CMSG_CLOEXEC,
            )
            .map_err(nix_error)?;
            let mut credentials = None;
            let mut unexpected_control = false;
            for control in message.cmsgs() {
                match control {
                    ControlMessageOwned::ScmCredentials(value) if credentials.is_none() => {
                        credentials = Some(value);
                    }
                    ControlMessageOwned::ScmRights(descriptors) => {
                        unexpected_control = true;
                        for descriptor in descriptors {
                            let _ = nix::unistd::close(descriptor);
                        }
                    }
                    _ => unexpected_control = true,
                }
            }
            (
                message.bytes,
                message.flags,
                credentials,
                unexpected_control,
            )
        };

        if flags.contains(MsgFlags::MSG_CTRUNC) || unexpected_control {
            return Err(SwitcherError::input_safety(
                "XTEST guardian transport received unexpected ancillary data",
            ));
        }
        if flags.contains(MsgFlags::MSG_TRUNC) || received > MAX_FRAME_BYTES {
            return Err(oversized_frame(received.max(MAX_FRAME_BYTES + 1)));
        }
        if received == 0 && credentials.is_none() {
            return Ok(Vec::new());
        }

        match self.receive_authentication {
            ReceiveAuthentication::PinnedPeer => {
                if let Some(credentials) = credentials {
                    validate_sender_credentials(credentials, None)?;
                }
            }
            ReceiveAuthentication::SenderCredentials { own_executable } => {
                let credentials = credentials.ok_or_else(|| {
                    SwitcherError::input_safety(
                        "XTEST guardian transport frame has no sender credentials",
                    )
                })?;
                validate_sender_credentials(credentials, Some(own_executable))?;
            }
        }
        Ok(buffer[..received].to_vec())
    }

    pub(crate) fn authenticate_peer(&self) -> Result<(), SwitcherError> {
        authenticate_peer(self.fd.as_raw_fd())
    }
}

impl AsRawFd for Seqpacket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

pub(crate) struct ActivatedListener {
    fd: OwnedFd,
}

impl ActivatedListener {
    pub(crate) fn from_process_environment() -> Result<Self, SwitcherError> {
        let metadata = ActivationMetadata::from_process_environment()?;
        validate_activation_metadata(&metadata, std::process::id())?;

        let fd = unsafe { OwnedFd::from_raw_fd(SYSTEMD_LISTEN_FD) };
        validate_activated_listener_fd(fd.as_raw_fd())?;
        set_close_on_exec(fd.as_raw_fd())?;
        clear_activation_environment();
        Ok(Self { fd })
    }

    pub(crate) fn accept_authenticated(&self) -> Result<Seqpacket, SwitcherError> {
        let raw_fd = accept4(self.fd.as_raw_fd(), SockFlag::SOCK_CLOEXEC).map_err(nix_error)?;
        let connection = Seqpacket {
            fd: unsafe { OwnedFd::from_raw_fd(raw_fd) },
            receive_authentication: ReceiveAuthentication::PinnedPeer,
        };
        connection.authenticate_peer()?;
        Ok(connection)
    }
}

impl AsRawFd for ActivatedListener {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActivationMetadata {
    listen_pid: u32,
    listen_fds: u32,
}

impl ActivationMetadata {
    fn from_process_environment() -> Result<Self, SwitcherError> {
        Ok(Self {
            listen_pid: parse_activation_integer(LISTEN_PID)?,
            listen_fds: parse_activation_integer(LISTEN_FDS)?,
        })
    }
}

fn parse_activation_integer(name: &'static str) -> Result<u32, SwitcherError> {
    let value = std::env::var_os(name)
        .ok_or_else(|| SwitcherError::input_safety("systemd activation metadata is missing"))?;
    let value = value
        .to_str()
        .ok_or_else(|| SwitcherError::input_safety("systemd activation metadata is not UTF-8"))?;
    value
        .parse()
        .map_err(|_| SwitcherError::input_safety("systemd activation metadata is invalid"))
}

fn validate_activation_metadata(
    metadata: &ActivationMetadata,
    current_pid: u32,
) -> Result<(), SwitcherError> {
    if metadata.listen_pid == 0 || metadata.listen_pid != current_pid {
        return Err(SwitcherError::input_safety(
            "systemd activation PID does not match this process",
        ));
    }
    if metadata.listen_fds != 1 {
        return Err(SwitcherError::input_safety(
            "systemd activation must provide exactly one descriptor",
        ));
    }
    Ok(())
}

fn clear_activation_environment() {
    std::env::remove_var(LISTEN_PID);
    std::env::remove_var(LISTEN_FDS);
    std::env::remove_var(LISTEN_FDNAMES);
}

fn validate_activated_listener_fd(fd: RawFd) -> Result<(), SwitcherError> {
    let socket_type = getsockopt(fd, sockopt::SockType).map_err(nix_error)?;
    if socket_type != SockType::SeqPacket {
        return Err(SwitcherError::input_safety(
            "activated guardian socket is not SOCK_SEQPACKET",
        ));
    }
    let _: UnixAddr = getsockname(fd).map_err(nix_error)?;
    if !getsockopt(fd, sockopt::AcceptConn).map_err(nix_error)? {
        return Err(SwitcherError::input_safety(
            "activated guardian socket is not listening",
        ));
    }
    Ok(())
}

fn set_close_on_exec(fd: RawFd) -> Result<(), SwitcherError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn enable_sender_credentials(fd: RawFd) -> Result<(), SwitcherError> {
    setsockopt(fd, sockopt::PassCred, &true).map_err(nix_error)
}

#[cfg(test)]
fn descriptor_is_close_on_exec(fd: RawFd) -> Result<bool, SwitcherError> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(flags & libc::FD_CLOEXEC != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

fn executable_identity(path: &Path) -> Result<ExecutableIdentity, SwitcherError> {
    let metadata = fs::metadata(path)?;
    Ok(ExecutableIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn validate_executable_identity(
    own: ExecutableIdentity,
    peer: ExecutableIdentity,
) -> Result<(), SwitcherError> {
    if own != peer {
        return Err(SwitcherError::input_safety(
            "guardian peer executable does not match this binary",
        ));
    }
    Ok(())
}

fn authenticate_peer(fd: RawFd) -> Result<(), SwitcherError> {
    let credentials = getsockopt(fd, sockopt::PeerCredentials).map_err(nix_error)?;
    validate_sender_credentials(
        credentials,
        Some(executable_identity(Path::new("/proc/self/exe"))?),
    )
}

fn authenticate_listener_uid(fd: RawFd) -> Result<(), SwitcherError> {
    let credentials = getsockopt(fd, sockopt::PeerCredentials).map_err(nix_error)?;
    validate_sender_credentials(credentials, None)
}

fn validate_sender_credentials(
    credentials: UnixCredentials,
    own_executable: Option<ExecutableIdentity>,
) -> Result<(), SwitcherError> {
    if credentials.uid() != getuid().as_raw() {
        return Err(SwitcherError::input_safety(
            "guardian peer UID does not match current UID",
        ));
    }
    if credentials.pid() <= 0 {
        return Err(SwitcherError::input_safety("guardian peer PID is invalid"));
    }

    if let Some(own_executable) = own_executable {
        let peer_path = format!("/proc/{}/exe", credentials.pid());
        let peer = executable_identity(Path::new(&peer_path))?;
        validate_executable_identity(own_executable, peer)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::xtest_guardian::protocol::MAX_FRAME_BYTES;
    use crate::error::InputSafetyError;
    use nix::sys::socket::{
        bind, listen, sendmsg, socket, AddressFamily, ControlMessage, SockFlag, SockType, UnixAddr,
    };
    use std::io::IoSlice;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::path::PathBuf;

    fn seqpacket_listener() -> (tempfile::TempDir, PathBuf, OwnedFd) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guardian.sock");
        let raw_fd = socket(
            AddressFamily::Unix,
            SockType::SeqPacket,
            SockFlag::SOCK_CLOEXEC,
            None,
        )
        .unwrap();
        bind(raw_fd, &UnixAddr::new(&path).unwrap()).unwrap();
        listen(raw_fd, 1).unwrap();
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        (directory, path, fd)
    }

    fn open_descriptor_count() -> usize {
        fs::read_dir("/proc/self/fd").unwrap().count()
    }

    #[test]
    fn seqpacket_preserves_one_frame_per_datagram() {
        let (left, right) = Seqpacket::pair().unwrap();

        left.send_frame(b"one").unwrap();
        left.send_frame(b"two").unwrap();

        assert_eq!(right.recv_frame().unwrap(), b"one");
        assert_eq!(right.recv_frame().unwrap(), b"two");
    }

    #[test]
    fn oversized_datagram_is_rejected_not_returned_truncated() {
        let (left, right) = Seqpacket::pair().unwrap();
        left.send_unchecked(&vec![0x41; MAX_FRAME_BYTES + 1])
            .unwrap();

        assert!(matches!(
            right.recv_frame(),
            Err(SwitcherError::InputSafety(
                InputSafetyError::OversizedFrame {
                    actual,
                    maximum: MAX_FRAME_BYTES
                }
            )) if actual > MAX_FRAME_BYTES
        ));
    }

    #[test]
    fn oversized_outgoing_frame_is_rejected_before_send() {
        let (left, _right) = Seqpacket::pair().unwrap();

        assert!(matches!(
            left.send_frame(&vec![0x41; MAX_FRAME_BYTES + 1]),
            Err(SwitcherError::InputSafety(
                InputSafetyError::OversizedFrame {
                    actual,
                    maximum: MAX_FRAME_BYTES
                }
            )) if actual == MAX_FRAME_BYTES + 1
        ));
    }

    #[test]
    fn unexpected_passed_descriptor_is_closed_and_rejected() {
        let (left, right) = Seqpacket::pair().unwrap();
        let descriptor_count_before = open_descriptor_count();
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();
        let payload = [IoSlice::new(b"x")];
        let descriptors = [read_fd];

        sendmsg::<()>(
            left.as_raw_fd(),
            &payload,
            &[ControlMessage::ScmRights(&descriptors)],
            MsgFlags::MSG_NOSIGNAL,
            None,
        )
        .unwrap();
        nix::unistd::close(read_fd).unwrap();
        nix::unistd::close(write_fd).unwrap();

        let result = right.recv_frame();
        assert!(matches!(result, Err(SwitcherError::InputSafety(_))));
        assert_eq!(open_descriptor_count(), descriptor_count_before);
    }

    #[test]
    fn socket_descriptors_are_close_on_exec_and_peer_credentials_match() {
        let (left, right) = Seqpacket::pair().unwrap();

        assert!(descriptor_is_close_on_exec(left.as_raw_fd()).unwrap());
        assert!(descriptor_is_close_on_exec(right.as_raw_fd()).unwrap());
        left.authenticate_peer().unwrap();
        right.authenticate_peer().unwrap();
    }

    #[test]
    fn every_receiver_enables_kernel_sender_credentials() {
        let (left, right) = Seqpacket::pair().unwrap();

        assert!(getsockopt(left.as_raw_fd(), sockopt::PassCred).unwrap());
        assert!(getsockopt(right.as_raw_fd(), sockopt::PassCred).unwrap());
    }

    #[test]
    fn every_nonempty_frame_requires_kernel_sender_credentials() {
        let (left, right) = Seqpacket::pair().unwrap();
        nix::sys::socket::setsockopt(right.as_raw_fd(), sockopt::PassCred, &false).unwrap();

        left.send_frame(b"unauthenticated").unwrap();

        assert!(matches!(
            right.recv_frame(),
            Err(SwitcherError::InputSafety(_))
        ));
    }

    #[test]
    fn activation_metadata_rejects_wrong_pid_and_fd_count() {
        let current_pid = std::process::id();
        assert!(validate_activation_metadata(
            &ActivationMetadata {
                listen_pid: 0,
                listen_fds: 1,
            },
            current_pid,
        )
        .is_err());
        assert!(validate_activation_metadata(
            &ActivationMetadata {
                listen_pid: current_pid,
                listen_fds: 2,
            },
            current_pid,
        )
        .is_err());
        validate_activation_metadata(
            &ActivationMetadata {
                listen_pid: current_pid,
                listen_fds: 1,
            },
            current_pid,
        )
        .unwrap();
    }

    #[test]
    fn activated_fd_must_be_a_listening_unix_seqpacket_socket() {
        let (_directory, _path, listener) = seqpacket_listener();
        validate_activated_listener_fd(listener.as_raw_fd()).unwrap();

        let stream = std::os::unix::net::UnixStream::pair().unwrap().0;
        assert!(validate_activated_listener_fd(stream.as_raw_fd()).is_err());

        let connected_seqpacket = Seqpacket::pair().unwrap().0;
        assert!(validate_activated_listener_fd(connected_seqpacket.as_raw_fd()).is_err());
    }

    #[test]
    fn connected_daemon_and_authenticated_acceptor_exchange_frames() {
        let (_directory, path, listener) = seqpacket_listener();
        let server = std::thread::spawn(move || {
            let listener = ActivatedListener { fd: listener };
            let connection = listener.accept_authenticated().unwrap();
            assert_eq!(connection.recv_frame().unwrap(), b"hello");
            connection.send_frame(b"ready").unwrap();
        });

        let client = Seqpacket::connect(&path).unwrap();
        client.send_frame(b"hello").unwrap();
        assert_eq!(client.recv_frame().unwrap(), b"ready");
        server.join().unwrap();
    }

    #[test]
    fn peer_binary_identity_rejects_different_device_or_inode() {
        let expected = ExecutableIdentity {
            device: 11,
            inode: 22,
        };
        validate_executable_identity(expected, expected).unwrap();
        assert!(validate_executable_identity(
            expected,
            ExecutableIdentity {
                device: 11,
                inode: 23,
            },
        )
        .is_err());
        assert!(validate_executable_identity(
            expected,
            ExecutableIdentity {
                device: 12,
                inode: 22,
            },
        )
        .is_err());
    }
}
