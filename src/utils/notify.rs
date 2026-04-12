//! Best-effort desktop notifications.
//!
//! Uses `osascript` on macOS and `notify-send` on Linux.
//! Errors are silently ignored — notifications must never break the main flow.

use anyhow::Result;

/// Send a notification and return the original result unchanged.
///
/// Typical usage:
/// ```ignore
/// notify_result(ctx.rt.compose_run(&args, &msg), &project, "build")
/// ```
pub fn notify_result(result: Result<()>, project: &str, operation: &str) -> Result<()> {
    let body = match &result {
        Ok(_) => format!("{project} — {operation} complete"),
        Err(_) => format!("{project} — {operation} failed"),
    };
    send("dip", &body);
    result
}

/// Fire-and-forget desktop notification. Never blocks, never fails visibly.
pub fn send(title: &str, body: &str) {
    #[cfg(target_os = "macos")]
    {
        let script = format!("display notification {body:?} with title {title:?}");
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args([title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}
