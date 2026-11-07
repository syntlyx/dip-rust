#!/usr/bin/env bash
#
# End-to-end tests for the exec path against a real Docker daemon.
#
# Everything here is invisible to `cargo test`: whether a command runs once or
# twice, whether a TTY reaches the container, whether an exit code survives the
# trip. Each of these was a real bug that unit tests could not have caught.
#
# Usage: scripts/integration-test.sh [path/to/dip]
# Requires: docker, python3. Leaves nothing behind.

set -uo pipefail

DIP=${1:-target/release/dip}
[ -x "$DIP" ] || { echo "no dip binary at $DIP — cargo build --release first"; exit 1; }
DIP=$(cd "$(dirname "$DIP")" && pwd)/$(basename "$DIP")

PROJECT=dipit$$
WORKDIR=$(mktemp -d)
FAILURES=0

# shellcheck disable=SC2329  # invoked via trap
cleanup() {
  docker rm -f "$(docker ps -aq --filter "label=com.docker.compose.project=$PROJECT")" >/dev/null 2>&1
  docker network rm "${PROJECT}_default" >/dev/null 2>&1
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

ok()   { printf '  \033[32m✓\033[0m %s\n' "$1"; }
fail() { printf '  \033[31m✗\033[0m %s\n     expected: %s\n     actual:   %s\n' "$1" "$2" "$3"; FAILURES=$((FAILURES+1)); }
# is <name> <actual> <expected>
is() { if [ "$2" = "$3" ]; then ok "$1"; else fail "$1" "$3" "$2"; fi; }

# ── a throwaway project ──────────────────────────────────────────────────────
mkdir -p "$WORKDIR/.dip"
printf 'PROJECT_NAME=%s\n' "$PROJECT" > "$WORKDIR/.dip/.env"
printf 'services:\n  app:\n    image: alpine:3.20\n    command: sleep 600\n' > "$WORKDIR/.dip/docker-compose.yml"
cd "$WORKDIR" || exit 1

echo "==> starting $PROJECT"
"$DIP" start >/dev/null 2>&1 || { echo "could not start the test project"; exit 1; }

echo "==> exit codes"
"$DIP" exec app sh -c 'exit 7' >/dev/null 2>&1
is "a failing command's exit code is ours" "$?" "7"
"$DIP" exec app true >/dev/null 2>&1
is "a succeeding command exits 0" "$?" "0"
"$DIP" exec app nosuchbinary >/dev/null 2>&1
is "a missing binary exits 127" "$?" "127"

echo "==> the command runs exactly once"
# Regression: a broken pipe was read as "the API attempt failed", and the
# fallback ran the command a second time.
"$DIP" exec app sh -c 'rm -f /tmp/n; :' >/dev/null 2>&1
"$DIP" exec app sh -c 'echo m >> /tmp/n; seq 1 500000' 2>/dev/null | head -2 >/dev/null
RUNS=$("$DIP" exec app sh -c 'wc -l < /tmp/n' 2>/dev/null | tr -d ' ')
is "a closed pipe does not re-run the command" "$RUNS" "1"
"$DIP" exec app sh -c 'seq 1 500000' 2>/dev/null | head -2 >/dev/null
is "a closed pipe exits 141, like docker exec" "${PIPESTATUS[0]}" "141"

echo "==> streams"
OUT=$("$DIP" exec app sh -c 'echo O; echo E >&2' 2>/dev/null)
is "stdout carries only the command's stdout" "$OUT" "O"
ERR=$("$DIP" exec app sh -c 'echo O; echo E >&2' 2>&1 >/dev/null)
case "$ERR" in *E*) ok "stderr carries the command's stderr";; *) fail "stderr carries the command's stderr" "E" "$ERR";; esac
# dip's own progress must not land in piped data.
PIPED=$("$DIP" exec app echo hi 2>/dev/null)
is "dip's banner stays off stdout" "$PIPED" "hi"
# shellcheck disable=SC2016  # $i is for the container's shell, not ours
MERGED=$("$DIP" exec app sh -c 'for i in 1 2 3; do echo o$i; echo e$i >&2; done' 2>&1 | grep -v Running | tr '\n' ' ')
is "merged order matches docker exec" "$MERGED" "o1 o2 o3 e1 e2 e3 "

