//! Pipe DACL and token checks (docs/SECURITY.md の4層防御の①と④、判断は
//! ADR-0017).
//!
//! The SDDL string is built by one pure, unit-pinned function — a hand-rolled
//! SDDL elsewhere is exactly the "silently wide open" accident the pin exists
//! to prevent. Never create a pipe without going through
//! `pipe_security_attributes`.

use std::io;

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

fn last_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

/// `D:P(A;;GA;;;SY)(A;;GRGW;;;<sid>)…` — SYSTEM gets full control, each
/// authorized SID read+write, nobody else (protected DACL, no inheritance,
/// no Everyone/anonymous ACE → default deny).
///
/// Administrators is deliberately absent: a UAC-filtered token carries it
/// deny-only and would not gain access anyway (docs/RESEARCH.md).
#[must_use]
pub fn pipe_sddl(authorized_sids: &[String]) -> String {
    let mut s = String::from("D:P(A;;GA;;;SY)");
    for sid in authorized_sids {
        s.push_str("(A;;GRGW;;;");
        s.push_str(sid);
        s.push(')');
    }
    s
}

/// Owns the security descriptor `LocalAlloc`'d by the SDDL conversion; the
/// `SECURITY_ATTRIBUTES` it hands out stays valid for its lifetime.
pub struct PipeSecurity {
    descriptor: *mut core::ffi::c_void,
}

// The descriptor is an opaque, immutable blob after creation.
unsafe impl Send for PipeSecurity {}
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// # Errors
    /// Returns the OS error if the SDDL string fails to convert to a security
    /// descriptor (`ConvertStringSecurityDescriptorToSecurityDescriptorW`).
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain([0]).collect();
        let mut descriptor: *mut core::ffi::c_void = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                (&raw mut descriptor).cast(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(Self { descriptor })
    }

    #[must_use]
    pub const fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor) };
    }
}

/// The current process token's user SID as a string ("S-1-5-21-…") —
/// `install` captures the installing user this way (docs/SECURITY.md 脅威1).
///
/// # Errors
/// Returns the OS error if opening the process token, querying its user, or
/// stringifying the SID fails.
pub fn current_user_sid() -> io::Result<String> {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) == 0 {
            return Err(last_error());
        }
        let token = OwnedToken(token);

        let mut needed = 0u32;
        GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
        let mut buf = vec![0u8; needed as usize];
        if GetTokenInformation(
            token.0,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        ) == 0
        {
            return Err(last_error());
        }
        // TOKEN_USER's first field is a PSID (pointer, 8-byte aligned), but the
        // Vec<u8> backing `buf` is only byte-aligned — forming a `&TOKEN_USER` to it
        // would be a misaligned reference (UB). Read the value out unaligned instead;
        // its `Sid` still points into `buf`, which outlives this use.
        let user = std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>());
        sid_to_string(user.User.Sid)
    }
}

/// Does `sid_str` name a real *user* account on this machine?
///
/// `install` uses it to vet a forwarded `--owner-sid` before trusting it onto
/// the pipe allowlist (docs/SECURITY.md 脅威1/7): a SID that resolves to
/// nothing — or to a group / well-known principal (SYSTEM, BUILTIN\Users…)
/// — is refused. Malformed/unresolvable → `Ok(false)`.
///
/// # Errors
/// Only genuine API faults are `Err` (the caller logs and ignores either way
/// — install must not die on a bad SID).
pub fn validate_user_sid(sid_str: &str) -> io::Result<bool> {
    use windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows_sys::Win32::Security::{LookupAccountSidW, PSID, SID_NAME_USE, SidTypeUser};

    // ConvertStringSidToSidW LocalAlloc's the SID — free it on every path.
    struct LocalSid(PSID);
    impl Drop for LocalSid {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0.cast()) };
        }
    }

    let wide: Vec<u16> = sid_str.encode_utf16().chain([0]).collect();
    let mut psid: PSID = std::ptr::null_mut();
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &raw mut psid) } == 0 {
        return Ok(false); // not even a well-formed SID string
    }
    let owned = LocalSid(psid);

    // First call sizes the name/domain buffers; a SID that maps to no
    // account leaves them at zero (ERROR_NONE_MAPPED).
    let mut name_len = 0u32;
    let mut domain_len = 0u32;
    let mut use_kind: SID_NAME_USE = 0;
    unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            owned.0,
            std::ptr::null_mut(),
            &raw mut name_len,
            std::ptr::null_mut(),
            &raw mut domain_len,
            &raw mut use_kind,
        );
    }
    if name_len == 0 {
        return Ok(false); // unresolvable → not a real account
    }
    let mut name = vec![0u16; name_len as usize];
    let mut domain = vec![0u16; domain_len as usize];
    let ok = unsafe {
        LookupAccountSidW(
            std::ptr::null(),
            owned.0,
            name.as_mut_ptr(),
            &raw mut name_len,
            domain.as_mut_ptr(),
            &raw mut domain_len,
            &raw mut use_kind,
        )
    };
    if ok == 0 {
        return Ok(false);
    }
    Ok(use_kind == SidTypeUser)
}

