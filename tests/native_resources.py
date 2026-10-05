#!/usr/bin/env python3
"""Opt-in installed-Herdr acceptance: real resources, no native agents/models.

Uses only fresh private temporary state and a named headless Herdr server. The
controller/node transport is real restricted NATS. Never contacts a user session.
"""

import argparse
import json
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path

from service_workflow import Fixture, uid, wait_for


class NativeResources(Fixture):
    def __init__(self, root, binary, nats, herdr, plugin=None):
        super().__init__(root, binary, nats, False)
        self.real_herdr = str(herdr)
        self.native_server = None
        self.native_socket = None
        self.plugin = plugin
        for key, name in (
            ("XDG_CONFIG_HOME", "hcfg"),
            ("XDG_STATE_HOME", "hstate"),
            ("XDG_CACHE_HOME", "hcache"),
            ("XDG_RUNTIME_DIR", "hrun"),
        ):
            directory = root / name
            directory.mkdir(mode=0o700)
            self.env[key] = str(directory)
        self.env["SHELL"] = str(Path(shutil.which("bash")).resolve())
        self.env["HERDR_CONFIG_PATH"] = str(
            self.file(
                "herdr.toml",
                f"""onboarding = false
[terminal]
default_shell = {json.dumps(self.env["SHELL"])}
shell_mode = "non_login"
new_cwd = "home"
[update]
version_check = false
manifest_check = false
[session]
resume_agents_on_restore = false
""",
            )
        )

    def native(self, *args):
        assert self.native_socket.is_relative_to(self.root)
        assert self.env["HERDR_SOCKET_PATH"] == str(self.native_socket)
        return json.loads(self.command([self.real_herdr, *args]))["result"]

    def node_row(self, attempt):
        db = self.node / "node.sqlite"
        if not db.exists():
            return None
        with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as connection:
            return connection.execute(
                "SELECT state,epoch FROM jobs WHERE id=?", (attempt,)
            ).fetchone()

    def workflow(self):
        session = "hs-test"
        self.native_socket = self.root / "hcfg/herdr/sessions" / session / "herdr.sock"
        assert not self.native_socket.exists()
        self.native_server = self.spawn(
            "real-herdr",
            [self.real_herdr, "--session", session, "server"],
            cwd=self.root,
        )
        self.env["HERDR_SOCKET_PATH"] = str(self.native_socket)
        wait_for("owned native server socket", self.native_socket.exists)
        version = self.command([self.real_herdr, "--version"])
        snapshot = self.native("api", "snapshot")
        protocol = snapshot["snapshot"]["protocol"]
        assert protocol >= 22, snapshot.keys()
        print(f"Native resource acceptance: {version}, protocol {protocol}", flush=True)
        if self.plugin is not None:
            # Disabled linking validates the installed manifest and registry
            # without executing startup hooks or changing any user config.
            manifest = (self.plugin / "herdr-plugin.toml").read_text()
            assert "[[build]]" not in manifest and '"target/release/' not in manifest
            self.command(
                [self.real_herdr, "plugin", "link", str(self.plugin), "--disabled"]
            )
            plugins = self.native(
                "plugin", "list", "--plugin", "herdr-supervisor", "--json"
            )["plugins"]
            assert len(plugins) == 1
            plugin = plugins[0]
            assert (
                plugin["enabled"] is False
                and Path(plugin["plugin_root"]) == self.plugin
            )
            assert Path(plugin["manifest_path"]) == self.plugin / "herdr-plugin.toml"
            assert {a["id"] for a in plugin["actions"]} == {
                "new",
                "open",
                "open-popup",
                "focus",
                "unfocus",
                "adopt-workspace",
                "pause",
                "resume",
                "configure",
                "doctor",
            }
            assert {p["id"] for p in plugin["panes"]} == {
                "projects",
                "new",
                "pick",
                "adopt",
            }
            executable = self.plugin.parent.parent / "bin/herdr-supervisor"
            for entry in plugin["actions"] + plugin["panes"] + plugin["startup"]:
                assert Path(entry["command"][0]) == executable and executable.is_file()
            assert "control" in self.command([executable, "--help"])
            self.command([self.real_herdr, "plugin", "unlink", "herdr-supervisor"])
            assert not self.native(
                "plugin", "list", "--plugin", "herdr-supervisor", "--json"
            )["plugins"]
            print(
                "PASS: packaged manifest link/list/unlink, disabled hooks, exact action/pane commands and executable",
                flush=True,
            )

        executor = json.loads((self.root / "services/executor.json").read_text())
        executor["herdr_binary"] = self.real_herdr
        executor["herdr_socket"] = str(self.native_socket)
        executor_path = self.file("native-executor.json", executor)
        task = self.apply(
            "create_task",
            run=self.run,
            expected_revision=self.snapshot()["run"]["revision"],
            dependencies=[],
            contract={
                "title": "Native resource acceptance",
                "instructions": "No model is launched",
                "criteria": ["Resource identity is exact"],
                "kind": "code",
                "checks": [],
                "memory_topics": [],
            },
        )
        attempt = self.apply(
            "dispatch",
            task=task["id"],
            expected_revision=task["revision"],
            node="local",
            profile="auto",
        )["id"]
        access = (
            "--node",
            "local",
            "--owner",
            self.owner,
            "--grant",
            self.root / "services/node-grant.json",
        )

        def relay_until(state):
            self.cli(
                self.node,
                "relay-node",
                "--broker",
                self.root / "services/node-broker.json",
                *access,
                "--once",
            )
            row = self.node_row(attempt)
            return row if row and row[0] == state else None

        wait_for(
            "offline dispatch persisted by real node relay",
            lambda: relay_until("reserved"),
        )
        prepare = ("prepare", "--executor", executor_path, *access, attempt)
        prepared = self.cli(self.node, *prepare)
        assert prepared["state"] == "prepared" and prepared["model_ready"] is False
        assert self.cli(self.node, *prepare) == prepared
        checkout = Path(prepared["worktree"])
        assert checkout == self.root / "worktrees" / attempt
        assert self.git(checkout, "rev-parse", "HEAD") == prepared["base_oid"]
        binding = prepared["binding"]
        spaces = self.native("workspace", "list")["workspaces"]
        matching = [
            w
            for w in spaces
            if w.get("worktree") and w["worktree"]["checkout_path"] == str(checkout)
        ]
        assert (
            len(matching) == 1 and matching[0]["workspace_id"] == binding["workspace"]
        )
        panes = self.native("pane", "list")["panes"]
        exact = [p for p in panes if p["pane_id"] == binding["pane"]]
        assert len(exact) == 1 and exact[0]["terminal_id"] == binding["terminal"]
        assert self.native("agent", "list")["agents"] == []
        print(
            "PASS: actual Herdr worktree creation, exact binding and no duplicate on replay",
            flush=True,
        )

        self.apply(
            "stop_attempt",
            attempt=attempt,
            expected_epoch=self.attempt(attempt)["epoch"],
        )
        _, epoch = wait_for(
            "cancellation fenced before launch", lambda: relay_until("cancelling")
        )

        # Only after the durable cancellation fence: this exercises production
        # lifecycle reconciliation without allowing a model-launching job.
        def native_cycle():
            cycle = self.cli(
                self.node,
                "run-node",
                "--broker",
                self.root / "services/node-broker.json",
                "--executor",
                executor_path,
                "--runtime-config",
                self.root / "services/runtime.json",
                *access,
                "--once",
            )
            health = cycle["herdr_events"]
            assert health["watchers"] == 1 and health["authority"] == "hints_only"
            return health if health["watching"] == 1 else None

        health = wait_for(
            "production event subscription and snapshot on actual Herdr", native_cycle
        )
        print(
            f"PASS: real native event subscription/snapshot, hints only; lifecycle_only={health['lifecycle_only']}",
            flush=True,
        )
        wait_for(
            "controller applies signed stop",
            lambda: (
                relay_until("stopped") and self.attempt(attempt)["state"] == "stopped"
            ),
        )

        def stop_received():
            relay_until("stopped")
            report = self.cli(
                self.node,
                "cleanup-inspect",
                "--executor",
                executor_path,
                "--expected-epoch",
                epoch,
                attempt,
            )
            return (
                report
                if all(
                    c["status"] == "passed"
                    for c in report["checks"]
                    if c["check"]
                    not in (
                        "descendants_and_background_processes_exited",
                        "retention_policy_satisfied",
                    )
                )
                else None
            )

        wait_for(
            "real native cleanup inspection and controller stop receipt", stop_received
        )
        with sqlite3.connect(
            f"file:{self.node / 'node.sqlite'}?mode=ro", uri=True
        ) as connection:
            assert (
                connection.execute("SELECT count(*) FROM runtime_processes").fetchone()[
                    0
                ]
                == 0
            )
        assert not self.models and not self.launches and not self.registrations
        assert self.native("agent", "list")["agents"] == []
        head = self.git(checkout, "rev-parse", "HEAD")
        decision = self.file(
            "native-retirement.json",
            {
                "operation": uid(),
                "attempt": attempt,
                "expected_epoch": epoch,
                "expected_head": head,
                "processes_confirmed_stopped": True,
                "retain_commits_remove_checkout": True,
                "reason": "Isolated fixture only; no command or model was launched in its idle shell",
            },
        )
        args = ("cleanup-retire", "--executor", executor_path, "--request", decision)
        receipt = self.cli(self.node, *args)
        assert receipt["state"] == "removed", receipt
        assert self.cli(self.node, *args) == receipt
        assert not checkout.exists()
        assert self.git(self.repo, "rev-parse", receipt["retained_ref"]) == head
        assert self.git(self.repo, "rev-parse", prepared["branch"]) == head
        assert all(
            w["workspace_id"] != binding["workspace"]
            for w in self.native("workspace", "list")["workspaces"]
        )
        print(
            "PASS: real NATS cancellation, no native process, actual Herdr removal and retained commits",
            flush=True,
        )

    def close(self):
        try:
            if self.native_server is not None and self.native_server.poll() is None:
                # Socket is pinned to our fresh private named session, never the
                # inherited session. No generic process-name/PID cleanup is used.
                assert self.native_socket.is_relative_to(self.root)
                assert self.env["HERDR_SOCKET_PATH"] == str(self.native_socket)
                self.command([self.real_herdr, "server", "stop"])
                self.native_server.wait(timeout=15)
                assert self.native_server.returncode == 0
        finally:
            super().close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--nats-server", required=True)
    parser.add_argument("--herdr", required=True)
    parser.add_argument(
        "--plugin",
        type=Path,
        help="Optional packaged plugin directory (linked disabled only)",
    )
    args = parser.parse_args()
    herdr = shutil.which(args.herdr)
    nats = shutil.which(args.nats_server)
    assert herdr and nats, "Herdr and NATS must be explicitly available"
    with tempfile.TemporaryDirectory(prefix="hs-native-") as directory:
        fixture = NativeResources(
            Path(directory),
            args.binary.resolve(),
            Path(nats).resolve(),
            Path(herdr).resolve(),
            args.plugin.resolve() if args.plugin else None,
        )
        try:
            fixture.setup()
            fixture.workflow()
        except Exception as error:
            print(f"Native resource acceptance failed: {error!r}", file=sys.stderr)
            for log in fixture.root.glob("*.log"):
                print(f"--- {log.name} ---\n{log.read_text()[-8000:]}", file=sys.stderr)
            raise
        finally:
            fixture.close()


if __name__ == "__main__":
    main()
