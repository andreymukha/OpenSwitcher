use nix::errno::Errno;
use nix::poll::{poll, PollFd, PollFlags};
use std::io;
use std::os::unix::io::RawFd;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum X11WaitOutcome {
    X11Ready,
    StopRequested,
}

pub(crate) fn wait_for_x11_or_stop(x11_fd: RawFd, stop_fd: RawFd) -> io::Result<X11WaitOutcome> {
    wait_for_x11_or_stop_with(x11_fd, stop_fd, poll)
}

fn wait_for_x11_or_stop_with(
    x11_fd: RawFd,
    stop_fd: RawFd,
    mut poll_fn: impl FnMut(&mut [PollFd], i32) -> nix::Result<i32>,
) -> io::Result<X11WaitOutcome> {
    let mut fds = [
        PollFd::new(x11_fd, PollFlags::POLLIN),
        PollFd::new(stop_fd, PollFlags::POLLIN),
    ];
    let stop_events =
        PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL;
    let x11_errors = PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL;

    loop {
        match poll_fn(&mut fds, -1) {
            Ok(_) => {}
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
        }

        let stop_revents = fds[1].revents().unwrap_or_else(PollFlags::empty);
        if stop_revents.intersects(stop_events) {
            return Ok(X11WaitOutcome::StopRequested);
        }

        let x11_revents = fds[0].revents().unwrap_or_else(PollFlags::empty);
        if x11_revents.intersects(x11_errors) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "X11 connection descriptor closed",
            ));
        }
        if x11_revents.contains(PollFlags::POLLIN) {
            return Ok(X11WaitOutcome::X11Ready);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{wait_for_x11_or_stop, wait_for_x11_or_stop_with, X11WaitOutcome};
    use nix::errno::Errno;
    use std::io::{self, Write};
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn x11_readiness_returns_x11_ready() {
        let (x11_reader, mut x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, _stop_writer) = UnixStream::pair().unwrap();
        x11_writer.write_all(&[1]).unwrap();

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::X11Ready
        );
    }

    #[test]
    fn stop_readiness_returns_stop_requested() {
        let (x11_reader, _x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, stop_writer) = UnixStream::pair().unwrap();
        stop_writer.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::StopRequested
        );
    }

    #[test]
    fn stop_wins_when_both_descriptors_are_ready() {
        let (x11_reader, mut x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, stop_writer) = UnixStream::pair().unwrap();
        x11_writer.write_all(&[1]).unwrap();
        stop_writer.shutdown(std::net::Shutdown::Write).unwrap();

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::StopRequested
        );
    }

    #[test]
    fn x11_hangup_is_an_error() {
        let (x11_reader, x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, _stop_writer) = UnixStream::pair().unwrap();
        drop(x11_writer);

        let error =
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn stop_hangup_is_a_stop_request() {
        let (x11_reader, _x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, stop_writer) = UnixStream::pair().unwrap();
        drop(stop_writer);

        assert_eq!(
            wait_for_x11_or_stop(x11_reader.as_raw_fd(), stop_reader.as_raw_fd()).unwrap(),
            X11WaitOutcome::StopRequested
        );
    }

    #[test]
    fn interrupted_poll_is_retried() {
        let (x11_reader, mut x11_writer) = UnixStream::pair().unwrap();
        let (stop_reader, _stop_writer) = UnixStream::pair().unwrap();
        x11_writer.write_all(&[1]).unwrap();
        let mut calls = 0;

        let outcome = wait_for_x11_or_stop_with(
            x11_reader.as_raw_fd(),
            stop_reader.as_raw_fd(),
            |fds, _timeout| {
                calls += 1;
                if calls == 1 {
                    Err(Errno::EINTR)
                } else {
                    nix::poll::poll(fds, 0)
                }
            },
        )
        .unwrap();

        assert_eq!(outcome, X11WaitOutcome::X11Ready);
        assert_eq!(calls, 2);
    }
}
