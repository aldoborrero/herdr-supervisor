#!/usr/bin/env python3
"""Continuous controller/node acceptance test; no live Herdr, mailer or model.

Run with --binary PATH --nats-server PATH. Uses only the Python standard library.
Actual services own all state changes; the fixture never writes their databases.
Git, JetStream, worker IPC, wrappers, process termination and restart are real.
"""

import argparse
import hashlib
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


class NativeEventsServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    """Native protocol fixture, separate from the CLI/model stub transport."""

    daemon_threads = True

    def __init__(self, path):
        self.lock = threading.Lock()
        self.clients = {}
        self.subscriptions = 0
        self.snapshots = 0
        self.errors = []

        class Handler(socketserver.StreamRequestHandler):
            def handle(handler):
                stream = handler.request
                stream.settimeout(5)
                try:
                    request = json.loads(handler.rfile.readline(262145))
                    method = request["method"]
                    if method == "events.subscribe":
                        selections = request["params"]["subscriptions"]
                        assert all(
                            s["type"] != "pane.output_matched" for s in selections
                        )
                        with self.lock:
                            self.clients[stream] = {
                                s["pane_id"] for s in selections if "pane_id" in s
                            }
                            self.subscriptions += 1
                        stream.sendall(
                            json.dumps(
                                {
                                    "id": request["id"],
                                    "result": {"type": "subscription_started"},
                                }
                            ).encode()
                            + b"\n"
                        )
                        # Wait for owned subscription cancellation, not a timer
                        # that would manufacture the reconnect under test.
                        stream.settimeout(None)
                        assert stream.recv(1) == b""
                    elif method == "session.snapshot":
                        # Empty observation deliberately proves absence is not
                        # accepted as exit of the still-running native wrapper.
                        value = {
                            "version": "0.9.1",
                            "protocol": 22,
                            "workspaces": [],
                            "tabs": [],
                            "panes": [],
                            "layouts": [],
                            "agents": [],
                        }
                        stream.sendall(
                            json.dumps(
                                {
                                    "id": request["id"],
                                    "result": {
                                        "type": "session_snapshot",
                                        "snapshot": value,
                                    },
                                }
                            ).encode()
                            + b"\n"
                        )
                        with self.lock:
                            if request["id"] != "supervisor-check":
                                assert self.subscriptions > 0, (
                                    "event reconciliation snapshot preceded subscription"
                                )
                            self.snapshots += 1
                    else:
                        raise AssertionError(f"unexpected native method {method}")
                except (BrokenPipeError, ConnectionResetError):
                    pass  # Target changes/shutdown may cancel a response.
                except Exception as error:  # noqa: BLE001 -- preserve fixture failures
                    self.errors.append(str(error))
                finally:
                    with self.lock:
                        self.clients.pop(stream, None)

        super().__init__(str(path), Handler)
        self.thread = threading.Thread(target=self.serve_forever, daemon=True)
        self.thread.start()

    def has_pane(self, pane):
        with self.lock:
            return any(pane in panes for panes in self.clients.values())

    def interrupt_with_false_exit(self, pane):
        with self.lock:
            streams = list(self.clients)
        assert streams
        wire = (
            json.dumps({"event": "pane_exited", "data": {"pane_id": pane}}).encode()
            + b"\n"
        )
        wire += json.dumps({"error": {"code": "events_lost"}}).encode() + b"\n"
        for stream in streams:
            stream.sendall(wire)

    def close(self):
        self.shutdown()
        self.thread.join(timeout=5)
        with self.lock:
            streams = list(self.clients)
        for stream in streams:
            try:
                stream.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        self.server_close()


