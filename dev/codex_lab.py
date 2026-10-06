"""Trust only an explicitly selected workspace in the disposable native lab."""

import fcntl
import json
import os
import sys
import tempfile
from pathlib import Path

import tomllib
from local_stack import lab, read, require


def workspace(root, argv):
    if "--approve-for-me" not in argv:
        return None  # Mailer's queue/status commands do not open a workspace.
    require(argv.count("--cd") == 1, "lab launch needs one explicit workspace")
    index = argv.index("--cd")
    require(index + 1 < len(argv), "missing lab workspace")
    cwd = Path(argv[index + 1])
    require(
        cwd.is_absolute() and cwd.resolve() == cwd and cwd.is_relative_to(root),
        "workspace escapes the authorized lab",
    )
    return cwd


def trust(root, cwd):
    # The installed TUI requires persisted user-level trust; -c is insufficient.
    # Only the lab's private CODEX_HOME is touched. Serialize concurrent launches.
    config = root / "codex/config.toml"
    with open(root / "codex/trust.lock", "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        text = config.read_text()
        entry = tomllib.loads(text).get("projects", {}).get(str(cwd))
        if entry is not None:
            require(entry.get("trust_level") == "trusted", "conflicting lab trust")
            return
        text += "\n[projects." + json.dumps(str(cwd)) + ']\ntrust_level = "trusted"\n'
        fd, name = tempfile.mkstemp(prefix="config.", dir=root / "codex")
        try:
            with os.fdopen(fd, "w") as stream:
                stream.write(text)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, config)
        finally:
            Path(name).unlink(missing_ok=True)


def main():
    root = lab(os.environ["HERDR_SUPERVISOR_LAB"])
    require(os.environ.get("CODEX_HOME") == str(root / "codex"), "foreign Codex home")
    binary = read(root / "lab.json")["codex"]
    cwd = workspace(root, sys.argv[1:])
    if cwd is not None:
        trust(root, cwd)
    os.execv(binary, [binary, *sys.argv[1:]])


if __name__ == "__main__":
    main()
