"""Exercise terminal protocol and cleanup without a terminal emulator or DSR replies."""
import base64
import errno
import fcntl
import os
from pathlib import Path
import pty
import re
import select
import shlex
import sqlite3
import struct
import subprocess
import sys
import tempfile
import termios
import time

binary = sys.argv[1]


def read_for(fd, duration=0.3):
    result = bytearray()
    deadline = time.monotonic() + duration
    while time.monotonic() < deadline:
        if select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]:
            try:
                result.extend(os.read(fd, 65536))
            except OSError as err:
                if err.errno == errno.EIO:
                    break
                raise
    return bytes(result)


def wait_for(fd, marker, timeout=8):
    result = b""
    deadline = time.monotonic() + timeout
    while marker not in result and time.monotonic() < deadline:
        result += read_for(fd, 0.1)
    assert marker in result, (marker, result[-3000:])
    return result


with tempfile.TemporaryDirectory(prefix="scat-pty-") as directory:
    root = Path(directory)
    scripts = root / "scripts"
    scripts.mkdir()
    (scripts / "a.py").write_text("#!/usr/bin/env python3\n# Purpose: PTY fixture\nprint(1)\n")
    db = root / "catalog.sqlite"
    env = dict(os.environ, TERM="xterm-256color", LC_ALL="C", NO_COLOR="1")
    for key in ("SCAT_CONFIG", "SCAT_TUI_GLYPHS", "SCAT_TUI_COLORS", "SCAT_DB"):
        env.pop(key, None)
    subprocess.run([binary, "--db", str(db), "catalog", "build", "--scan-root", str(scripts), "-q"], env=env, check=True, capture_output=True)

    # Diagnostics must work without a catalog; rejection must emit no escapes.
    info = subprocess.run([binary, "tui", "--terminal-info"], env=env, capture_output=True)
    assert info.returncode == 0 and b"Clipboard availability: unknown" in info.stdout
    rejected = subprocess.run([binary, "tui"], env=env, capture_output=True)
    assert rejected.returncode != 0 and b"requires terminal stdin and stdout" in rejected.stderr
    assert b"\x1b[?1049h" not in rejected.stdout

    # Explicit flags override mode environment settings and locale guesses.
    override_env = dict(env, SCAT_TUI_GLYPHS="unicode", SCAT_TUI_COLORS="full")
    info = subprocess.run([binary, "tui", "--terminal-info", "--glyphs", "ascii", "--colors", "reduced"], env=override_env, capture_output=True, check=True)
    assert b"glyphs=Ascii; colors=Mono" in info.stdout
    override_env.pop("NO_COLOR", None)
    info = subprocess.run([binary, "tui", "--terminal-info", "--glyphs", "ascii", "--colors", "reduced"], env=override_env, capture_output=True, check=True)
    assert b"glyphs=Ascii; colors=Reduced" in info.stdout

    # A real PTY is still unsuitable when it advertises TERM=dumb.
    master, slave = pty.openpty()
    try:
        original = termios.tcgetattr(slave)
        process = subprocess.Popen([binary, "tui"], stdin=slave, stdout=slave, stderr=slave, env=dict(env, TERM="dumb"))
        process.wait(timeout=5)
        rejected = read_for(master)
        assert process.returncode != 0 and b"TERM is unset or dumb" in rejected
        assert b"\x1b[?1049h" not in rejected
        assert termios.tcgetattr(slave) == original
    finally:
        os.close(master)
        os.close(slave)

    # A catalog this scat cannot read is reported on the terminal at launch,
    # with the actionable message, before any fullscreen setup. The catalog
    # must be left byte-identical.
    for label, delta, expected in (("newer", 1, b"upgrade scat on this host"), ("older", -1, b"publisher must upgrade")):
        mismatched = root / f"{label}.sqlite"
        mismatched.write_bytes(db.read_bytes())
        conn = sqlite3.connect(mismatched)
        conn.execute("UPDATE index_metadata SET schema_version = schema_version + ?", (delta,))
        conn.commit()
        conn.close()
        before = mismatched.read_bytes()
        master, slave = pty.openpty()
        try:
            process = subprocess.Popen([binary, "--db", str(mismatched), "--no-cache", "tui"], stdin=slave, stdout=slave, stderr=slave, env=env)
            process.wait(timeout=5)
            output = read_for(master)
            assert process.returncode != 0 and expected in output, (label, output)
            assert b"catalog schema is version" in output, (label, output)
            assert b"\x1b[?1049h" not in output, (label, output)
        finally:
            os.close(master)
            os.close(slave)
        assert mismatched.read_bytes() == before
        assert sorted(p.name for p in root.glob(f"{label}.sqlite*")) == [f"{label}.sqlite"]

    for viewer_exit in (0, 1):
        viewer = root / "viewer.py"
        viewer.write_text("import termios, sys\n"
                          "assert termios.tcgetattr(0)[3] & termios.ICANON\n"
                          "print('VIEWER_READY', flush=True)\n"
                          f"sys.exit({viewer_exit})\n")
        env["SCAT_EDITOR"] = f"{shlex.quote(sys.executable)} {shlex.quote(str(viewer))}"
        extra = []
        if viewer_exit == 1:
            env.pop("NO_COLOR", None)
            extra = ["--no-color", "--colors", "full"]
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 120, 0, 0))
        original = termios.tcgetattr(slave)
        process = subprocess.Popen([binary, "--db", str(db), "--no-cache", "tui", "--glyphs", "ascii"] + extra,
                                   stdin=slave, stdout=slave, stderr=slave, env=env)
        transcript = b""
        try:
            transcript += wait_for(master, b"\x1b[?2004h")
            transcript += read_for(master, 0.6)
            assert not termios.tcgetattr(slave)[3] & termios.ICANON
            # F2 releases mouse; frozen screen must remain byte-for-byte idle.
            os.write(master, b"\x1bOQ")
            transcript += wait_for(master, b"\x1b[?1000l")
            transcript += read_for(master, 0.3)
            assert read_for(master, 0.4) == b"", "native selection redrew"
            os.write(master, b"\x1bOQ")
            transcript += wait_for(master, b"\x1b[?1000h")
            transcript += read_for(master, 0.2)
            # Paste printable shortcuts as literal query text; F3 copies it.
            pasted = b"?qvsi\r\nabc"
            os.write(master, b"\x1b[200~" + pasted + b"\x1b[201~")
            transcript += read_for(master, 0.4)
            os.write(master, b"\x1bOR")
            copied = wait_for(master, b"\x1b]52;c;") + read_for(master, 0.2)
            transcript += copied
            encoded = re.search(rb"\x1b\]52;c;([^\x07]*)\x07", copied)
            assert encoded and base64.b64decode(encoded[1]) == b"?qvsi abc", copied
            # Clear query, await result, enter results, open catalog viewer.
            os.write(master, b"\x15")  # Ctrl+U
            transcript += read_for(master, 0.6)
            os.write(master, b"\r")
            transcript += read_for(master, 0.3)
            os.write(master, b"v")
            transition = wait_for(master, b"VIEWER_READY") + read_for(master, 0.5)
            transcript += transition
            assert b"\x1b[?2004l" in transition and b"\x1b[?1049l" in transition
            assert b"\x1b[?2004h" in transition and b"\x1b[?1049h" in transition
            assert not termios.tcgetattr(slave)[3] & termios.ICANON
            os.write(master, b"\x03")
            process.wait(timeout=5)
            transcript += read_for(master, 0.2)
            assert process.returncode == 0
            assert termios.tcgetattr(slave) == original, "terminal attributes not restored"
            assert transcript.endswith(b"\x1b[?25h") or b"\x1b[?25h" in transcript[-100:]
            assert b"\x1b[?2004l" in transcript[-200:]
            assert b"\x1b[6n" not in transcript, "cursor-position query emitted"
            assert all(b < 128 for b in transcript), "ASCII output contained UTF-8"
            # SGR may include reset/default colors, bold, reverse, underline etc.
            for parameters in re.findall(rb"\x1b\[([0-9;]*)m", transcript):
                assert all(int(n or b"0") not in list(range(30, 38)) + list(range(40, 48)) + [38, 48, 58] + list(range(90, 108)) for n in parameters.split(b";")), parameters
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            os.close(master)
            os.close(slave)
print("PTY terminal regressions passed")