struct OwnedToken(HANDLE);

impl Drop for OwnedToken {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

unsafe fn sid_to_string(sid: windows_sys::Win32::Security::PSID) -> io::Result<String> {
    let mut out: *mut u16 = std::ptr::null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &raw mut out) } == 0 {
        return Err(last_error());
    }
    let mut len = 0;
    while unsafe { *out.add(len) } != 0 {
        len += 1;
    }
    let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
    unsafe { LocalFree(out.cast()) };
    Ok(s)
}

/// Protected DACL for the data root: SYSTEM + Administrators only. The
/// snapshots inside hold every file name on the machine (SECURITY.md 脅威7).
#[must_use]
pub fn data_dir_sddl() -> String {
    "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)".to_string()
}

/// logs/ keeps user read so the unelevated F12 "診断情報をコピー" can tail
/// engine.log.
///
/// Each authorized user (the installing admin *and* a forwarded owner SID
/// under OTS elevation) gets read, so the daily user is never locked out of
/// its own diagnostics.
#[must_use]
pub fn logs_dir_sddl(user_sids: &[&str]) -> String {
    let mut s = String::from("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    for sid in user_sids {
        s.push_str("(A;OICI;GR;;;");
        s.push_str(sid);
        s.push(')');
    }
    s
}

/// The protected DACLs `install` applies across the data tree.
///
/// Returned as `(subdir, sddl)` pairs (`""` = the data root). Centralized here so
/// the 脅威7 invariant — `index/` (machine-wide file-name snapshots) is
/// SYSTEM+Administrators only, never world-readable — is unit-pinned next to the
/// SDDL builders, without needing an elevated install to verify it. `install`
/// applies `index/` EXPLICITLY rather than relying on inheritance: it is created
/// inheriting `%ProgramData%`'s Users ACE, and `set_dir_dacl`'s `SetFileSecurityW`
/// does not re-propagate the root DACL onto an already-existing child.
#[must_use]
pub fn data_tree_dacls(log_readers: &[&str]) -> Vec<(&'static str, String)> {
    vec![
        ("", data_dir_sddl()),
        ("index", data_dir_sddl()),
        ("logs", logs_dir_sddl(log_readers)),
    ]
}

