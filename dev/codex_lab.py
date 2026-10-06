"""Trust only an explicitly selected workspace in the disposable native lab."""

import fcntl
import hashlib
import json
import os
import selectors
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import tomllib
from local_stack import lab, read, require

REVIEWED_HOOK = "38a90a2a99872d06dcbc978e7613406cdd852dbb07503abd7a113f518d5951f3"


def hook_state(root, config, env):
    script = root / "codex/herdr-agent-state.sh"
    require(
        hashlib.sha256(script.read_bytes()).hexdigest() == REVIEWED_HOOK,
        "Herdr Codex hook changed; review its source before trusting it",
    )
    with (
        open(root / "logs/codex-hooks.log", "ab") as log,
        subprocess.Popen(
            [config["codex"], "app-server", "--stdio"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            cwd=root,
            env=env,
        ) as process,
    ):

        def send(value):
            process.stdin.write((json.dumps(value) + "\n").encode())
            process.stdin.flush()

        def receive(target):
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if not selector.select(1):
                        continue
                    raw = process.stdout.readline(1024 * 1024)
                    require(raw.endswith(b"\n"), "invalid Codex hook response")
                    value = json.loads(raw)
                    if value.get("id") == target:
                        require("result" in value, "Codex hook inspection failed")
                        return value["result"]
                raise RuntimeError("Codex hook inspection timed out")

        try:
            send(
                {
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {"name": "herdr-supervisor-lab", "version": "1"},
                        "capabilities": {"experimentalApi": True},
                    },
                }
            )
            receive(1)
            send({"method": "initialized", "params": {}})
            send({"id": 2, "method": "hooks/list", "params": {"cwds": [str(root)]}})
            data = receive(2)["data"]
            require(
                len(data) == 1 and not data[0]["errors"] and len(data[0]["hooks"]) == 1,
                "unexpected Codex hooks",
            )
            hook = data[0]["hooks"][0]
            require(
                hook["sourcePath"] == str(root / "codex/hooks.json")
                and hook["eventName"] == "sessionStart"
                and hook["handlerType"] == "command"
                and hook["command"] == "bash '" + str(script) + "' session",
                "unexpected Codex hook identity",
            )
            return hook
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


def install_hook_trust(root, config, env):
    hook = hook_state(root, config, env)
    path = root / "codex/config.toml"
    with open(path, "a") as stream:
        stream.write(
            "\n[hooks.state."
            + json.dumps(hook["key"])
            + "]\ntrusted_hash = "
            + json.dumps(hook["currentHash"])
            + "\n"
        )
        stream.flush()
        os.fsync(stream.fileno())
    require(
        hook_state(root, config, env)["trustStatus"] == "trusted",
        "Codex hook trust was not persisted",
    )


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
