use std::net::TcpStream;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use std::{ffi::c_void, net::TcpStream, os::fd::AsRawFd};

    #[repr(C)]
    struct Linger {
        enabled: i32,
        seconds: i32,
    }
    unsafe extern "C" {
        fn setsockopt(fd: i32, level: i32, option: i32, value: *const c_void, len: u32) -> i32;
    }
    pub(super) fn arm(stream: &TcpStream) {
        #[cfg(target_os = "linux")]
        let (level, option) = (1, 13);
        #[cfg(target_os = "macos")]
        let (level, option) = (0xffff, 0x0080);
        let linger = Linger {
            enabled: 1,
            seconds: 0,
        };
        let result = unsafe {
            setsockopt(
                stream.as_raw_fd(),
                level,
                option,
                std::ptr::from_ref(&linger).cast(),
                size_of::<Linger>() as u32,
            )
        };
        assert_eq!(
            result,
            0,
            "arming peer reset: {}",
            std::io::Error::last_os_error()
        );
    }
}
#[cfg(windows)]
mod platform {
    use std::{net::TcpStream, os::windows::io::AsRawSocket};

    #[repr(C)]
    struct Linger {
        enabled: u16,
        seconds: u16,
    }
    #[link(name = "ws2_32")]
    unsafe extern "system" {
        fn setsockopt(socket: usize, level: i32, option: i32, value: *const u8, len: i32) -> i32;
        #[link_name = "WSAGetLastError"]
        fn wsa_get_last_error() -> i32;
    }
    pub(super) fn arm(stream: &TcpStream) {
        let linger = Linger {
            enabled: 1,
            seconds: 0,
        };
        let result = unsafe {
            setsockopt(
                stream.as_raw_socket() as usize,
                0xffff,
                0x0080,
                std::ptr::from_ref(&linger).cast(),
                size_of::<Linger>() as i32,
            )
        };
        if result != 0 {
            let code = unsafe { wsa_get_last_error() };
            panic!(
                "arming peer reset: {}",
                std::io::Error::from_raw_os_error(code)
            );
        }
    }
}
pub fn reset(stream: TcpStream) {
    platform::arm(&stream);
    drop(stream);
}