echo "==> payload integrity"
head -c 1000000 /dev/urandom > "$WORKDIR/in.bin"
IN=$(python3 -c "import hashlib,sys;print(hashlib.md5(open(sys.argv[1],'rb').read()).hexdigest())" "$WORKDIR/in.bin")
"$DIP" exec app cat < "$WORKDIR/in.bin" 2>/dev/null > "$WORKDIR/out.bin"
OUT_MD5=$(python3 -c "import hashlib,sys;print(hashlib.md5(open(sys.argv[1],'rb').read()).hexdigest())" "$WORKDIR/out.bin")
is "1 MB survives stdin -> stdout unchanged" "$OUT_MD5" "$IN"

echo "==> routing"
if "$DIP" -v exec app true 2>&1 | grep -q "ran over the docker API"; then
  ok "a piped exec takes the API path"
else
  fail "a piped exec takes the API path" "API route" "fell back"
fi
if python3 - "$DIP" <<'PY'
import pty, os, select, sys, time
DIP = sys.argv[1]
def under_pty(argv, feed=None, secs=30):
    pid, fd = pty.fork()
    if pid == 0:
        try: os.execvp(argv[0], argv)
        finally: os._exit(127)
    out, done, fed = b"", time.time() + secs, False
    try:
        while time.time() < done:
            r, _, _ = select.select([fd], [], [], 0.4)
            if r:
                try: d = os.read(fd, 65536)
                except OSError: break
                if not d: break
                out += d
                if feed and not fed and len(out) > 10:
                    time.sleep(0.6); os.write(fd, feed); fed = True
    except Exception: pass
    try: os.kill(pid, 9); os.waitpid(pid, 0)
    except Exception: pass
    return out.decode(errors="replace")

checks = []
t = under_pty([DIP, "exec", "app", "sh", "-c", "test -t 1 && echo TTY_YES || echo TTY_NO"])
checks.append(("a terminal gets a TTY inside the container", "TTY_YES" in t))
r = under_pty([DIP, "-v", "exec", "app", "true"])
checks.append(("a terminal exec takes the docker CLI path", "addressed container directly" in r))
s = under_pty([DIP, "shell", "app"], feed=b"exit\n")
checks.append(("dip shell falls back to sh when bash is absent", "falling back to sh" in s))
bad = 0
for name, good in checks:
    print(f"  \033[32m✓\033[0m {name}" if good else f"  \033[31m✗\033[0m {name}")
    bad += 0 if good else 1
sys.exit(1 if bad else 0)
PY
then :; else FAILURES=$((FAILURES+1)); fi

echo "==> colour discipline"
if python3 - "$DIP" <<'PY'
import os, subprocess, sys
DIP = sys.argv[1]
env = dict(os.environ); env.pop("NO_COLOR", None); env.pop("CLICOLOR_FORCE", None)
def ansi(extra=None, args=("status",)):
    e = dict(env, **(extra or {}))
    return b"\x1b[" in subprocess.run([DIP, *args], capture_output=True, env=e).stdout
checks = [
    ("redirected output carries no escape codes", not ansi()),
    ("NO_COLOR is honoured", not ansi({"NO_COLOR": "1"})),
    ("CLICOLOR_FORCE keeps colour through a pipe", ansi({"CLICOLOR_FORCE": "1"})),
    ("--no-color wins", not ansi(args=("--no-color", "status"))),
]
bad = 0
for name, good in checks:
    print(f"  \033[32m✓\033[0m {name}" if good else f"  \033[31m✗\033[0m {name}")
    bad += 0 if good else 1
sys.exit(1 if bad else 0)
PY
then :; else FAILURES=$((FAILURES+1)); fi

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "all integration checks passed"
else
  echo "$FAILURES integration check(s) failed"
fi
exit "$FAILURES"
