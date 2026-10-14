//! Cancellable pipe/terminal stdout writes. Regular files use Tokio's adapter.
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::AsyncWrite;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs::File,
        io::Write,
        os::fd::{AsRawFd, FromRawFd},
    };
    use tokio::io::unix::AsyncFd;

    pub struct NonblockingFile {
        file: File,
        flags: libc::c_int,
    }

    impl AsRawFd for NonblockingFile {
        fn as_raw_fd(&self) -> libc::c_int {
            self.file.as_raw_fd()
        }
    }

    impl Drop for NonblockingFile {
        fn drop(&mut self) {
            // dup shares stdout's open file description. Restore its flags;
            // observation does not run concurrently with other CLI output.
            unsafe {
                libc::fcntl(self.as_raw_fd(), libc::F_SETFL, self.flags);
            }
        }
    }

    pub enum Stdout {
        Ready(AsyncFd<NonblockingFile>),
        File(tokio::fs::File),
    }

    impl Stdout {
        pub fn new() -> io::Result<Self> {
            // Own the duplicate, never close the process stdout descriptor.
            let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let file = unsafe { File::from_raw_fd(fd) };
            Self::from_file(file)
        }

        fn from_file(file: File) -> io::Result<Self> {
            let fd = file.as_raw_fd();
            if file.metadata()?.is_file() {
                return Ok(Self::File(tokio::fs::File::from_std(file)));
            }
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(Self::Ready(AsyncFd::new(NonblockingFile { file, flags })?))
        }
    }

    impl AsyncWrite for Stdout {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            match &mut *self {
                Self::File(file) => Pin::new(file).poll_write(cx, bytes),
                Self::Ready(fd) => loop {
                    let mut ready = std::task::ready!(fd.poll_write_ready(cx))?;
                    match ready.try_io(|inner| (&inner.get_ref().file).write(bytes)) {
                        Ok(result) => return Poll::Ready(result),
                        Err(_) => continue,
                    }
                },
            }
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match &mut *self {
                Self::File(file) => Pin::new(file).poll_flush(cx),
                Self::Ready(_) => Poll::Ready(Ok(())),
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.poll_flush(cx)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            os::{fd::OwnedFd, unix::net::UnixStream},
            time::Duration,
        };
        use tokio::io::AsyncWriteExt;

        #[tokio::test]
        async fn unread_socket_output_times_out_and_restores_descriptor_flags() {
            let (socket, _unread_peer) = UnixStream::pair().unwrap();
            let flags = unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) };
            let file = File::from(OwnedFd::from(socket.try_clone().unwrap()));
            let mut output = Stdout::from_file(file).unwrap();
            let bytes = vec![b'x'; 4 * 1024 * 1024];
            assert!(
                tokio::time::timeout(Duration::from_millis(20), output.write_all(&bytes),)
                    .await
                    .is_err()
            );
            drop(output);
            assert_eq!(
                unsafe { libc::fcntl(socket.as_raw_fd(), libc::F_GETFL) },
                flags,
            );
        }
    }
}

#[cfg(unix)]
pub use unix::Stdout;

#[cfg(not(unix))]
pub struct Stdout(tokio::io::Stdout);

#[cfg(not(unix))]
impl Stdout {
    pub fn new() -> io::Result<Self> {
        Ok(Self(tokio::io::stdout()))
    }
}

#[cfg(not(unix))]
impl AsyncWrite for Stdout {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
