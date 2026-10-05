"""Exercise the plugin's creation entrypoint on a real, private terminal."""

import errno
import fcntl
import os
import pty
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
from pathlib import Path


def main():
    with tempfile.TemporaryDirectory(prefix="supervisor-setup-pane-") as folder:
        home = Path(folder)
        root = home / "projects"
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
        process = subprocess.Popen(
            [sys.argv[1], "--root", str(root), "pane", "new"],
            stdin=slave,
            stdout=slave,
            stderr=slave,
            env={"HOME": folder, "TERM": "xterm-256color", "PATH": os.environ["PATH"]},
        )
        os.close(slave)
        output = bytearray()
        try:
            until = time.monotonic() + 8
            while b"Ctrl-R" not in output and time.monotonic() < until:
                if select.select([master], [], [], 0.1)[0]:
                    try:
                        data = os.read(master, 65536)
                    except OSError as error:
                        if error.errno == errno.EIO:
                            break
                        raise
                    if not data:
                        break
                    output.extend(data)
                if process.poll() is not None:
                    break
            assert b"New supervised project" in output, output.decode(errors="replace")
            assert b"Owner credential" in output, output.decode(errors="replace")
            assert b"Controller" in output and b"Base commit" in output
            assert b"Check argv" in output and b"Ctrl-R" in output
            assert not root.exists(), "Opening the form created project state"
            os.write(master, b"\x1b")
            time.sleep(0.2)
            os.write(master, b"q")
            assert process.wait(timeout=5) == 0
            assert not root.exists(), "Cancelling the form created project state"
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
            os.close(master)
    print(
        "Plugin creation pane opens managed form; cancellation has no project effects"
    )


if __name__ == "__main__":
    main()
