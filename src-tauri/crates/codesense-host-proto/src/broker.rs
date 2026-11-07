// SPDX-License-Identifier: MIT
//! Small Unix capability transfer primitive shared by daemon and bridge.
//! Every received descriptor is owned immediately, including rejected messages.
use std::io;
use std::mem::{size_of, size_of_val, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

pub fn send_fd(stream: &UnixStream, fd: RawFd) -> io::Result<()> {
    let mut byte = [b'F'];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // usize storage provides cmsghdr alignment; reserve room for one descriptor.
    let mut ancillary = [0usize; 8];
    // SAFETY: header points into live aligned buffers throughout sendmsg.
    let result = unsafe {
        let mut msg: libc::msghdr = zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(size_of::<RawFd>() as _) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(size_of::<RawFd>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<RawFd>(), fd);
        libc::sendmsg(stream.as_raw_fd(), &msg, 0)
    };
    if result == 1 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn receive_fd(stream: &UnixStream) -> io::Result<OwnedFd> {
    let mut byte = [0u8];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut ancillary = [0usize; 32];
    let mut descriptors = Vec::new();
    let mut malformed = false;
    // SAFETY: recvmsg initializes only buffers described here; CMSG iteration
    // remains within that buffer and received FDs become RAII owners at once.
    let (count, flags) = unsafe {
        let mut msg: libc::msghdr = zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = ancillary.as_mut_ptr().cast();
        msg.msg_controllen = size_of_val(&ancillary) as _;
        let count = libc::recvmsg(stream.as_raw_fd(), &mut msg, 0);
        if count >= 0 {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let bytes =
                        ((*cmsg).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                    malformed |= !bytes.is_multiple_of(size_of::<RawFd>());
                    for index in 0..bytes / size_of::<RawFd>() {
                        let fd = std::ptr::read_unaligned(
                            libc::CMSG_DATA(cmsg).cast::<RawFd>().add(index),
                        );
                        descriptors.push(OwnedFd::from_raw_fd(fd));
                    }
                } else {
                    malformed = true;
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        (count, msg.msg_flags)
    };
    if count != 1
        || byte != [b'F']
        || malformed
        || flags & libc::MSG_CTRUNC != 0
        || descriptors.len() != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid broker capability transfer",
        ));
    }
    let fd = descriptors.pop().expect("exactly one received descriptor");
    set_inheritable(fd.as_raw_fd(), false)?;
    Ok(fd)
}

pub fn identity(fd: RawFd) -> io::Result<(u64, u64)> {
    // SAFETY: fstat writes a valid stat into owned initialized storage.
    let mut stat: libc::stat = unsafe { zeroed() };
    if unsafe { libc::fstat(fd, &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "broker capability is not a socket",
        ));
    }
    // dev_t is signed 32-bit on macOS and unsigned 64-bit on Linux.
    #[allow(clippy::unnecessary_cast)]
    let device = stat.st_dev as u64;
    Ok((device, stat.st_ino))
}

pub fn set_inheritable(fd: RawFd, inherit: bool) -> io::Result<()> {
    // SAFETY: fcntl operates only on caller's live descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let flags = if inherit {
        flags & !libc::FD_CLOEXEC
    } else {
        flags | libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reverse_mcp_capability_preserves_exact_endpoint_identity() {
        let (transport, receiver) = UnixStream::pair().unwrap();
        let (capability, peer) = UnixStream::pair().unwrap();
        send_fd(&transport, capability.as_raw_fd()).unwrap();
        let received = receive_fd(&receiver).unwrap();
        assert_eq!(
            identity(received.as_raw_fd()).unwrap(),
            identity(capability.as_raw_fd()).unwrap()
        );
        assert_ne!(
            identity(received.as_raw_fd()).unwrap(),
            identity(peer.as_raw_fd()).unwrap()
        );
        assert_ne!(
            unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
    #[test]
    fn reverse_mcp_rejects_unrelated_descriptor_and_missing_rights() {
        use std::io::Write;
        let file = std::fs::File::open("/dev/null").unwrap();
        assert!(identity(file.as_raw_fd()).is_err());
        let (mut transport, receiver) = UnixStream::pair().unwrap();
        transport.write_all(b"F").unwrap();
        assert!(receive_fd(&receiver).is_err());
    }
}
