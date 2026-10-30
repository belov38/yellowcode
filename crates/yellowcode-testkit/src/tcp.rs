#[path = "../../yellowcode-http/tests/support/tcp_reset.rs"]
mod reset;
pub use reset::reset;

use std::net::TcpStream;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket;

#[cfg(unix)]
type Socket = i32;
#[cfg(windows)]
type Socket = usize;
#[cfg(target_os = "linux")]
type Count = usize;
#[cfg(any(target_os = "macos", windows))]
type Count = u32;

#[repr(C)]
struct PollFd {
    socket: Socket,
    events: i16,
    returned: i16,
}
#[cfg(unix)]
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: Count, timeout: i32) -> i32;
}
#[cfg(windows)]
#[link(name = "ws2_32")]
unsafe extern "system" {
    #[link_name = "WSAPoll"]
    fn poll(fds: *mut PollFd, count: Count, timeout: i32) -> i32;
}

pub fn wait_for_reset(peer: &TcpStream) {
    #[cfg(unix)]
    let (socket, readable, retired) = (peer.as_raw_fd(), 0x0001, 0x0018);
    #[cfg(windows)]
    let (socket, readable, retired) = (peer.as_raw_socket() as usize, 0x0100, 0x0003);
    let mut fd = PollFd {
        socket,
        events: readable,
        returned: 0,
    };
    let ready = unsafe { poll(&mut fd, 1, 30_000) };
    assert_eq!(
        ready,
        1,
        "reset arrival: {}",
        std::io::Error::last_os_error()
    );
    assert_ne!(fd.returned & retired, 0, "peer reset not received");
}
