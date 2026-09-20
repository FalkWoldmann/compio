use std::{
    io,
    os::windows::{io::AsRawHandle, process::ExitStatusExt},
    process,
    task::Poll,
};

use compio_buf::{BufResult, IntoInner, IoBuf, IoBufMut};
use compio_driver::{
    BufferRef, OpCode, OpType, ResultTakeBuffer, ToSharedFd,
    op::{BufResultExt, Read, ReadManaged, Write},
    syscall,
};
use compio_io::{AsyncRead, AsyncReadManaged, AsyncWrite};
use compio_runtime::Runtime;
use windows_sys::Win32::System::{IO::OVERLAPPED, Threading::GetExitCodeProcess};

use crate::{ChildStderr, ChildStdin, ChildStdout};

struct WaitProcess {
    child: process::Child,
}

impl WaitProcess {
    pub fn new(child: process::Child) -> Self {
        Self { child }
    }
}

// SAFETY: `OpCode` requires the operation be safe to poll according to the
// `OpType` it returns. This one returns `OpType::Event(handle)`, naming the
// child's process handle, and `operate` is only ever called after that event
// signals. The handle is kept alive by the `process::Child` this op owns, and
// `operate` reads the exit code through it without retaining any pointer, so
// there are no self-references for `init` to pin.
unsafe impl OpCode for WaitProcess {
    type Control = ();

    unsafe fn init(&mut self, _: &mut Self::Control) {}

    fn op_type(&self, _: &Self::Control) -> OpType {
        OpType::Event(self.child.as_raw_handle() as _)
    }

    unsafe fn operate(
        &mut self,
        _: &mut Self::Control,
        _optr: *mut OVERLAPPED,
    ) -> Poll<io::Result<usize>> {
        let mut code = 0;
        syscall!(
            BOOL,
            GetExitCodeProcess(self.child.as_raw_handle() as _, &mut code)
        )?;
        Poll::Ready(Ok(code as _))
    }
}

pub async fn child_wait(child: process::Child) -> io::Result<process::ExitStatus> {
    let op = WaitProcess::new(child);
    let code = compio_runtime::submit(op).await.0?;
    Ok(process::ExitStatus::from_raw(code as _))
}

impl AsyncRead for ChildStdout {
    async fn read<B: IoBufMut>(&mut self, buffer: B) -> BufResult<usize, B> {
        let fd = self.to_shared_fd();
        let op = Read::new(fd, buffer);
        let res = compio_runtime::submit(op).await.into_inner();
        // SAFETY: the completed `Read` reported how many bytes it wrote into
        // the buffer, so advancing to that length only covers initialized
        // bytes.
        unsafe { res.map_advanced() }
    }
}

impl AsyncReadManaged for ChildStdout {
    type Buffer = BufferRef;

    async fn read_managed(&mut self, len: usize) -> io::Result<Option<Self::Buffer>> {
        let fd = self.to_shared_fd();
        let res = Runtime::with_current(|rt| {
            let buffer_pool = rt.buffer_pool()?;
            let op = ReadManaged::new(fd, &buffer_pool, len)?;
            io::Result::Ok(rt.submit(op))
        })?
        .await;
        // SAFETY: the managed read completed, so the pool slice it names is
        // filled to the reported length.
        unsafe { res.take_buffer() }
    }
}

impl AsyncRead for ChildStderr {
    async fn read<B: IoBufMut>(&mut self, buffer: B) -> BufResult<usize, B> {
        let fd = self.to_shared_fd();
        let op = Read::new(fd, buffer);
        let res = compio_runtime::submit(op).await.into_inner();
        // SAFETY: the completed `Read` reported how many bytes it wrote into
        // the buffer, so advancing to that length only covers initialized
        // bytes.
        unsafe { res.map_advanced() }
    }
}

impl AsyncReadManaged for ChildStderr {
    type Buffer = BufferRef;

    async fn read_managed(&mut self, len: usize) -> io::Result<Option<Self::Buffer>> {
        let fd = self.to_shared_fd();
        let res = Runtime::with_current(|rt| {
            let buffer_pool = rt.buffer_pool()?;
            let op = ReadManaged::new(fd, &buffer_pool, len)?;
            io::Result::Ok(rt.submit(op))
        })?
        .await;
        // SAFETY: the managed read completed, so the pool slice it names is
        // filled to the reported length.
        unsafe { res.take_buffer() }
    }
}

impl AsyncWrite for ChildStdin {
    async fn write<T: IoBuf>(&mut self, buffer: T) -> BufResult<usize, T> {
        let fd = self.to_shared_fd();
        let op = Write::new(fd, buffer);
        compio_runtime::submit(op).await.into_inner()
    }

    async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        Ok(())
    }
}