class Fixture:
    def __init__(self, root, binary, nats, artifact=False):
        self.root = root
        self.binary = str(binary)
        self.nats = str(nats)
        self.artifact_mode = artifact
        self.controller = root / "controller"
        self.node = root / "node"
        self.repo = root / "repo"
        self.mailer = root / "mailer"
        self.mailer.mkdir()
        self.children = []
        self.servers = []
        self.native_events = NativeEventsServer(root / "native-events.sock")
        self.servers.append(self.native_events)
        self.spaces = {}
        self.models = {}
        self.registrations = {}
        self.deliveries = {}
        self.memory_deliveries = {}
        self.creates = []
        self.launches = []
        self.wrappers = {}
        self.removals = []
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
        if args[:2] == ["worktree", "remove"]:
            assert len(args) == 4 and args[2] == "--workspace", args
            attempt = next(
                a for a, s in self.spaces.items() if s["workspace_id"] == args[3]
            )
            assert self.wrappers[attempt].poll() is not None
            assert attempt not in self.removals, "duplicate removal"
            self.git(self.repo, "worktree", "remove", self.spaces[attempt]["cwd"])
            del self.spaces[attempt]
            self.removals.append(attempt)
            return {"result": {}}
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
                        if a in self.spaces and self.wrappers[a].poll() is None
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
            self.wrappers[attempt] = self.spawn(
                f"wrapper-{attempt}", shlex.split(args[3]), cwd=space["cwd"]
            )
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
            return {
                "protocol": 2,
                "idempotentSend": True,
                "recipientPrecondition": True,
            }
        if request["op"] == "doctor":
            assert request["token"] == self.notifier_token
            return {
                "sqlite": "ok",
                "owner": self.owner,
                "agent": "notifier",
                "session": self.notifier_session,
                "epoch": 1,
            }
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
            if self.artifact_mode and self.attempt(attempt)["role"] == "reviewer":
                inputs = (
                    Path(self.spaces[attempt]["cwd"])
                    / f".herdr-supervisor-artifacts-{attempt}"
                )
                assert (inputs / "evidence.bin").read_bytes() == bytes(range(256)) * 65
                assert str(inputs) in request["body"]
                assert "submit_artifact_review" in request["body"]
            self.deliveries[attempt] = request
        assert self.bootstrap_gate.wait(timeout=10), "test did not release bootstrap"
        return {
            "id": uid(),
            "recipientSession": receiver["id"],
            "recipientEpoch": receiver["epoch"],
        }

    def start_node(self):
        self.node_process = self.spawn("node", self.prepared["commands"]["node"])
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
            self.file(
                f"{user}-creds.json",
                {"user": user, "password": f"{user}-test-password"},
            )
        mailer_admin = self.file("mailer-admin.token", uid() + uid())
        self.notifier_token = uid() + uid()
        self.notifier_session = uid()
        self.file("notifier.token", self.notifier_token)
        setup_spec = self.file(
            "local-services.json",
            {
                "node": "local",
                "node_state": str(self.node),
                "max_active": 2,
                "grant_expires_at": int(time.time()) + 3600,
                "repositories": {"/fixture": str(self.repo)},
                "worktree_root": str(self.root / "worktrees"),
                "check_timeout_secs": 10,
                "broker": {
                    "url": f"nats://127.0.0.1:{port}",
                    "admin_credentials": str(self.root / "admin-creds.json"),
                    "controller_credentials": str(self.root / "controller-creds.json"),
                    "node_credentials": str(self.root / "node-creds.json"),
                    "private_network_plaintext": True,
                },
                "native": {
                    "herdr_binary": str(self.root / "herdr-stub"),
                    "herdr_socket": str(self.root / "native-events.sock"),
                    "supervisor_binary": self.binary,
                    "mailer_state": str(self.mailer),
                    "mailer_admin_credential": str(mailer_admin),
                    "codex_binary": str(self.root / "model-stub"),
                    "mailer_runtime_binary": str(self.root / "unused-runtime"),
                    "claude_development_channel": False,
                },
                "notifier_credential": str(self.root / "notifier.token"),
            },
        )
        setup_args = [
            "setup-local",
            "--spec",
            setup_spec,
            "--bundle",
            self.root / "services",
        ]
        self.prepared = self.cli(self.controller, *setup_args, "--provision")
        assert self.prepared["owner"] == self.owner
        assert self.prepared["broker_provisioned_this_invocation"] is True
        assert self.prepared["services_started"] is False
        assert self.prepared["models_started"] is False
        replay = self.cli(self.controller, *setup_args)
        assert replay["commands"] == self.prepared["commands"]
        assert replay["broker_provisioned_this_invocation"] is False
        self.controller_process = self.spawn(
            "controller", self.prepared["commands"]["controller"]
        )
        wait_for("controller API", lambda: (self.controller / "control.sock").exists())
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
                    "test": [
                        "sh",
                        "-c",
                        'test "$(wc -c < evidence.bin)" -eq 16640'
                        if self.artifact_mode
                        else 'test "$(cat feature.txt)" = implemented',
                    ]
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
        before = self.snapshot()["run"]
        diagnostic = self.cli(
            self.controller, "check-local", "--bundle", self.root / "services"
        )
        assert diagnostic["checks_passed"] is True, diagnostic
        assert diagnostic["model_readiness"] == "not_checked"
        assert diagnostic["models_started"] is False
        assert diagnostic["services_started"] is False
        assert "model_authentication" in diagnostic["not_checked"]
        assert self.snapshot()["run"] == before
        assert not self.registrations and not self.models and not self.launches
        assert self.notifier_token not in json.dumps(diagnostic)

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
                "kind": "artifact" if self.artifact_mode else "code",
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
        wait_for(
            "native status subscription for the exact author pane",
            lambda: self.native_events.has_pane(self.spaces[author]["pane_id"]),
        )
        before_native = self.attempt(author)
        snapshots = self.native_events.snapshots
        self.native_events.interrupt_with_false_exit(self.spaces[author]["pane_id"])
        wait_for(
            "native event loss reconnects and takes a fresh snapshot",
            lambda: self.native_events.snapshots > snapshots,
        )
        after_native = self.attempt(author)
        assert after_native["state"] == before_native["state"]
        assert after_native["epoch"] == before_native["epoch"]
        assert after_native["resource"] == before_native["resource"]
        assert self.snapshot()["active_attempts"] == 1
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
        evidence = self.root / "artifact-fixture.bin"
        evidence_bytes = bytes(range(256)) * 65
        evidence.write_bytes(evidence_bytes)
        upload_args = [
            "worker",
            "--credential",
            self.node / "workers" / author / "worker.token",
            "--attempt",
            author,
            "--epoch",
            "1",
        ]
        uploaded = self.cli(
            self.node, *upload_args, "upload-artifact", "evidence.bin", evidence
        )
        assert uploaded["sha256"] == hashlib.sha256(evidence_bytes).hexdigest()
        assert len(uploaded["events"]) == 2
        assert not uploaded["content_verified"] and not uploaded["task_accepted"]
        repeated = self.cli(
            self.node, *upload_args, "upload-artifact", "evidence.bin", evidence
        )
        assert repeated["events"] == uploaded["events"]
        verified = wait_for(
            "artifact content reaches controller",
            lambda: (
                status
                if (
                    status := self.cli(
                        self.node,
                        *upload_args,
                        "artifact-status",
                        "evidence.bin",
                        uploaded["sha256"],
                    )
                )["content_verified"]
                else None
            ),
        )
        assert not verified["task_accepted"]
        checkout = Path(self.spaces[author]["cwd"])
        commit = None
        if not self.artifact_mode:
            (checkout / "feature.txt").write_text("implemented\n")
            self.git(checkout, "add", "feature.txt")
            self.git(checkout, "commit", "-qm", "implementation")
            commit = self.git(checkout, "rev-parse", "HEAD")
        self.report(
            author,
            "submit_result",
            contract_hash=context["attempt"]["contract_hash"],
            commit=commit,
            artifacts={"evidence.bin": uploaded["sha256"]}
            if self.artifact_mode
            else {},
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
        if self.artifact_mode:
            assert checked["candidate_oid"] is None
            assert review_context["review"]["artifact"] == checked["artifact"]
            review_identity = {"manifest_hash": checked["artifact"]["manifest_hash"]}
            review_base = checked["base_oid"]
        else:
            assert review_context["review"]["candidate_oid"] == checked["candidate_oid"]
            review_identity = {"candidate_oid": checked["candidate_oid"]}
            review_base = checked["candidate_oid"]
        assert (
            self.git(Path(self.spaces[reviewer]["cwd"]), "rev-parse", "HEAD")
            == review_base
        )
        self.report(
            reviewer,
            "submit_artifact_review" if self.artifact_mode else "submit_review",
            integration=integration["id"],
            **review_identity,
            contract_hash=checked["contract_hash"],
            approve=True,
            criteria=[
                {
                    "criterion": 0,
                    "passed": True,
                    "evidence": "Read the exact manifest and evidence.bin"
                    if self.artifact_mode
                    else "Read feature.txt in exact candidate",
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
        applied = wait_for(
            "verified candidate acceptance", lambda: integration_state("applied")
        )
        if self.artifact_mode:
            assert self.snapshot()["tasks"][0]["state"] == "completed"
            assert self.snapshot()["run"]["integration_oid"] == checked["base_oid"]
            assert self.git(self.repo, "rev-parse", "HEAD") == checked["base_oid"]
            assert (
                self.git(
                    self.repo,
                    "for-each-ref",
                    "--format=%(refname)",
                    f"refs/heads/hs/integration/{self.run}",
                )
                == ""
            )
        else:
            assert (
                self.git(
                    self.repo, "rev-parse", f"refs/heads/hs/integration/{self.run}"
                )
                == applied["candidate_oid"]
            )
            assert (
                self.git(self.repo, "rev-parse", "HEAD") != applied["candidate_oid"]
            ), "user branch was changed"
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
        rejected = self.cli(self.controller, "reception-status")
        assert rejected["state_counts"] == {}
        assert rejected["recent"] == []
        pruned = self.cli(self.controller, "reception-prune", "--before", "1")
        assert pruned == {"removed": 0, "before": 1, "pending_preserved": True}
        assert self.creates == self.launches == [author, reviewer]
        assert len(self.registrations) == len(self.deliveries) == 2
        # Controller application must make the return trip before local retirement.
        executor = self.root / "services/executor.json"
        cleanup_epochs = {}
        for attempt in (author, reviewer):
            epoch = self.attempt(attempt)["epoch"]
            cleanup_epochs[attempt] = epoch

            def stop_received(attempt=attempt, epoch=epoch):
                report = self.cli(
                    self.node,
                    "cleanup-inspect",
                    "--executor",
                    executor,
                    "--expected-epoch",
                    epoch,
                    attempt,
                )
                return any(
                    c["check"] == "controller_stop_applied" and c["status"] == "passed"
                    for c in report["checks"]
                )

            wait_for("node has controller stop receipt", stop_received)
        self.stop(self.node_process)
        self.stop(self.controller_process)
        wait_for(
            "native event streams close on shutdown",
            lambda: not self.native_events.clients,
        )
        assert not self.native_events.errors, self.native_events.errors
        assert not (self.node / "worker.sock").exists()
        assert not (self.controller / "control.sock").exists()
        if not self.artifact_mode:
            for attempt in (author, reviewer):
                assert self.wrappers[attempt].wait(timeout=5) == 0
                path = Path(self.spaces[attempt]["cwd"])
                head = self.git(path, "rev-parse", "HEAD")
                decision = self.file(
                    f"retire-{attempt}.json",
                    {
                        "operation": uid(),
                        "attempt": attempt,
                        "expected_epoch": cleanup_epochs[attempt],
                        "expected_head": head,
                        "processes_confirmed_stopped": True,
                        "retain_commits_remove_checkout": True,
                        "reason": "Fixture wrapper and owned loop stopped; preserve commits",
                    },
                )
                args = ("cleanup-retire", "--executor", executor, "--request", decision)
                receipt = self.cli(self.node, *args)
                assert receipt["state"] == "removed", receipt
                assert self.cli(self.node, *args) == receipt
                assert not path.exists()
                assert self.git(self.repo, "rev-parse", receipt["retained_ref"]) == head
            assert self.removals == [author, reviewer]
            decision = self.file(
                "retire-verification.json",
                {
                    "operation": uid(),
                    "integration": applied["id"],
                    "expected_revision": applied["revision"],
                    "expected_candidate": applied["candidate_oid"],
                    "processes_confirmed_stopped": True,
                    "retain_commits_remove_checkout": True,
                    "reason": "Fixture controller and all owned checks stopped",
                },
            )
            args = (
                "cleanup-verification",
                "--credential",
                self.controller / "admin.token",
                "--verifier",
                self.root / "services/repositories.json",
                "--request",
                decision,
            )
            receipt = self.cli(self.controller, *args)
            assert receipt["state"] == "removed", receipt
            assert self.cli(self.controller, *args) == receipt
            assert not (
                self.controller / "verification" / applied["id"] / "checkout"
            ).exists()
            assert (
                self.git(self.repo, "rev-parse", receipt["retained_ref"])
                == applied["candidate_oid"]
            )
        print(
            f"PASS: continuous services, offline dispatch, node restart, native binding, result, checks, review, {'artifact acceptance' if self.artifact_mode else 'Git CAS'}, stop, completion"
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
    parser.add_argument("--artifact", action="store_true")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="hs-workflow-") as directory:
        nats = shutil.which(str(args.nats_server))
        assert nats, "NATS server is required; this test never silently skips"
        fixture = Fixture(
            Path(directory), args.binary.resolve(), Path(nats).resolve(), args.artifact
        )
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
