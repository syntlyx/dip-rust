//! Run `exec` against the Docker Engine API directly, without spawning the
//! docker CLI at all.
//!
//! The CLI's own startup is the single biggest cost in a `dip exec`: measured
//! against a live daemon (macOS, Docker 29.8), `docker exec <id> true` takes
//! ~26 ms, of which only ~13 ms is the daemon actually running the command.
//! Doing the three API calls ourselves — create, start, inspect — costs
//! ~14 ms, which puts `dip exec` below the bare CLI it wraps.
//!
//! Non-TTY only. Without a TTY the daemon multiplexes stdout and stderr into
//! one framed stream that we demultiplex back onto our own; a TTY exec would
//! additionally need raw terminal mode and `SIGWINCH` forwarding, so
//! `dip shell` stays on the CLI.
//!
//! Like the rest of the API fast path this is an optimization and never a
//! requirement — but only up to a point. Once the daemon has been told to
//! start the command, falling back would run it *again*, so failures are
//! reported as [`Attempt::Ran`] and go no further. Only a failure before that
//! point returns [`Attempt::NotStarted`], which the caller is free to retry
//! through the docker CLI.

use std::pin::Pin;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::docker_api::{ApiStream, Endpoint, connect};

/// Everything the Engine needs to create the exec, independent of how the
/// caller spelled it on a command line.
pub(super) struct ExecSpec<'a> {
    pub container_id: &'a str,
    pub command: Vec<String>,
    pub env: Vec<String>,
    pub user: Option<&'a str>,
    pub workdir: Option<&'a str>,
    pub privileged: bool,
    /// Forward our stdin to the command (compose/docker `-i`).
    pub interactive: bool,
}

/// What became of an exec attempt.
///
/// The distinction matters more than the exit code: falling back to another
/// route re-runs the command, and re-running `php artisan migrate` or
/// `composer install` because we lost the connection reading its output is
/// worse than any error we could report. Only `NotStarted` is safe to retry.
pub(super) enum Attempt {
    /// The daemon never got as far as running anything, so nothing happened
    /// and the caller is free to try another route.
    NotStarted(anyhow::Error),
    /// The command ran. Final however it turned out.
    Ran(Result<i32>),
}

/// Exit code convention for "the thing reading our stdout went away",
/// matching what `docker exec cmd | head` returns (128 + SIGPIPE).
const EXIT_SIGPIPE: i32 = 141;

/// Create, run and reap one exec against an already-resolved endpoint.
pub(super) fn run(endpoint: &Endpoint, spec: &ExecSpec) -> Attempt {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")
    {
        Ok(rt) => rt,
        Err(e) => return Attempt::NotStarted(e),
    };
    let attempt = rt.block_on(exec(endpoint, spec));
    // Drop the runtime without waiting for the detached stdin pump: it is a
    // blocking read that may still be parked on a terminal which will never
    // produce another byte. Whatever it had left to forward is moot — either
    // the command finished, or the pipeline we were writing to went away.
    rt.shutdown_background();
    attempt
}

async fn exec(endpoint: &Endpoint, spec: &ExecSpec<'_>) -> Attempt {
    let exec_id = match create(endpoint, spec).await {
        Ok(id) => id,
        Err(e) => return Attempt::NotStarted(e),
    };

    // Sending the start request is the point of no return. Once those bytes
    // are on the wire we cannot know whether the daemon acted on them, so
    // everything from here on counts as "ran" — reporting a failure is always
    // better than running the command a second time somewhere else.
    let started = match begin(endpoint, &exec_id).await {
        Ok(started) => started,
        Err(e) => return Attempt::NotStarted(e),
    };

    Attempt::Ran(finish(endpoint, &exec_id, started, spec.interactive).await)
}

