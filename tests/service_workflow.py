#!/usr/bin/env python3
"""Continuous controller/node acceptance test; no live Herdr, mailer or model.

Run with --binary PATH --nats-server PATH. Uses only the Python standard library.
Actual services own all state changes; the fixture never writes their databases.
Git, JetStream, worker IPC, wrappers, process termination and restart are real.
"""

import argparse
import json
import os
import shlex
import shutil
import signal
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path


def uid():
    return str(uuid.uuid4())


def rpc(path, request):
    with socket.socket(socket.AF_UNIX) as stream:
        stream.settimeout(10)
        stream.connect(str(path))
        stream.sendall(json.dumps(request).encode() + b"\n")
        with stream.makefile("rb") as reader:
            response = json.loads(reader.readline(4 * 1024 * 1024))
    if "error" in response:
        raise RuntimeError(response["error"])
    return response["data"]


def wait_for(description, check, timeout=45):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            result = check()
            if result:
                return result
        except (OSError, RuntimeError) as error:
            last = error
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {description}; last error: {last}")


class JsonServer(socketserver.UnixStreamServer):
    def __init__(self, path, callback):
        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                self.request.settimeout(10)
                try:
                    response = {
                        "data": callback(json.loads(self.rfile.readline(300000)))
                    }
                except Exception as error:  # noqa: BLE001 -- propagate fixture failures over RPC
                    print(f"fixture handler failed: {error}", file=sys.stderr)
                    response = {"error": str(error)}
                self.wfile.write(json.dumps(response).encode() + b"\n")

        super().__init__(str(path), Handler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def close(self):
        self.shutdown()
        self.thread.join(timeout=5)
        self.server_close()


class Fixture:
    def __init__(self, root, binary, nats):
        self.root = root
        self.binary = str(binary)
        self.nats = str(nats)
        self.controller = root / "controller"
        self.node = root / "node"
        self.repo = root / "repo"
        self.mailer = root / "mailer"
        self.mailer.mkdir()
        self.children = []
        self.servers = []
        self.spaces = {}
        self.models = {}
        self.registrations = {}
        self.deliveries = {}
        self.memory_deliveries = {}
        self.creates = []
        self.launches = []
        self.bootstrap_gate = threading.Event()
        self.env = {
            "PATH": os.environ["PATH"],
            "HOME": str(root),
            "HS_WORKFLOW_SOCKET": str(root / "fixture.sock"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": "/dev/null",
        }
        if "TZDIR" in os.environ:
            self.env["TZDIR"] = os.environ["TZDIR"]

    def file(self, name, value):
        path = self.root / name
        path.write_text(json.dumps(value) if not isinstance(value, str) else value)
        path.chmod(0o600)
        return path

    def command(self, args, cwd=None):
        result = subprocess.run(
            [str(a) for a in args],
            cwd=cwd,
            env=self.env,
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
        assert result.returncode == 0, (args, result.stdout, result.stderr)
        return result.stdout.strip()

    def cli(self, state, *args):
        return json.loads(
            self.command([self.binary, "control", "--state", state, *args])
        )

    def spawn(self, name, args, cwd=None):
        with (self.root / f"{name}.log").open("ab") as log:
            child = subprocess.Popen(
                [str(a) for a in args],
                cwd=cwd,
                env=self.env,
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
                start_new_session=True,
            )
        self.children.append(child)
        return child

    @staticmethod
    def stop(child):
        if child.poll() is None:
            child.send_signal(signal.SIGINT)
            child.wait(timeout=15)
        assert child.returncode == 0, f"service exited with {child.returncode}"

    def close(self):
        self.bootstrap_gate.set()
        # Services/wrappers are our direct children; never signal recovered PIDs.
        for child in reversed(self.children):
            if child.poll() is None:
                child.send_signal(signal.SIGTERM)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait(timeout=5)
        for server in self.servers:
            server.close()

    def git(self, path, *args):
        return self.command(
            [
                "git",
                "-c",
                "user.name=Workflow Test",
                "-c",
                "user.email=workflow@example.invalid",
                "-c",
                "commit.gpgsign=false",
                *args,
            ],
            cwd=path,
        )

    def snapshot(self):
        return rpc(
            self.controller / "control.sock",
            {
                "op": "snapshot",
                "token": self.owner_token,
                "run": self.run,
            },
        )

    def apply(self, method, **params):
        return rpc(
            self.controller / "control.sock",
            {
                "op": "apply",
                "token": self.supervisor_token,
                "operation_id": uid(),
                "command": {"method": method, "params": params},
            },
        )

    def attempt(self, attempt):
        return next(a for a in self.snapshot()["attempts"] if a["id"] == attempt)

    def worker(self, subject, op, **params):
        token = (self.node / "workers" / subject / "worker.token").read_text().strip()
        return rpc(self.node / "worker.sock", {"op": op, "token": token, **params})

    def report(self, attempt, method, **params):
        operation = uid()
        event = wait_for(
            "worker queues event",
            lambda: self.worker(
                attempt,
                "apply",
                operation_id=operation,
                command={
                    "method": method,
                    "params": {"attempt": attempt, "epoch": 1, **params},
                },
            ),
        )
        assert event["state"] == "queued" and event["controller_applied"] is False

        def receipt():
            value = self.worker(
                attempt, "receipt", attempt=attempt, epoch=1, event=event["event"]
            )
            return value if value["state"] == "controller_applied" else None

        return wait_for("signed event accepted by controller", receipt)

    def herdr(self, request):
        if request["kind"] == "model":
            attempt = request["attempt"]
            assert attempt not in self.models, "duplicate native process"
            self.models[attempt] = {"session": uid(), "cwd": request["cwd"]}
            return self.models[attempt]
        args = request["argv"]
        if args[:2] == ["worktree", "create"]:

            def option(name):
                return args[args.index(name) + 1]

            path = option("--path")
            attempt = Path(path).name
            assert attempt not in self.spaces, "duplicate worktree creation"
            self.git(
                Path(option("--cwd")),
                "worktree",
                "add",
                "-b",
                option("--branch"),
                path,
                option("--base"),
            )
            self.spaces[attempt] = {
                "pane_id": f"p-{attempt}",
                "workspace_id": f"w-{attempt}",
                "tab_id": f"t-{attempt}",
                "terminal_id": f"term-{attempt}",
                "cwd": path,
            }
            self.creates.append(attempt)
            return {"result": {}}
        if args == ["workspace", "list"]:
            return {
                "result": {
                    "workspaces": [
                        {
                            "workspace_id": s["workspace_id"],
                            "worktree": {
                                "checkout_path": s["cwd"],
                                "is_linked_worktree": True,
                            },
                        }
                        for s in self.spaces.values()
                    ]
                }
            }
        if args == ["pane", "list"]:
            return {"result": {"panes": list(self.spaces.values())}}
        if args == ["agent", "list"]:
            return {
                "result": {
                    "agents": [
                        {
                            **self.spaces[a],
                            "agent": "codex",
                            "agent_session": {"value": m["session"]},
                        }
                        for a, m in self.models.items()
                    ]
                }
            }
        if args[:2] == ["pane", "process-info"]:
            return {
                "result": {
                    "process_info": {
                        "shell_pid": 7,
                        "foreground_process_group_id": 7,
                        "foreground_processes": [{"pid": 7}],
                    }
                }
            }
        if args[:2] == ["pane", "run"]:
            space = next(s for s in self.spaces.values() if s["pane_id"] == args[2])
            attempt = Path(space["cwd"]).name
            assert attempt not in self.launches, "duplicate wrapper launch"
            self.launches.append(attempt)
            self.spawn(f"wrapper-{attempt}", shlex.split(args[3]), cwd=space["cwd"])
            return {"result": {}}
        raise AssertionError(f"unexpected Herdr call: {args}")

    def mail(self, request):
        if request["op"] == "register":
            assert request["agent"] not in self.registrations, "duplicate registration"
            registration = {
                "session": {
                    "id": uid(),
                    "epoch": 1,
                    "agent": request["agent"],
                    "runtime": request["runtime"],
                    "location": request["location"],
                },
                "token": uid() + uid(),
            }
            self.registrations[request["agent"]] = registration
            return registration
        if request["op"] == "capabilities":
            return {"idempotentSend": True, "recipientPrecondition": True}
        assert request["op"] == "send", request["op"]
        receiver = self.registrations[request["recipient"]]["session"]
        assert request["recipientSession"] == receiver["id"]
        assert request["recipientEpoch"] == receiver["epoch"]
        attempt = next(
            a for a, m in self.models.items() if m["session"] == receiver["location"]
        )
        # Native bootstrap must follow the controller's resource binding.
        resource = self.attempt(attempt)["resource"]
        assert resource["session"] == receiver["location"]
        assert resource["mailer_agent"] == receiver["agent"]
        if request["operationId"].startswith("memory-"):
            version = int(request["operationId"].rsplit("-", 1)[1])
            self.memory_deliveries[(attempt, version)] = request
        else:
            self.deliveries[attempt] = request
        assert self.bootstrap_gate.wait(timeout=10), "test did not release bootstrap"
        return {
            "id": uid(),
            "recipientSession": receiver["id"],
            "recipientEpoch": receiver["epoch"],
        }

    def start_node(self):
        self.node_process = self.spawn(
            "node",
            [
                self.binary,
                "control",
                "--state",
                self.node,
                "run-node",
                "--broker",
                self.root / "node-broker.json",
                "--executor",
                self.root / "executor.json",
                "--runtime-config",
                self.root / "runtime.json",
                "--node",
                "local",
                "--owner",
                self.owner,
                "--grant",
                self.root / "grant.json",
            ],
        )
        wait_for("node API", lambda: (self.node / "worker.sock").exists())

    def setup(self):
        self.repo.mkdir()
        self.git(self.repo, "init", "--quiet")
        (self.repo / "base.txt").write_text("base\n")
        self.git(self.repo, "add", ".")
        self.git(self.repo, "commit", "-qm", "baseline")
        base = self.git(self.repo, "rev-parse", "HEAD")
        self.owner = self.cli(self.controller, "init")["owner"]
        self.owner_token = (self.controller / "admin.token").read_text().strip()
        self.supervisor_token = self.owner_token
        node_key = self.cli(self.node, "node-key")["key"]
        self.file(
            "grant.json",
            self.cli(
                self.controller,
                "grant",
                "local",
                node_key,
                "--expires-at",
                str(int(time.time()) + 3600),
            ),
        )
        policy = self.cli(self.controller, "broker-policy", "--node", "local")
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        config = self.file(
            "nats.conf",
            {
                "listen": f"127.0.0.1:{port}",
                "jetstream": {
                    "store_dir": str(self.root / "jetstream"),
                    "max_file_store": 2147483648,
                    "max_memory_store": 8388608,
                },
                "authorization": {
                    "users": [
                        {"user": "admin", "password": "admin-test-password"},
                        {
                            "user": "controller",
                            "password": "controller-test-password",
                            "permissions": policy["controller"]["permissions"],
                        },
                        {
                            "user": "node",
                            "password": "node-test-password",
                            "permissions": policy["nodes"]["local"]["permissions"],
                        },
                    ]
                },
            },
        )
        self.spawn("nats", [self.nats, "-c", config])

        def listening():
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return True

        wait_for("private NATS", listening)
        for user in ("admin", "controller", "node"):
            creds = self.file(
                f"{user}-creds.json",
                {"user": user, "password": f"{user}-test-password"},
            )
            self.file(
                f"{user}-broker.json",
                {
                    "url": f"nats://127.0.0.1:{port}",
                    "credentials": str(creds),
                    "private_network_plaintext": True,
                },
            )
        self.cli(
            self.controller,
            "provision",
            "--broker",
            self.root / "admin-broker.json",
            "--target",
            "controller",
            "--target",
            "local",
        )
        self.file(
            "repositories.json",
            {
                "repositories": {
                    "/fixture": {
                        "path": str(self.repo),
                        "node_remotes": {},
                    }
                },
                "check_timeout_secs": 10,
            },
        )
        self.controller_process = self.spawn(
            "controller",
            [
                self.binary,
                "control",
                "--state",
                self.controller,
                "run-controller",
                "--broker",
                self.root / "controller-broker.json",
                "--repositories",
                self.root / "repositories.json",
            ],
        )
        wait_for("controller API", lambda: (self.controller / "control.sock").exists())
        enrolled = self.apply("enroll_node", id="local", key=node_key, max_active=2)
        self.file("node.token", enrolled["credential"])
        run = self.apply(
            "create_run",
            project="workflow",
            repo="/fixture",
            base_oid=base,
            objective="Exercise continuous services",
            policy={
                "max_active": 2,
                "reserved_reviewers": 1,
                "max_attempts": 3,
                "duration_secs": 3600,
                "require_review": True,
                "profiles": {"auto": {"runtime": "codex", "argv": [], "limit": 2}},
                "checks": {
                    "test": ["sh", "-c", 'test "$(cat feature.txt)" = implemented']
                },
            },
        )
        self.run = run["id"]
        bound = self.apply(
            "bind_supervisor", run=self.run, expected_revision=run["revision"]
        )
        self.supervisor_token = bound["credential"]
        script = Path(__file__).resolve()
        for name, mode in (("herdr-stub", "herdr"), ("model-stub", "model")):
            stub = self.file(
                name,
                f'#!{shutil.which("sh")}\nexec {shlex.quote(sys.executable)} {shlex.quote(str(script))} --stub {mode} "$@"\n',
            )
            stub.chmod(0o700)
        self.servers.append(JsonServer(self.root / "fixture.sock", self.herdr))
        self.servers.append(JsonServer(self.mailer / "daemon.sock", self.mail))
        self.file(
            "executor.json",
            {
                "repositories": {"/fixture": str(self.repo)},
                "worktree_root": str(self.root / "worktrees"),
                "herdr_binary": str(self.root / "herdr-stub"),
                "herdr_socket": str(self.root / "fixture.sock"),
            },
        )
        admin = self.file("mailer-admin.token", uid() + uid())
        self.file(
            "runtime.json",
            {
                "supervisor_binary": self.binary,
                "mailer_state": str(self.mailer),
                "mailer_admin_credential": str(admin),
                "node_credential": str(self.root / "node.token"),
                "codex_binary": str(self.root / "model-stub"),
                "mailer_runtime_binary": str(self.root / "unused-runtime"),
                "claude_development_channel": False,
            },
        )

    def ready(self, attempt):
        wait_for(
            "native bootstrap after controller binding",
            lambda: attempt in self.deliveries,
        )
        # A mock native delivery has not made the model ready.
        assert self.attempt(attempt)["state"] == "awaiting_ready"
        context = wait_for(
            "worker context",
            lambda: self.worker(attempt, "context", attempt=attempt, epoch=1),
        )
        self.report(attempt, "worker_ready", session=self.models[attempt]["session"])
        return context

    def workflow(self):
        task = self.apply(
            "create_task",
            run=self.run,
            expected_revision=self.snapshot()["run"]["revision"],
            dependencies=[],
            contract={
                "title": "Implement fixture",
                "instructions": "Write feature.txt",
                "criteria": ["Feature is implemented"],
                "kind": "code",
                "checks": ["test"],
                "memory_topics": [],
            },
        )
        author = self.apply(
            "dispatch",
            task=task["id"],
            expected_revision=task["revision"],
            node="local",
            profile="auto",
        )["id"]
        # Dispatch while node is offline; durable transport must deliver on startup.
        self.start_node()
        wait_for(
            "bootstrap enters in-flight external call",
            lambda: author in self.deliveries,
        )
        # Force SIGINT into the old loop's gap: the executor is blocked in mailer,
        # outside its sleep/select. Shutdown must be retained until work drains.
        self.node_process.send_signal(signal.SIGINT)
        self.bootstrap_gate.set()
        self.node_process.wait(timeout=15)
        assert self.node_process.returncode == 0
        assert not (self.node / "worker.sock").exists()
        # The native wrapper belongs to the pane and survives the relay restart.
        self.start_node()
        context = self.ready(author)
        proposal = self.apply(
            "propose_memory",
            run=self.run,
            topic="preferences",
            text="Use UTF-8 byte offsets",
            source_attempt=None,
            evidence=["review:workflow-fixture"],
        )
        self.apply("approve_memory", proposal=proposal["id"], expected_revision=0)
        wait_for(
            "live approved memory native notice",
            lambda: (author, 1) in self.memory_deliveries,
        )
        current = wait_for(
            "current memory context",
            lambda: self.worker(author, "context", attempt=author, epoch=1),
        )
        assert current["memory"] == context["memory"], "startup snapshot was mutated"
        assert (
            current["current_memory"]["snapshot"]["topics"]["preferences"]["text"]
            == "Use UTF-8 byte offsets"
        )
        assert current["current_memory"]["acknowledged_version"] is None, (
            "native send claimed model acknowledgement"
        )
        acknowledged = self.cli(
            self.node,
            "worker",
            "--credential",
            self.node / "workers" / author / "worker.token",
            "--attempt",
            author,
            "--epoch",
            "1",
            "acknowledge-memory",
            "1",
        )
        assert (
            acknowledged["acknowledged"] and not acknowledged["model_behavior_verified"]
        )
        self.stop(self.node_process)
        self.apply(
            "withdraw_memory", run=self.run, topic="preferences", expected_revision=1
        )
        self.start_node()
        wait_for(
            "offline withdrawal reaches same native session",
            lambda: (author, 2) in self.memory_deliveries,
        )
        current = wait_for(
            "withdrawn memory context",
            lambda: self.worker(author, "context", attempt=author, epoch=1),
        )
        assert current["current_memory"]["version"] == 2
        assert current["current_memory"]["snapshot"]["topics"] == {}
        assert current["current_memory"]["acknowledged_version"] == 1
        checkout = Path(self.spaces[author]["cwd"])
        (checkout / "feature.txt").write_text("implemented\n")
        self.git(checkout, "add", "feature.txt")
        self.git(checkout, "commit", "-qm", "implementation")
        commit = self.git(checkout, "rev-parse", "HEAD")
        self.report(
            author,
            "submit_result",
            contract_hash=context["attempt"]["contract_hash"],
            commit=commit,
            artifacts={},
            summary="Fixture implementation",
        )
        revision = self.snapshot()["tasks"][0]["revision"]
        integration = self.apply(
            "request_verification", attempt=author, expected_revision=revision
        )

        def integration_state(state):
            return next(
                (
                    i
                    for i in self.snapshot()["integrations"]
                    if i["id"] == integration["id"] and i["state"] == state
                ),
                None,
            )

        checked = wait_for(
            "real Git candidate and independent checks",
            lambda: integration_state("awaiting_review"),
        )
        reviewer = self.apply(
            "request_review",
            integration=integration["id"],
            expected_revision=checked["revision"],
            node="local",
            profile="auto",
        )["attempt"]["id"]
        review_context = self.ready(reviewer)
        assert review_context["current_memory"]["version"] == 2
        proposal = self.apply(
            "propose_memory",
            run=self.run,
            topic="preferences",
            text="Report evidence for every criterion",
            source_attempt=None,
            evidence=["review:workflow-broadcast"],
        )
        self.apply("approve_memory", proposal=proposal["id"], expected_revision=2)
        wait_for(
            "memory broadcast to author and independent reviewer",
            lambda: all(
                (attempt, 3) in self.memory_deliveries for attempt in (author, reviewer)
            ),
        )
        latest_review = wait_for(
            "reviewer memory context",
            lambda: self.worker(reviewer, "context", attempt=reviewer, epoch=1),
        )
        assert latest_review["memory"] == review_context["memory"]
        assert latest_review["current_memory"]["version"] == 3
        assert reviewer != author
        assert review_context["review"]["candidate_oid"] == checked["candidate_oid"]
        assert (
            self.git(Path(self.spaces[reviewer]["cwd"]), "rev-parse", "HEAD")
            == checked["candidate_oid"]
        )
        self.report(
            reviewer,
            "submit_review",
            integration=integration["id"],
            candidate_oid=checked["candidate_oid"],
            contract_hash=checked["contract_hash"],
            approve=True,
            criteria=[
                {
                    "criterion": 0,
                    "passed": True,
                    "evidence": "Read feature.txt in exact candidate",
                }
            ],
            findings=[],
        )
        verified = wait_for(
            "independent review accepted", lambda: integration_state("verified")
        )
        self.apply(
            "apply_integration",
            integration=integration["id"],
            expected_revision=verified["revision"],
        )
        applied = wait_for("owned-ref CAS", lambda: integration_state("applied"))
        assert (
            self.git(self.repo, "rev-parse", f"refs/heads/hs/integration/{self.run}")
            == applied["candidate_oid"]
        )
        assert self.git(self.repo, "rev-parse", "HEAD") != applied["candidate_oid"], (
            "user branch was changed"
        )
        for attempt in (author, reviewer):
            self.apply(
                "stop_attempt",
                attempt=attempt,
                expected_epoch=self.attempt(attempt)["epoch"],
            )
        wait_for(
            "real process exits release capacity",
            lambda: self.snapshot()["active_attempts"] == 0,
        )
        self.apply(
            "complete_run",
            run=self.run,
            expected_revision=self.snapshot()["run"]["revision"],
        )
        assert self.snapshot()["run"]["state"] == "completed"
        deliveries = self.cli(
            self.controller,
            "delivery-status",
            "--credential",
            self.controller / "admin.token",
            "--node",
            "local",
        )
        assert deliveries["state_counts"].get("broker_stored", 0) > 0
        assert len(deliveries["recent"]) <= deliveries["limit"] == 100
        assert self.owner_token not in json.dumps(deliveries)
        assert all(
            "payload" not in row and "wire" not in row for row in deliveries["recent"]
        )
        assert self.creates == self.launches == [author, reviewer]
        assert len(self.registrations) == len(self.deliveries) == 2
        self.stop(self.node_process)
        self.stop(self.controller_process)
        assert not (self.node / "worker.sock").exists()
        assert not (self.controller / "control.sock").exists()
        print(
            "PASS: continuous services, offline dispatch, node restart, native binding, result, checks, review, CAS, stop, completion"
        )


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--stub":
        if sys.argv[2] == "herdr":
            print(
                json.dumps(
                    rpc(
                        os.environ["HS_WORKFLOW_SOCKET"],
                        {"kind": "herdr", "argv": sys.argv[3:]},
                    )
                )
            )
        else:
            rpc(
                os.environ["HS_WORKFLOW_SOCKET"],
                {
                    "kind": "model",
                    "cwd": os.getcwd(),
                    "attempt": os.environ["HERDR_SUPERVISOR_ATTEMPT"],
                },
            )
            while True:
                time.sleep(1)
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--nats-server", required=True, type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="hs-workflow-") as directory:
        nats = shutil.which(str(args.nats_server))
        assert nats, "NATS server is required; this test never silently skips"
        fixture = Fixture(Path(directory), args.binary.resolve(), Path(nats).resolve())
        try:
            fixture.setup()
            fixture.workflow()
        except Exception:
            for log in fixture.root.glob("*.log"):
                print(
                    f"--- {log.name} ---\n{log.read_text()[-12000:]}", file=sys.stderr
                )
            raise
        finally:
            fixture.close()


if __name__ == "__main__":
    main()
