"""Select Auto and trust only the disposable lab's explicit Claude workspace."""

import fcntl
import json
import os
import sys
import tempfile
from pathlib import Path

from local_stack import lab, read, require


def prepare_workspace(root, cwd):
    require(
        cwd.is_relative_to(root) and cwd.resolve() == cwd,
        "workspace escapes the authorized lab",
    )
    path = root / "claude/.claude.json"
    with open(root / "claude/workspaces.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        value = read(path) if path.exists() else {}
        value.setdefault("projects", {}).setdefault(str(cwd), {})[
            "hasTrustDialogAccepted"
        ] = True
        fd, name = tempfile.mkstemp(prefix="config.", dir=root / "claude")
        try:
            with os.fdopen(fd, "w") as stream:
                json.dump(value, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, path)
        finally:
            Path(name).unlink(missing_ok=True)


def main():
    root = lab(os.environ["HERDR_SUPERVISOR_LAB"])
    require(
        os.environ.get("CLAUDE_CONFIG_DIR") == str(root / "claude"),
        "foreign Claude config",
    )
    binary = read(root / "lab.json")["claude"]
    args = sys.argv[1:]
    if "--session-id" in args or "--resume" in args:
        prepare_workspace(root, Path.cwd())
        args = ["--permission-mode", "auto", *args]
    os.execv(binary, [binary, *args])


if __name__ == "__main__":
    main()