/// Drive a started exec to completion and collect its exit code.
async fn finish(
    endpoint: &Endpoint,
    exec_id: &str,
    started: Started,
    interactive: bool,
) -> Result<i32> {
    let Started { stream, leftover } = started;
    let (reader, mut writer) = tokio::io::split(stream);

    if interactive {
        // Detached, never awaited: the exec ends when the daemon closes the
        // output side, and a pump blocked on an idle terminal must not hold
        // that up.
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut writer).await;
            // EOF on our stdin means EOF for the command's stdin too.
            let _ = writer.shutdown().await;
        });
    } else {
        // Nothing will be sent; close the write side so the daemon is not left
        // waiting on a half-open connection.
        let _ = writer.shutdown().await;
    }

    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let pumped = pump_output(reader, leftover, &mut stdout.lock(), &mut stderr.lock()).await?;

    match pumped {
        // The command may still be producing output we are no longer reading,
        // so there is no exit code to wait for — report the broken pipe the
        // way a shell pipeline does.
        Pumped::ConsumerGone => Ok(EXIT_SIGPIPE),
        Pumped::Complete => exit_code(endpoint, exec_id).await,
    }
}

// ─── create ───────────────────────────────────────────────────────────────────

async fn create(endpoint: &Endpoint, spec: &ExecSpec<'_>) -> Result<String> {
    let mut body = json!({
        "AttachStdin": spec.interactive,
        "AttachStdout": true,
        "AttachStderr": true,
        "Tty": false,
        "Cmd": spec.command,
    });
    let map = body.as_object_mut().expect("json! built an object");
    if !spec.env.is_empty() {
        map.insert("Env".into(), json!(spec.env));
    }
    if let Some(user) = spec.user {
        map.insert("User".into(), json!(user));
    }
    if let Some(workdir) = spec.workdir {
        map.insert("WorkingDir".into(), json!(workdir));
    }
    if spec.privileged {
        map.insert("Privileged".into(), json!(true));
    }

    let path = format!("/containers/{}/exec", spec.container_id);
    let response = json_request(endpoint, "POST", &path, Some(&body)).await?;
    response["Id"]
        .as_str()
        .map(str::to_string)
        .context("exec create returned no Id")
}

// ─── start ────────────────────────────────────────────────────────────────────

/// A started exec: the hijacked connection plus any output bytes that arrived
/// alongside the response headers.
struct Started {
    stream: Pin<Box<dyn ApiStream>>,
    leftover: Vec<u8>,
}

/// Start the exec on a hijacked connection.
///
/// Asking for `Upgrade: tcp` makes the daemon answer `101 UPGRADED` and hand
/// back a raw full-duplex socket, which sidesteps chunked transfer-encoding
/// for the output stream and lets stdin flow the other way at the same time.
///
/// Any error here means the request never landed, so the command did not run.
async fn begin(endpoint: &Endpoint, exec_id: &str) -> Result<Started> {
    let mut stream = connect(endpoint).await?;
    let body = json!({ "Detach": false, "Tty": false }).to_string();
    let request = format!(
        "POST /exec/{exec_id}/start HTTP/1.1\r\n\
         Host: docker\r\n\
         Content-Type: application/json\r\n\
         Connection: Upgrade\r\n\
         Upgrade: tcp\r\n\
         Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .context("send exec start")?;

    let leftover = read_headers(&mut stream).await?;
    Ok(Started { stream, leftover })
}

/// Consume the status line and headers, returning the body bytes that arrived
/// in the same read — with a hijacked stream the daemon can start framing
/// output into the very packet that carried the response headers.
async fn read_headers(stream: &mut Pin<Box<dyn ApiStream>>) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
        if buf.len() > 16 * 1024 {
            anyhow::bail!("exec response headers too large");
        }
        let n = stream
            .read(&mut chunk)
            .await
            .context("read exec response")?;
        if n == 0 {
            anyhow::bail!("daemon closed the connection before responding");
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..end]);
    // 101 is the hijack; a 200 would mean the daemon streamed without
    // upgrading, which frames output the same way.
    match status_code(&head) {
        Some(101) | Some(200) => Ok(buf[end + 4..].to_vec()),
        _ => anyhow::bail!(
            "exec start returned {}",
            head.lines().next().unwrap_or_default().trim()
        ),
    }
}

// ─── output demultiplexing ────────────────────────────────────────────────────

