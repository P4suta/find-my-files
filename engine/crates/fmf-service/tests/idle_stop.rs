//! Non-elevated behavioural test for the on-demand idle self-stop (ADR-0027):
//! a console `fmf-service run --no-index` with a 1s idle timeout exits on its
//! own shortly after its only client disconnects. No elevation, no real scan —
//! it exercises the real `serve()` park loop in a child process.

use std::io::Write as _;
use std::time::{Duration, Instant};

use fmf_proto::PROTOCOL_VERSION;
use fmf_proto::frame::{FrameHeader, read_frame, write_frame};
use fmf_proto::messages::{self, opcode};
use fmf_service::pipe::PipeStream;

fn connect_with_retry(pipe_name: &str, deadline: Duration) -> PipeStream {
    let begin = Instant::now();
    loop {
        if let Ok(s) = PipeStream::connect(pipe_name) {
            return s;
        }
        assert!(begin.elapsed() < deadline, "pipe never came up");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn idle_stop_exits_after_last_client_disconnects() {
    let data_dir = fmf_core::index::testutil::TestDir::new();
    // 1s idle timeout; no volumes scanned (no_index below).
    let mut f = std::fs::File::create(data_dir.join("service.json")).unwrap();
    f.write_all(br#"{ "idle_stop_secs": 1, "flush_interval_secs": 10 }"#)
        .unwrap();
    drop(f);
    let pipe_name = format!(r"\\.\pipe\fmf-idle-{}", std::process::id());

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fmf-service"))
        .args([
            "run",
            "--no-index",
            "--pipe-name",
            &pipe_name,
            "--data-dir",
            &data_dir.path().to_string_lossy(),
        ])
        .spawn()
        .expect("spawn fmf-service run");

    // Connect, say Hello, and hold the connection long enough for the serve()
    // park loop (200ms tick) to observe a live client, then disconnect.
    {
        let mut s = connect_with_retry(&pipe_name, Duration::from_secs(10));
        write_frame(
            &mut s,
            FrameHeader {
                len: 0,
                opcode: opcode::HELLO,
                flags: 0,
                request_id: 1,
                status: 0,
            },
            &messages::HelloReq {
                protocol_version: PROTOCOL_VERSION,
            }
            .encode(),
        )
        .expect("write hello");
        let _ = read_frame(&mut s).expect("hello reply");
        std::thread::sleep(Duration::from_millis(700));
    } // stream dropped → server sees the disconnect → the idle clock starts

    // It should self-stop within the 1s idle timeout plus generous slack.
    let begin = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            assert!(
                status.success(),
                "idle stop should be a clean exit: {status:?}"
            );
            return;
        }
        assert!(
            begin.elapsed() < Duration::from_secs(30),
            "service did not idle-stop within 30s of the client leaving"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
