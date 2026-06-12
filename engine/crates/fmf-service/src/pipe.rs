//! Overlapped named-pipe I/O behind blocking `Read`/`Write` so the frame
//! codec (fmf-proto) works unchanged. The pipe is created OVERLAPPED solely
//! so the accept loop can wait on (connect, stop) at once; data I/O issues
//! an overlapped op and immediately waits on its per-call event — blocking
//! semantics, cancel-safe via `CloseHandle` (pending ops fail, threads exit).

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::Arc;

use windows_sys::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE,
    OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;
const BUFFER_SIZE: u32 = 64 * 1024;

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Auto-reset event handle (owned).
pub struct Event(OwnedHandle);

impl Event {
    pub fn new() -> io::Result<Self> {
        let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if h.is_null() {
            return Err(last_error());
        }
        Ok(Self(unsafe {
            OwnedHandle::from_raw_handle(h as RawHandle)
        }))
    }

    pub fn set(&self) {
        unsafe { SetEvent(self.0.as_raw_handle() as HANDLE) };
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

/// One duplex pipe endpoint. Cloning shares the OS handle; reads and writes
/// may run on different threads (independent OVERLAPPED + events).
#[derive(Clone)]
pub struct PipeStream {
    handle: Arc<OwnedHandle>,
}

impl PipeStream {
    pub(crate) fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// Client side: opens an existing pipe (blocking I/O is fine here, but
    /// we open OVERLAPPED for symmetry with the I/O helpers).
    pub fn connect(path: &str) -> io::Result<Self> {
        // SQOS with Identification level is mandatory: the server's
        // verify_client ImpersonateNamedPipeClient's the connection to read
        // the caller's SID against authorized_sids. Without SECURITY_SQOS_PRESENT
        // the client defaults to SecurityAnonymous and the server gets an
        // anonymous token → rejected (ERROR_PIPE_NOT_CONNECTED at the client).
        const SECURITY_SQOS_PRESENT: u32 = 0x0010_0000;
        const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
        let h = unsafe {
            CreateFileW(
                wide(path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_NONE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        Ok(Self {
            handle: Arc::new(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) }),
        })
    }

    /// Server side: force-disconnects the client without closing our handle
    /// (no double-close risk across clones); pending reads complete broken.
    pub fn disconnect(&self) {
        unsafe { DisconnectNamedPipe(self.raw()) };
    }

    fn overlapped_io(
        &self,
        buf_len: usize,
        start: impl FnOnce(*mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        let ev = Event::new()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.raw();
        let ok = start(&mut ov);
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            if err != ERROR_IO_PENDING {
                return Err(io::Error::from_raw_os_error(err as i32));
            }
            unsafe { WaitForSingleObject(ov.hEvent, INFINITE) };
        }
        let mut transferred: u32 = 0;
        let ok = unsafe { GetOverlappedResult(self.raw(), &ov, &mut transferred, 1) };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_BROKEN_PIPE {
                return Ok(0);
            }
            return Err(io::Error::from_raw_os_error(err as i32));
        }
        debug_assert!(transferred as usize <= buf_len);
        Ok(transferred as usize)
    }
}

impl Read for PipeStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let raw = self.raw();
        self.overlapped_io(buf.len(), |ov| unsafe {
            ReadFile(
                raw,
                buf.as_mut_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
                ov,
            )
        })
    }
}

impl Write for PipeStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let raw = self.raw();
        self.overlapped_io(buf.len(), |ov| unsafe {
            WriteFile(
                raw,
                buf.as_ptr(),
                buf.len() as u32,
                std::ptr::null_mut(),
                ov,
            )
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Listener for one pipe name: creates instances, accepts with a 2-wait on
/// (connect, stop) so SCM stop / Ctrl+C interrupts a quiet accept.
pub struct PipeListener {
    path_w: Vec<u16>,
    instances: u32,
    first_created: bool,
    /// Explicit descriptor (security::PipeSecurity). None = process default
    /// (console/test mode only — the installed service always sets one).
    security: Option<crate::security::PipeSecurity>,
}

pub enum Accepted {
    Connection(PipeStream),
    Stopped,
}

impl PipeListener {
    pub fn new(
        path: &str,
        instances: u32,
        security: Option<crate::security::PipeSecurity>,
    ) -> Self {
        Self {
            path_w: wide(path),
            instances,
            first_created: false,
            security,
        }
    }

    /// Creates the next server instance and waits for a client or the stop
    /// event. The first instance carries FILE_FLAG_FIRST_PIPE_INSTANCE —
    /// and only the first (a second flagged instance would fail against our
    /// own; docs/SECURITY.md 脅威4).
    pub fn accept(&mut self, stop: &Event) -> io::Result<Accepted> {
        let first_flag = if self.first_created {
            0
        } else {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        };
        let attrs = self.security.as_ref().map(|s| s.attributes());
        let h = unsafe {
            CreateNamedPipeW(
                self.path_w.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first_flag,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                self.instances,
                BUFFER_SIZE,
                BUFFER_SIZE,
                0,
                attrs
                    .as_ref()
                    .map_or(std::ptr::null(), |a| a as *const SECURITY_ATTRIBUTES),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        self.first_created = true;
        let stream = PipeStream {
            handle: Arc::new(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) }),
        };

        let ev = Event::new()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.raw();
        let ok = unsafe { ConnectNamedPipe(h, &mut ov) };
        if ok == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => return Ok(Accepted::Connection(stream)),
                ERROR_IO_PENDING => {}
                err => return Err(io::Error::from_raw_os_error(err as i32)),
            }
            let handles = [ev.raw(), stop.raw()];
            let waited = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
            if waited != WAIT_OBJECT_0 {
                // Stop (or wait failure). Close the instance to cancel the
                // pending connect, then wait for the kernel to finish with
                // the stack-held OVERLAPPED before it goes out of scope.
                drop(stream);
                unsafe { WaitForSingleObject(ev.raw(), INFINITE) };
                return Ok(Accepted::Stopped);
            }
            let mut transferred = 0u32;
            let ok = unsafe { GetOverlappedResult(h, &ov, &mut transferred, 0) };
            if ok == 0 {
                return Err(last_error());
            }
        }
        Ok(Accepted::Connection(stream))
    }
}