/// Header of one stdcopy frame: stream id, three reserved bytes, big-endian
/// payload length.
const FRAME_HEADER: usize = 8;
const STREAM_STDERR: u8 = 2;

/// Why the output pump stopped.
#[derive(Debug, PartialEq)]
enum Pumped {
    /// The daemon closed the stream — the command is done.
    Complete,
    /// Whatever was reading our stdout went away (`dip exec … | head`).
    ConsumerGone,
}

/// Split the daemon's multiplexed stream back onto the given sinks.
async fn pump_output<R, O, E>(
    mut reader: R,
    seed: Vec<u8>,
    out: &mut O,
    err: &mut E,
) -> Result<Pumped>
where
    R: tokio::io::AsyncRead + Unpin,
    O: std::io::Write,
    E: std::io::Write,
{
    let mut buf = seed;
    let mut chunk = [0u8; 32 * 1024];

    loop {
        // Drain every whole frame currently buffered before reading more.
        while buf.len() >= FRAME_HEADER {
            let len = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) as usize;
            if buf.len() < FRAME_HEADER + len {
                break;
            }
            let payload = &buf[FRAME_HEADER..FRAME_HEADER + len];
            // Flush each frame as it lands: a long-running command should
            // stream its progress, not surface it all at exit.
            let written = if buf[0] == STREAM_STDERR {
                err.write_all(payload).and_then(|()| err.flush())
            } else {
                out.write_all(payload).and_then(|()| out.flush())
            };
            if let Err(e) = written {
                // `dip exec app … | head` closes our stdout early. That is an
                // ordinary end to a pipeline, not a failure of the exec — and
                // crucially not a reason to run the command again elsewhere.
                if e.kind() == std::io::ErrorKind::BrokenPipe {
                    return Ok(Pumped::ConsumerGone);
                }
                return Err(e.into());
            }
            buf.drain(..FRAME_HEADER + len);
        }

        let n = reader.read(&mut chunk).await.context("read exec output")?;
        if n == 0 {
            return Ok(Pumped::Complete);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

// ─── exit code ────────────────────────────────────────────────────────────────

async fn exit_code(endpoint: &Endpoint, exec_id: &str) -> Result<i32> {
    let path = format!("/exec/{exec_id}/json");
    let response = json_request(endpoint, "GET", &path, None).await?;
    response["ExitCode"]
        .as_i64()
        .map(|c| c as i32)
        .context("exec inspect returned no ExitCode")
}

// ─── plain JSON request/response ──────────────────────────────────────────────

/// One request/response over a fresh connection, using `Connection: close` so
/// the body ends at EOF and we never have to decode chunked framing.
async fn json_request(
    endpoint: &Endpoint,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let mut stream = connect(endpoint).await?;
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n");
    let payload = body.map(|b| b.to_string());
    if let Some(payload) = &payload {
        request.push_str("Content-Type: application/json\r\n");
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    request.push_str("\r\n");
    if let Some(payload) = &payload {
        request.push_str(payload);
    }
    stream.write_all(request.as_bytes()).await?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .context("malformed docker API response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];

    let code = status_code(&head).context("malformed docker API status line")?;
    if !(200..300).contains(&code) {
        // The daemon puts the useful part in the body ({"message": "..."}),
        // not the reason phrase.
        anyhow::bail!("docker API returned {code}: {}", api_message(body));
    }
    serde_json::from_slice(body).context("docker API response body")
}

/// Numeric status from an HTTP status line ("HTTP/1.1 201 Created" → 201).
fn status_code(head: &str) -> Option<u16> {
    head.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

/// The daemon's own error text, falling back to the raw body.
fn api_message(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| String::from_utf8_lossy(body).trim().to_string())
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `frames` through the real `pump_output` in `chunk`-sized reads and
    /// return what landed on each sink.
    fn pump(frames: &[u8], chunk: usize) -> (String, String) {
        struct Trickle<'a> {
            data: &'a [u8],
            chunk: usize,
        }
        impl tokio::io::AsyncRead for Trickle<'_> {
            fn poll_read(
                mut self: Pin<&mut Self>,
                _cx: &mut std::task::Context<'_>,
                buf: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                let n = self.chunk.min(self.data.len()).min(buf.remaining());
                buf.put_slice(&self.data[..n]);
                self.data = &self.data[n..];
                std::task::Poll::Ready(Ok(()))
            }
        }

        let (mut out, mut err) = (Vec::new(), Vec::new());
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let done = rt
            .block_on(pump_output(
                Trickle {
                    data: frames,
                    chunk,
                },
                Vec::new(),
                &mut out,
                &mut err,
            ))
            .unwrap();
        assert_eq!(done, Pumped::Complete);
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// A sink that behaves like a pipe whose reader has exited.
    struct ClosedPipe;
    impl std::io::Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "closed",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_consumer_ends_the_pump_without_an_error() {
        // `dip exec app … | head` closes our stdout early. This has to report
        // ConsumerGone, not Err: the caller reads an error as "never ran" and
        // would then run the command a second time through the docker CLI.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mut err = Vec::new();
        let done = rt
            .block_on(pump_output(
                LIVE_FRAMES,
                Vec::new(),
                &mut ClosedPipe,
                &mut err,
            ))
            .expect("a broken pipe ends a pipeline, it does not fail the exec");
        assert_eq!(done, Pumped::ConsumerGone);
    }

    /// Byte-for-byte the framing a live daemon produced for
    /// `sh -c "cat; echo ERR >&2"` with `hello from stdin` on stdin.
    const LIVE_FRAMES: &[u8] = b"\x01\x00\x00\x00\x00\x00\x00\x11hello from stdin\n\
                                 \x02\x00\x00\x00\x00\x00\x00\x04ERR\n";

    #[test]
    fn demultiplexes_stdout_and_stderr_onto_separate_sinks() {
        let (out, err) = pump(LIVE_FRAMES, 4096);
        assert_eq!(out, "hello from stdin\n");
        assert_eq!(err, "ERR\n");
    }

    #[test]
    fn frames_split_across_reads_are_reassembled() {
        // One byte at a time: every header and payload straddles a read.
        let (out, err) = pump(LIVE_FRAMES, 1);
        assert_eq!(out, "hello from stdin\n");
        assert_eq!(err, "ERR\n");

        // A payload larger than one read, too.
        let mut big = vec![1u8, 0, 0, 0];
        big.extend_from_slice(&5000u32.to_be_bytes());
        big.extend_from_slice(&[b'x'; 5000]);
        let (out, err) = pump(&big, 64);
        assert_eq!(out.len(), 5000);
        assert!(err.is_empty());
    }

    #[test]
    fn reads_the_numeric_status_not_a_substring_of_the_reason_phrase() {
        assert_eq!(status_code("HTTP/1.1 101 UPGRADED\r\nFoo: bar"), Some(101));
        assert_eq!(status_code("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(status_code("HTTP/1.1 404 Not Found"), Some(404));
        // A reason phrase mentioning another code must not be mistaken for it.
        assert_eq!(
            status_code("HTTP/1.1 500 error 200 while starting"),
            Some(500)
        );
        assert_eq!(status_code("garbage"), None);
        assert_eq!(status_code(""), None);
    }

    #[test]
    fn surfaces_the_daemons_message_rather_than_the_reason_phrase() {
        assert_eq!(
            api_message(br#"{"message":"No such container: abc"}"#),
            "No such container: abc"
        );
        // Not JSON, or JSON without a message: fall back to the raw body.
        assert_eq!(api_message(b"  plain text  "), "plain text");
        assert_eq!(api_message(br#"{"other":1}"#), r#"{"other":1}"#);
    }

    #[test]
    fn a_truncated_trailing_frame_is_dropped_not_misread() {
        // Daemon died mid-frame: emit the complete frames, never a partial
        // payload padded with whatever came next.
        let mut truncated = LIVE_FRAMES.to_vec();
        truncated.extend_from_slice(&[1u8, 0, 0, 0, 0, 0, 0, 99]);
        truncated.extend_from_slice(b"only some of it");
        let (out, err) = pump(&truncated, 4096);
        assert_eq!(out, "hello from stdin\n");
        assert_eq!(err, "ERR\n");
    }
}
