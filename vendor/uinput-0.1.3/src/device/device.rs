use std::{mem, ptr, slice};
use libc::c_int;
use libc::{timeval, gettimeofday};
use nix::unistd;
use ffi::*;
use {Result as Res, event};
use event::{Kind, Code};

/// The virtual device.
pub struct Device {
	fd: c_int,
}

impl Device {
	/// Wrap a file descriptor in a `Device`.
	pub fn new(fd: c_int) -> Self {
		Device {
			fd: fd
		}
	}

	#[doc(hidden)]
	pub fn write(&mut self, kind: c_int, code: c_int, value: c_int) -> Res<()> {
		unsafe {
			let mut event = input_event {
				time:  timeval { tv_sec: 0, tv_usec: 0 },
				kind:  kind as u16,
				code:  code as u16,
				value: value as i32,
			};

			gettimeofday(&mut event.time, ptr::null_mut());

			let ptr  = &event as *const _ as *const u8;
			let size = mem::size_of_val(&event);

			try!(unistd::write(self.fd, slice::from_raw_parts(ptr, size)));
		}

		Ok(())
	}

	/// Synchronize the device.
	pub fn synchronize(&mut self) -> Res<()> {
		self.write(EV_SYN, SYN_REPORT, 0)
	}

	/// Send an event.
	pub fn send<T: Into<event::Event>>(&mut self, event: T, value: i32) -> Res<()> {
		let event = event.into();
		self.write(event.kind(), event.code(), value)
	}

	/// Send a press event.
	pub fn press<T: event::Press>(&mut self, event: &T) -> Res<()> {
		self.write(event.kind(), event.code(), 1)
	}

	/// Send a release event.
	pub fn release<T: event::Release>(&mut self, event: &T) -> Res<()> {
		self.write(event.kind(), event.code(), 0)
	}

	/// Send a press and release event.
	pub fn click<T: event::Press + event::Release>(&mut self, event: &T) -> Res<()> {
		try!(self.press(event));
		try!(self.release(event));

		Ok(())
	}

	/// Send a relative or absolute positioning event.
	pub fn position<T: event::Position>(&mut self, event: &T, value: i32) -> Res<()> {
		self.write(event.kind(), event.code(), value)
	}
}

impl Drop for Device {
	fn drop(&mut self) {
		if self.fd < 0 {
			return;
		}

		unsafe {
			ui_dev_destroy(self.fd);
		}
		let _ = unistd::close(self.fd);
		self.fd = -1;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pipe_write_fd() -> c_int {
		let mut fds = [-1; 2];
		assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
		assert_eq!(unsafe { libc::close(fds[0]) }, 0);
		fds[1]
	}

	fn fd_is_open(fd: c_int) -> bool {
		unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
	}

	#[test]
	fn device_drop_closes_owned_fd() {
		let fd = pipe_write_fd();
		drop(Device::new(fd));

		let still_open = fd_is_open(fd);
		if still_open {
			assert_eq!(unsafe { libc::close(fd) }, 0);
		}
		assert!(!still_open, "Device::drop left its fd open");
	}
}
