//! Elevated: the GC Scheduled Task document must be one the registrar accepts.
//! Gated like every machine-mutating test: `#[ignore]` + `FMF_ADMIN_TESTS=1`
//! (`just test-admin`, elevated).

use std::path::PathBuf;
use std::process::Command;

/// Fails closed. `#[ignore]` is what *skips* this test; reaching the body
/// without the arming variable means the harness was invoked outside
/// `just test-admin`, and a silent early return would be indistinguishable
/// from a registration that actually succeeded.
fn require_admin_gate() {
    assert_eq!(
        std::env::var("FMF_ADMIN_TESTS").as_deref(),
        Ok("1"),
        "this ignored machine-security test must run only through `just test-admin`"
    );
}

fn schtasks() -> PathBuf {
    // Known-Folder-resolved absolute path, never a PATH search — same rule the
    // installer follows for this binary.
    PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot is set"))
        .join("System32")
        .join("schtasks.exe")
}

/// Deletes the probe task on the way out, including during panic unwinding.
struct TaskGuard(String);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        let _ = Command::new(schtasks())
            .args(["/Delete", "/TN", &self.0, "/F"])
            .output();
    }
}

/// The unit test can only assert that some string is present in the document.
/// That is exactly how `<RequiredPrivileges>` — an `IPrincipal2` property with
/// no place in the schema `schtasks` validates against — stayed in the template
/// while every real install failed at task registration. Only registering the
/// document proves it is accepted.
#[test]
#[ignore = "registers a real Scheduled Task; gated by FMF_ADMIN_TESTS=1 (just test-admin)"]
fn gc_task_xml_registers_with_schtasks() {
    require_admin_gate();

    let dir = tempfile::tempdir().expect("temp dir");
    let xml_path = dir.path().join("gc-task.xml");
    // The stable installed path is what production registers; the task is
    // deleted before anything could run it.
    let stable_exe = std::path::Path::new(r"C:\ProgramData\find-my-files\fmf-service.exe");
    std::fs::write(&xml_path, fmf_service::lifecycle::gc_task_xml(stable_exe)).expect("write xml");

    let name = format!("fmf-gc-xml-probe-{}", std::process::id());
    let guard = TaskGuard(name.clone());
    let output = Command::new(schtasks())
        .args([
            "/Create",
            "/TN",
            &name,
            "/XML",
            &xml_path.to_string_lossy(),
            "/F",
        ])
        .output()
        .expect("run schtasks");

    assert!(
        output.status.success(),
        "schtasks rejected the GC task document (exit {:?})\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
}