/// Applies an SDDL-described protected DACL to a directory (install-time).
///
/// # Errors
/// Returns the OS error if the SDDL fails to convert or `SetFileSecurityW`
/// fails.
pub fn set_dir_dacl(path: &std::path::Path, sddl: &str) -> io::Result<()> {
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, SetFileSecurityW};

    let sec = PipeSecurity::from_sddl(sddl)?;
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain([0])
        .collect();
    let ok = unsafe { SetFileSecurityW(wide.as_ptr(), DACL_SECURITY_INFORMATION, sec.descriptor) };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Connect-time token check — defense in depth behind the DACL (a DACL
/// construction bug must not become full exposure). Empty `authorized` =
/// check disabled (console/test mode).
///
/// # Errors
/// Returns the OS error if impersonating the pipe client or reading its token
/// fails. A successfully read token that is not authorized returns `Ok(false)`.
pub fn verify_client(pipe: &crate::pipe::PipeStream, authorized: &[String]) -> io::Result<bool> {
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;
    use windows_sys::Win32::System::Threading::{GetCurrentThread, OpenThreadToken};

    if authorized.is_empty() {
        return Ok(true);
    }
    unsafe {
        if ImpersonateNamedPipeClient(pipe.raw()) == 0 {
            return Err(last_error());
        }
    }
    // From here on we *must* revert — the closure scopes the impersonation.
    let result = (|| {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &raw mut token) == 0 {
                return Err(last_error());
            }
            let token = OwnedToken(token);
            let mut needed = 0u32;
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
            let mut buf = vec![0u8; needed as usize];
            if GetTokenInformation(
                token.0,
                TokenUser,
                buf.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            ) == 0
            {
                return Err(last_error());
            }
            // See current_user_sid: read TOKEN_USER out unaligned (its leading PSID
            // wants 8-byte alignment; the Vec<u8> is byte-aligned) so we never form a
            // misaligned reference. `buf` outlives the Sid read below.
            let user = std::ptr::read_unaligned(buf.as_ptr().cast::<TOKEN_USER>());
            let sid = sid_to_string(user.User.Sid)?;
            Ok(sid == "S-1-5-18" /* SYSTEM (self-connections) */
                || authorized.iter().any(|a| a == &sid))
        }
    })();
    unsafe {
        windows_sys::Win32::Security::RevertToSelf();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sddl_structure_is_pinned() {
        // Protected DACL, SYSTEM full control, per-SID read+write, nothing
        // else — the literal shape SECURITY.md documents.
        assert_eq!(pipe_sddl(&[]), "D:P(A;;GA;;;SY)");
        assert_eq!(
            pipe_sddl(&["S-1-5-21-1-2-3-1001".to_string()]),
            "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-5-21-1-2-3-1001)"
        );
        let two = pipe_sddl(&["S-1-1-1".to_string(), "S-1-2-2".to_string()]);
        assert_eq!(two, "D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-1-1)(A;;GRGW;;;S-1-2-2)");
        assert!(!two.contains(";;;WD)"), "no Everyone ACE, ever");
        assert!(!two.contains(";;;AU)"), "no Authenticated Users ACE, ever");
        assert!(
            !two.contains(";;;BA)"),
            "no Administrators ACE (deny-only under UAC)"
        );
    }

    #[test]
    fn sddl_converts_and_user_sid_resolves() {
        // Conversion exercises the real API (unelevated-safe).
        let sec = PipeSecurity::from_sddl(&pipe_sddl(&["S-1-5-32-545".to_string()]))
            .expect("valid SDDL converts");
        assert!(!sec.attributes().lpSecurityDescriptor.is_null());

        let sid = current_user_sid().expect("own token is readable");
        assert!(sid.starts_with("S-1-"), "stringified SID: {sid}");
        // The full loop: a captured SID round-trips through the builder.
        PipeSecurity::from_sddl(&pipe_sddl(&[sid])).expect("captured SID is SDDL-legal");
    }

    #[test]
    fn validate_user_sid_accepts_self() {
        // The process's own token is a real user account.
        let sid = current_user_sid().expect("own sid");
        assert!(validate_user_sid(&sid).expect("validate own sid"));
    }

    #[test]
    fn validate_user_sid_rejects_system_and_garbage() {
        // SYSTEM resolves but is a well-known group, not a user.
        assert!(!validate_user_sid("S-1-5-18").expect("validate SYSTEM"));
        // A syntactically valid but unmapped local SID.
        assert!(
            !validate_user_sid("S-1-5-21-1111111111-2222222222-3333333333-4444")
                .expect("validate unmapped")
        );
        // Not even a SID string.
        assert!(!validate_user_sid("not-a-sid").expect("validate garbage"));
    }

    #[test]
    fn logs_dir_sddl_grants_read_per_user() {
        let one = logs_dir_sddl(&["S-1-5-21-1-2-3-1001"]);
        assert!(one.contains("(A;OICI;FA;;;SY)"), "SYSTEM full control");
        assert!(
            one.contains("(A;OICI;FA;;;BA)"),
            "Administrators full control"
        );
        assert!(
            one.contains("(A;OICI;GR;;;S-1-5-21-1-2-3-1001)"),
            "user read"
        );
        let two = logs_dir_sddl(&["S-1-1-1", "S-1-2-2"]);
        assert!(two.contains("(A;OICI;GR;;;S-1-1-1)"));
        assert!(two.contains("(A;OICI;GR;;;S-1-2-2)"));
    }

    #[test]
    fn data_tree_hardens_index_like_root_with_no_users() {
        let t = data_tree_dacls(&["S-1-5-21-1-2-3-1001"]);
        let find = |k: &str| t.iter().find(|(s, _)| *s == k).map(|(_, v)| v.clone());
        // 脅威7: index/ — machine-wide file-name snapshots — gets the SAME
        // protected SYSTEM+Admins-only DACL as the data root. Regressing this
        // (e.g. dropping the explicit index/ hardening) re-exposes every file
        // name on the machine to any local user.
        assert_eq!(find("index").as_deref(), Some(data_dir_sddl().as_str()));
        assert_eq!(find("").as_deref(), Some(data_dir_sddl().as_str()));
        let index = find("index").expect("index/ must be in the hardened tree");
        for forbidden in [";;;WD)", ";;;AU)", ";;;BU)"] {
            assert!(
                !index.contains(forbidden),
                "index/ must not grant {forbidden}"
            );
        }
        // logs/ additionally grants the per-user read ACE for the F12 copy path.
        assert!(
            find("logs")
                .unwrap()
                .contains("(A;OICI;GR;;;S-1-5-21-1-2-3-1001)")
        );
    }
}
