//! Overlapped named-pipe I/O behind blocking `Read`/`Write` so the frame
//! codec (fmf-proto) works unchanged.
//!
//! The pipe is created OVERLAPPED solely so the accept loop can wait on
//! (connect, stop) at once; data I/O issues an overlapped op and immediately
//! waits on its per-call event — blocking semantics, cancel-safe via
//! `CloseHandle` (pending ops fail, threads exit).

use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::sync::{Arc, OnceLock};

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
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
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
    /// # Errors
    /// Returns the OS error if `CreateEventW` fails.
    pub fn new() -> io::Result<Self> {
        let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if h.is_null() {
            return Err(last_error());
        }
        Ok(Self(unsafe {
            OwnedHandle::from_raw_handle(h as RawHandle)
        }))
    }

    /// Signals the event, waking any thread waiting on it (used to break a
    /// quiet accept loop on SCM stop / Ctrl+C).
    pub fn set(&self) {
        unsafe { SetEvent(self.0.as_raw_handle() as HANDLE) };
    }

    /// Clears the signaled state before reusing the event for a new overlapped
    /// op: a synchronous completion leaves an auto-reset event signaled (the
    /// wait that would have reset it never happened), so a reused event must be
    /// reset or the next wait returns prematurely.
    fn reset(&self) {
        unsafe { ResetEvent(self.0.as_raw_handle() as HANDLE) };
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

/// One duplex pipe endpoint. Cloning shares the OS handle; reads and writes
/// may run on different threads (independent OVERLAPPED + events).
pub struct PipeStream {
    handle: Arc<OwnedHandle>,
    /// This clone's own auto-reset event for overlapped I/O, created on first
    /// use. Reused across every read/write on this clone instead of a fresh
    /// `CreateEventW`/`CloseHandle` pair per op. Created lazily because `Clone`
    /// is infallible and `CreateEventW` is not; each clone gets a *separate*
    /// event (reads and writes run on different threads and must not share one
    /// — they would cross each other's waits). `OnceLock` keeps the type
    /// `Send + Sync` and the per-clone access is single-role (a reader owns its
    /// clone; writes are serialized under a `Mutex`, server.rs).
    io_event: OnceLock<Event>,
}

impl Clone for PipeStream {
    fn clone(&self) -> Self {
        Self {
            handle: Arc::clone(&self.handle),
            // A fresh, independent event for the clone — NOT a shared one.
            io_event: OnceLock::new(),
        }
    }
}

impl PipeStream {
    pub(crate) fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    /// This clone's reusable overlapped-I/O event, created on first use.
    fn io_event(&self) -> io::Result<&Event> {
        if let Some(ev) = self.io_event.get() {
            return Ok(ev);
        }
        // First I/O on this clone: create the event. `set` only fails if a
        // concurrent caller won the race (there is none in practice — single
        // role per clone), in which case `get_or_init` returns the winner and
        // our `created` drops; no fallible closure or `unwrap` needed.
        let created = Event::new()?;
        Ok(self.io_event.get_or_init(|| created))
    }

    /// Client side: opens an existing pipe (blocking I/O is fine here, but
    /// we open OVERLAPPED for symmetry with the I/O helpers).
    ///
    /// # Errors
    /// Returns the OS error if `CreateFileW` fails (e.g. the pipe does not
    /// exist or the caller is not authorized).
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
            io_event: OnceLock::new(),
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
        let ev = self.io_event()?;
        // Reused event: clear any leftover signal from a prior synchronous
        // completion before issuing the next op (see Event::reset).
        ev.reset();
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.raw();
        let ok = start(&raw mut ov);
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
        let ok = unsafe { GetOverlappedResult(self.raw(), &raw const ov, &raw mut transferred, 1) };
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
    /// Explicit descriptor (`security::PipeSecurity`). None = process default
    /// (console/test mode only — the installed service always sets one).
    security: Option<crate::security::PipeSecurity>,
}

/// Outcome of one `accept`: either a connected client stream or a stop signal.
pub enum Accepted {
    /// A client connected; carries the duplex pipe endpoint.
    Connection(PipeStream),
    /// The stop event fired before any client connected; the accept loop exits.
    Stopped,
}

#[derive(Clone, Copy)]
enum RemoteClients {
    Reject,
    #[cfg(test)]
    AcceptForTest,
}

impl RemoteClients {
    const fn pipe_mode(self) -> u32 {
        match self {
            Self::Reject => PIPE_REJECT_REMOTE_CLIENTS,
            #[cfg(test)]
            Self::AcceptForTest => windows_sys::Win32::System::Pipes::PIPE_ACCEPT_REMOTE_CLIENTS,
        }
    }
}

impl PipeListener {
    /// Creates a listener for `path` allowing up to `instances` concurrent
    /// pipe instances; `security` is the explicit descriptor (None = process
    /// default, console/test only — the installed service always sets one).
    #[must_use]
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
    /// event. The first instance carries `FILE_FLAG_FIRST_PIPE_INSTANCE` —
    /// and only the first (a second flagged instance would fail against our
    /// own; docs/SECURITY.md threat 4).
    ///
    /// # Errors
    /// Returns the OS error if `CreateNamedPipeW`, the stop-event creation, or
    /// the connect wait fails.
    pub fn accept(&mut self, stop: &Event, on_listening: impl FnOnce()) -> io::Result<Accepted> {
        self.accept_with_remote_policy(stop, on_listening, RemoteClients::Reject)
    }

    #[cfg(test)]
    fn accept_remote_for_test(
        &mut self,
        stop: &Event,
        on_listening: impl FnOnce(),
    ) -> io::Result<Accepted> {
        self.accept_with_remote_policy(stop, on_listening, RemoteClients::AcceptForTest)
    }

    fn accept_with_remote_policy(
        &mut self,
        stop: &Event,
        on_listening: impl FnOnce(),
        remote_clients: RemoteClients,
    ) -> io::Result<Accepted> {
        let first_flag = if self.first_created {
            0
        } else {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        };
        let attrs = self
            .security
            .as_ref()
            .map(super::security::PipeSecurity::attributes);
        let h = unsafe {
            CreateNamedPipeW(
                self.path_w.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED | first_flag,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | remote_clients.pipe_mode(),
                self.instances,
                BUFFER_SIZE,
                BUFFER_SIZE,
                0,
                attrs
                    .as_ref()
                    .map_or(std::ptr::null(), std::ptr::from_ref::<SECURITY_ATTRIBUTES>),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        self.first_created = true;
        let stream = PipeStream {
            handle: Arc::new(unsafe { OwnedHandle::from_raw_handle(h as RawHandle) }),
            io_event: OnceLock::new(),
        };

        let ev = Event::new()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.raw();
        let ok = unsafe { ConnectNamedPipe(h, &raw mut ov) };
        if ok == 0 {
            match unsafe { GetLastError() } {
                ERROR_PIPE_CONNECTED => {
                    on_listening();
                    return Ok(Accepted::Connection(stream));
                }
                ERROR_IO_PENDING => on_listening(),
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
            let ok = unsafe { GetOverlappedResult(h, &raw const ov, &raw mut transferred, 0) };
            if ok == 0 {
                return Err(last_error());
            }
        } else {
            on_listening();
        }
        Ok(Accepted::Connection(stream))
    }
}

#[cfg(test)]
mod admin_security_tests {
    use std::io::{self, Write as _};
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, GetLastError, LocalFree,
    };
    use windows_sys::Win32::NetworkManagement::NetManagement::{
        NERR_Success, NERR_UserExists, NERR_UserNotFound, NetUserAdd, NetUserDel,
        UF_NORMAL_ACCOUNT, UF_SCRIPT, USER_INFO_1, USER_PRIV_USER,
    };
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, ImpersonateLoggedOnUser, LOGON32_LOGON_INTERACTIVE,
        LOGON32_PROVIDER_DEFAULT, LogonUserW, RevertToSelf, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        GetComputerNameW, MAX_COMPUTERNAME_LENGTH,
    };

    use super::{Accepted, Event, PipeListener, PipeStream};
    use crate::security::{PipeSecurity, current_user_sid, pipe_sddl, verify_client};

    const READY_TIMEOUT: Duration = Duration::from_secs(10);
    static PIPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn require_admin_gate() {
        assert_eq!(
            std::env::var("FMF_ADMIN_TESTS").as_deref(),
            Ok("1"),
            "this ignored machine-security test must run only through `just test-admin`"
        );
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain([0]).collect()
    }

    fn random_bytes<const N: usize>() -> [u8; N] {
        let mut bytes = [0u8; N];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                N as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        assert_eq!(status, 0, "BCryptGenRandom failed with NTSTATUS {status}");
        bytes
    }

    fn lowercase_hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        output
    }

    fn secret_password_wide() -> Vec<u16> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut random = random_bytes::<16>();
        let mut password: Vec<u16> = "Fmf!9aZ-".encode_utf16().collect();
        password.reserve(random.len() * 2 + 1);
        for byte in &random {
            password.push(u16::from(HEX[usize::from(byte >> 4)]));
            password.push(u16::from(HEX[usize::from(byte & 0x0f)]));
        }
        random.fill(0);
        password.push(0);
        password
    }

    fn computer_name() -> String {
        let mut buffer = vec![0u16; MAX_COMPUTERNAME_LENGTH as usize + 1];
        let mut length = buffer.len() as u32;
        let ok = unsafe { GetComputerNameW(buffer.as_mut_ptr(), &raw mut length) };
        assert_ne!(
            ok,
            0,
            "GetComputerNameW failed: {}",
            io::Error::last_os_error()
        );
        String::from_utf16(&buffer[..length as usize]).expect("computer name is valid UTF-16")
    }

    fn unique_pipe_paths(tag: &str) -> (String, String) {
        let sequence = PIPE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = lowercase_hex(&random_bytes::<4>());
        let short = format!(
            "fmf-admin-{tag}-{:08x}-{sequence:016x}-{nonce}",
            std::process::id()
        );
        (
            format!(r"\\.\pipe\{short}"),
            format!(r"\\{}\pipe\{short}", computer_name()),
        )
    }

    struct TemporaryLocalUser {
        name_w: Vec<u16>,
        password_w: Vec<u16>,
        deleted: bool,
    }

    impl TemporaryLocalUser {
        fn create() -> Self {
            for _ in 0..8 {
                // Twenty characters keeps compatibility with every supported
                // local-account API while PID + CSPRNG prevents collisions
                // across parallel/retried test processes.
                let name = format!(
                    "fmft{:08x}{}",
                    std::process::id(),
                    lowercase_hex(&random_bytes::<4>())
                );
                let mut name_w = wide(&name);
                let mut password_w = secret_password_wide();
                let info = USER_INFO_1 {
                    usri1_name: name_w.as_mut_ptr(),
                    usri1_password: password_w.as_mut_ptr(),
                    usri1_password_age: 0,
                    usri1_priv: USER_PRIV_USER,
                    usri1_home_dir: std::ptr::null_mut(),
                    usri1_comment: std::ptr::null_mut(),
                    usri1_flags: UF_SCRIPT | UF_NORMAL_ACCOUNT,
                    usri1_script_path: std::ptr::null_mut(),
                };
                let mut parameter_error = 0u32;
                let status = unsafe {
                    NetUserAdd(
                        std::ptr::null(),
                        1,
                        (&raw const info).cast(),
                        &raw mut parameter_error,
                    )
                };
                if status == NERR_Success {
                    return Self {
                        name_w,
                        password_w,
                        deleted: false,
                    };
                }
                password_w.fill(0);
                assert!(
                    status == NERR_UserExists,
                    "NetUserAdd failed with status {status} at USER_INFO_1 field {parameter_error}"
                );
            }
            panic!("could not allocate a unique ephemeral local-account name");
        }

        fn logon(&mut self) -> OwnedHandle {
            let domain_w = wide(&computer_name());
            let mut token = std::ptr::null_mut();
            let ok = unsafe {
                LogonUserW(
                    self.name_w.as_ptr(),
                    domain_w.as_ptr(),
                    self.password_w.as_ptr(),
                    LOGON32_LOGON_INTERACTIVE,
                    LOGON32_PROVIDER_DEFAULT,
                    &raw mut token,
                )
            };
            self.password_w.fill(0);
            assert_ne!(
                ok,
                0,
                "LogonUserW failed for the ephemeral standard user: {}",
                io::Error::last_os_error()
            );
            unsafe { OwnedHandle::from_raw_handle(token.cast()) }
        }

        fn delete_inner(&mut self) -> u32 {
            if self.deleted {
                return NERR_Success;
            }
            let status = unsafe { NetUserDel(std::ptr::null(), self.name_w.as_ptr()) };
            if status == NERR_Success || status == NERR_UserNotFound {
                self.deleted = true;
            }
            self.password_w.fill(0);
            status
        }

        fn remove(mut self) {
            let status = self.delete_inner();
            assert!(
                status == NERR_Success || status == NERR_UserNotFound,
                "NetUserDel failed with status {status}"
            );
        }
    }

    impl Drop for TemporaryLocalUser {
        fn drop(&mut self) {
            let status = self.delete_inner();
            assert!(
                thread::panicking() || status == NERR_Success || status == NERR_UserNotFound,
                "NetUserDel failed with status {status}"
            );
        }
    }

    fn token_user_sid(token: &OwnedHandle) -> String {
        let mut needed = 0u32;
        let first = unsafe {
            GetTokenInformation(
                token.as_raw_handle().cast(),
                TokenUser,
                std::ptr::null_mut(),
                0,
                &raw mut needed,
            )
        };
        assert_eq!(first, 0, "TokenUser size query unexpectedly succeeded");
        assert_eq!(
            unsafe { GetLastError() },
            ERROR_INSUFFICIENT_BUFFER,
            "TokenUser size query failed unexpectedly"
        );
        assert!(needed >= size_of::<TOKEN_USER>() as u32);
        let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        let ok = unsafe {
            GetTokenInformation(
                token.as_raw_handle().cast(),
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        };
        assert_ne!(
            ok,
            0,
            "read ephemeral TokenUser: {}",
            io::Error::last_os_error()
        );
        let user = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
        let mut string_sid = std::ptr::null_mut();
        let converted = unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut string_sid) };
        assert_ne!(
            converted,
            0,
            "stringify ephemeral TokenUser SID: {}",
            io::Error::last_os_error()
        );
        let mut length = 0usize;
        while unsafe { *string_sid.add(length) } != 0 {
            length += 1;
        }
        let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(string_sid, length) })
            .expect("Windows returned a valid SID string");
        unsafe { LocalFree(string_sid.cast()) };
        sid
    }

    fn with_impersonated_user<T>(token: &OwnedHandle, operation: impl FnOnce() -> T) -> T {
        let impersonated = unsafe { ImpersonateLoggedOnUser(token.as_raw_handle().cast()) };
        assert_ne!(
            impersonated,
            0,
            "ImpersonateLoggedOnUser failed: {}",
            io::Error::last_os_error()
        );
        let outcome = catch_unwind(AssertUnwindSafe(operation));
        let reverted = unsafe { RevertToSelf() };
        assert_ne!(
            reverted,
            0,
            "RevertToSelf failed: {}",
            io::Error::last_os_error()
        );
        match outcome {
            Ok(value) => value,
            Err(payload) => resume_unwind(payload),
        }
    }

    type AcceptOutcome = io::Result<Option<bool>>;

    struct PendingAccept {
        stop: Arc<Event>,
        thread: Option<JoinHandle<AcceptOutcome>>,
    }

    impl PendingAccept {
        fn start(
            local_path: &str,
            sddl: &str,
            authorized_sids: Vec<String>,
            accept_remote_for_control: bool,
        ) -> Self {
            let security = PipeSecurity::from_sddl(sddl).expect("convert test pipe SDDL");
            let mut listener = PipeListener::new(local_path, 1, Some(security));
            let stop = Arc::new(Event::new().expect("create stop event"));
            let thread_stop = Arc::clone(&stop);
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("fmf-admin-pipe-security".to_string())
                .spawn(move || {
                    let accepted = if accept_remote_for_control {
                        listener.accept_remote_for_test(&thread_stop, || {
                            ready_tx.send(()).expect("publish pipe readiness");
                        })
                    } else {
                        listener.accept(&thread_stop, || {
                            ready_tx.send(()).expect("publish pipe readiness");
                        })
                    }?;
                    match accepted {
                        Accepted::Connection(stream) => {
                            let authorized = verify_client(&stream, &authorized_sids)?;
                            if !authorized {
                                stream.disconnect();
                            }
                            Ok(Some(authorized))
                        }
                        Accepted::Stopped => Ok(None),
                    }
                })
                .expect("spawn pipe security listener");

            match ready_rx.recv_timeout(READY_TIMEOUT) {
                Ok(()) => Self {
                    stop,
                    thread: Some(thread),
                },
                Err(error) => {
                    stop.set();
                    let outcome = thread.join();
                    panic!(
                        "pipe listener did not become ready within {READY_TIMEOUT:?}: \
                         {error}; thread outcome: {outcome:?}"
                    );
                }
            }
        }

        fn finish(mut self) -> Option<bool> {
            self.thread
                .take()
                .expect("accept thread is present")
                .join()
                .expect("accept thread did not panic")
                .expect("accept/verify client")
        }
    }

    impl Drop for PendingAccept {
        fn drop(&mut self) {
            self.stop.set();
            if let Some(thread) = self.thread.take() {
                let outcome = thread.join();
                if thread::panicking() {
                    if !matches!(outcome, Ok(Ok(None))) {
                        eprintln!("pipe accept cleanup during unwind returned {outcome:?}");
                    }
                } else {
                    assert!(
                        matches!(outcome, Ok(Ok(None))),
                        "cancelled accept cleanup did not stop cleanly: {outcome:?}"
                    );
                }
            }
        }
    }

    fn connect_as(token: &OwnedHandle, path: &str) -> io::Result<PipeStream> {
        with_impersonated_user(token, || PipeStream::connect(path))
    }

    #[test]
    #[ignore = "requires elevation and creates an ephemeral local standard user; gated by FMF_ADMIN_TESTS=1"]
    fn named_pipe_security_boundaries_are_enforced_on_real_tokens_and_transports() {
        require_admin_gate();

        let current_sid = current_user_sid().expect("current user SID");
        let mut temporary_user = TemporaryLocalUser::create();
        let other_token = temporary_user.logon();
        let other_sid = token_user_sid(&other_token);
        assert_ne!(
            current_sid, other_sid,
            "the adversarial connection must carry a genuinely different TokenUser SID"
        );

        // Layer 1: production SDDL denies the real second user at CreateFile,
        // while the ordinary authorized token reaches the very same pending
        // instance and passes the independent server-side SID check.
        let (dacl_local, _) = unique_pipe_paths("dacl");
        let production_sddl = pipe_sddl(std::slice::from_ref(&current_sid));
        let dacl_accept = PendingAccept::start(
            &dacl_local,
            &production_sddl,
            vec![current_sid.clone()],
            false,
        );
        let denied = connect_as(&other_token, &dacl_local);
        let Err(denied) = denied else {
            panic!("the production pipe DACL admitted a different local user");
        };
        assert_eq!(
            denied.raw_os_error(),
            Some(ERROR_ACCESS_DENIED as i32),
            "a different local user must be denied by the pipe DACL"
        );
        let authorized_client =
            PipeStream::connect(&dacl_local).expect("authorized same-pipe control");
        assert_eq!(
            dacl_accept.finish(),
            Some(true),
            "the authorized control must pass verify_client"
        );
        drop(authorized_client);

        // Layer 4 independently holds if layer 1 is accidentally widened:
        // Everyone may reach this test-only pipe at the kernel, but the real
        // client TokenUser SID is still rejected by verify_client and the
        // server disconnects it.
        let (token_local, _) = unique_pipe_paths("token");
        let deliberately_wide_sddl = "D:P(A;;GA;;;SY)(A;;GRGW;;;WD)";
        let token_accept = PendingAccept::start(
            &token_local,
            deliberately_wide_sddl,
            vec![current_sid],
            false,
        );
        let mut unauthorized_client = connect_as(&other_token, &token_local)
            .expect("wide test DACL must prove kernel-level connection first");
        assert_eq!(
            token_accept.finish(),
            Some(false),
            "verify_client must reject the different TokenUser SID"
        );
        assert!(
            unauthorized_client.write_all(b"rejected").is_err(),
            "verify_client rejection must disconnect the admitted kernel client"
        );
        drop(unauthorized_client);

        // Layer 2 is behavioral, not a constant/SDDL assertion. First prove
        // this host can traverse the remote named-pipe transport with the
        // accept control. Only then test the production reject mode under the
        // same identity, DACL, host, and API path. A broken SMB/control
        // environment is a hard failure, never a skip.
        let (remote_control_local, remote_control_unc) = unique_pipe_paths("remote-control");
        let remote_control = PendingAccept::start(
            &remote_control_local,
            deliberately_wide_sddl,
            Vec::new(),
            true,
        );
        let remote_client = PipeStream::connect(&remote_control_unc).unwrap_or_else(|error| {
            panic!(
                "PIPE_ACCEPT_REMOTE_CLIENTS control could not prove remote \
                     transport on this host: {error}"
            );
        });
        assert_eq!(
            remote_control.finish(),
            Some(true),
            "remote accept control must reach the server"
        );
        drop(remote_client);

        let (remote_reject_local, remote_reject_unc) = unique_pipe_paths("remote-reject");
        let remote_reject = PendingAccept::start(
            &remote_reject_local,
            deliberately_wide_sddl,
            Vec::new(),
            false,
        );
        let rejected = PipeStream::connect(&remote_reject_unc);
        let Err(rejected) = rejected else {
            panic!("PIPE_REJECT_REMOTE_CLIENTS admitted a remote transport");
        };
        assert_eq!(
            rejected.raw_os_error(),
            Some(ERROR_ACCESS_DENIED as i32),
            "remote rejection must be an explicit access denial"
        );
        let local_control = PipeStream::connect(&remote_reject_local)
            .expect("local control after remote rejection");
        assert_eq!(
            remote_reject.finish(),
            Some(true),
            "the production listener must remain usable by the local authorized user"
        );
        drop(local_control);

        drop(other_token);
        temporary_user.remove();
    }
}
