//! Passing file descriptors over a Unix socket (SCM_RIGHTS).

use std::collections::VecDeque;
use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Room for a few fds per message.
const CMSG_SPACE: usize = 64;

/// Send `bytes` (at least its first byte) with `fd` attached; returns how
/// many bytes went.
pub fn send_with_fd(sock: &UnixStream, bytes: &[u8], fd: RawFd) -> io::Result<usize> {
    let mut iov = libc::iovec { iov_base: bytes.as_ptr() as *mut _, iov_len: bytes.len() };
    let mut cmsg_buf = [0u8; CMSG_SPACE];
    // SAFETY: the buffers outlive the call; the cmsg header is written within
    // `cmsg_buf`, sized by CMSG_SPACE for one fd.
    unsafe {
        let space = libc::CMSG_SPACE(mem::size_of::<RawFd>() as u32) as usize;
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = space as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<RawFd>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), fd);
        loop {
            let n = libc::sendmsg(sock.as_raw_fd(), &msg, 0);
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

/// Read into `buf`, queueing any fds that came along (closed on exec).
pub fn recv_with_fds(sock: &UnixStream, buf: &mut [u8], fds: &mut VecDeque<OwnedFd>) -> io::Result<usize> {
    let mut iov = libc::iovec { iov_base: buf.as_mut_ptr().cast(), iov_len: buf.len() };
    let mut cmsg_buf = [0u8; CMSG_SPACE];
    // SAFETY: as above; every fd read from a SCM_RIGHTS header is new and
    // owned by us.
    unsafe {
        let mut msg: libc::msghdr = mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = CMSG_SPACE as _;
        let n = loop {
            let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, 0);
            if n >= 0 {
                break n as usize;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        };
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(cmsg).cast::<RawFd>();
                let count = ((*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize) / mem::size_of::<RawFd>();
                for i in 0..count {
                    let fd = std::ptr::read_unaligned(data.add(i));
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                    fds.push_back(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
        Ok(n)
    }
}
