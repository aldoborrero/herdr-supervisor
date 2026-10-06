#!/usr/bin/env python3
"""Opt-in acceptance through already prepared, isolated real services.

Default: print a plan without reading credentials, opening sockets or starting
processes. --execute starts real model sessions and may incur provider charges.
It requires explicit operator authorization in addition to this command flag.
No fake provider, worker API response or model result is supplied by this driver.
"""

import argparse
import hashlib
import json
import os
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import time
import uuid
from contextlib import closing
from pathlib import Path


class Failure(RuntimeError):
    pass


def require(ok, message):
    if not ok:
        raise Failure(message)


def read_json(path, limit=262144):
    with open(path, "rb") as stream:
        raw = stream.read(limit + 1)
    require(len(raw) <= limit, "JSON document exceeds limit")
    return json.loads(raw)


def plan(spec):
    require(isinstance(spec, dict), "specification must be an object")
    require(
        set(spec) <= {"root", "binary", "runtime", "profile_args", "duration_secs"},
        "unknown specification field",
    )
    root = Path(spec["root"])
    binary = Path(spec["binary"])
    require(
        root.is_absolute()
        and ".." not in root.parts
        and root.name.startswith("herdr-supervisor-live."),
        "use an absolute dedicated herdr-supervisor-live.NAME root",
    )
    require(
        binary.is_absolute() and ".." not in binary.parts,
        "absolute supervisor binary required",
    )
    runtime = spec["runtime"]
    allowed = {
        "codex": {"--model", "-m"},
        "claude": {"--model", "--effort"},
        "pi": {"--model", "--provider", "--thinking"},
    }
    require(runtime in allowed, "runtime must be codex, claude or pi")
    args = spec.get("profile_args", [])
    require(
        isinstance(args, list) and len(args) <= 8 and len(args) % 2 == 0,
        "profile arguments must be bounded flag/value pairs",
    )
    for flag, value in zip(args[::2], args[1::2]):
        require(
            flag in allowed[runtime]
            and isinstance(value, str)
            and 0 < len(value) <= 256
            and not value.startswith("-")
            and not any(ord(c) < 32 for c in value),
            "only model/tuning profile arguments are allowed",
        )
    duration = spec.get("duration_secs", 600)
    require(
        type(duration) is int and 120 <= duration <= 1800,
        "duration must be 120..1800 seconds",
    )
    return {
        "version": 1,
        "root": str(root),
        "binary": str(binary),
        "runtime": runtime,
        "profile_args": args,
        "duration_secs": duration,
        "controller": str(root / "controller"),
        "node": str(root / "node"),
        "bundle": str(root / "services"),
        "repository": str(root / "repo"),
        "models_started": False,
        "execution_effects": [
            "create one run in a fresh isolated controller",
            "start one native coordinator with the selected runtime",
            "coordinator dispatches one author and one independent reviewer",
            "observe scoped question, curated memory, verified Git integration and exits",
            "request cancellation on failure; retain evidence and uncertain resources",
        ],
        "policy_limits": {"max_active_attempts": 2, "task_attempt_budget": 1},
        "observed_scope_limits": {
            "tasks": 1,
            "worker_attempts": 2,
            "coordinators_started_by_driver": 1,
        },
        "money_limit": "not_enforced",
        "requires": "fresh private services and authenticated runtime; explicit permission to launch new models",
    }


def private(path, directory=False):
    path = Path(path)
    info = path.lstat()
    require(
        path.resolve() == path
        and info.st_uid == os.getuid()
        and info.st_mode & 0o077 == 0,
        "test state must be canonical, private and owned by this user",
    )
    require(
        stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode),
        "unexpected private state file type",
    )
    return path


def inside(root, value):
    path = Path(value)
    require(
        path.is_absolute()
        and path.resolve() == path
        and path != root
        and path.is_relative_to(root),
        "service path escapes the isolated acceptance root",
    )
    return path


def write_new(path, value):
    raw = (json.dumps(value, indent=2) + "\n").encode()
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())


def rpc(path, value, native=False):
    raw = json.dumps(value, separators=(",", ":")).encode() + b"\n"
    require(len(raw) <= 300000, "request exceeds supervisor frame limit")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(10)
        connection.connect(str(path))
        connection.sendall(raw)
        with connection.makefile("rb") as reader:
            raw = reader.readline(4 * 1024 * 1024 + 1)
    require(
        raw.endswith(b"\n") and len(raw) <= 4 * 1024 * 1024,
        "incomplete or oversized service response",
    )
    response = json.loads(raw)
    # Never print a remote error body: it may contain provider or token details.
    require(
        not response.get("error"),
        "service rejected the request; inspect private service logs",
    )
    if native:
        require(response.get("id") == value["id"], "native response identity changed")
        return response["result"]
    require("data" in response, "supervisor returned no data")
    return response["data"]


def readonly(path):
    private(path)
    return closing(sqlite3.connect(Path(path).as_uri() + "?mode=ro", uri=True))


class Live:
    def __init__(self, prepared):
        self.plan = prepared
        self.root = Path(prepared["root"])
        self.controller = self.root / "controller"
        self.node = self.root / "node"
        self.bundle = self.root / "services"
        self.repo = self.root / "repo"
        self.run = None
        self.token = None
        self.launch = None
        self.nonce = uuid.uuid4().hex

    def command(self, argv, cwd=None):
        result = subprocess.run(
            argv, cwd=cwd, capture_output=True, timeout=30, check=False
        )
        require(
            result.returncode == 0, "acceptance command failed; inspect private state"
        )
        return result.stdout.decode().strip()

    def git(self, *args):
        return self.command(
            [
                shutil.which("git"),
                "-c",
                "core.fsmonitor=false",
                "-C",
                str(self.repo),
                *args,
            ]
        )

    def call(self, op, **params):
        return rpc(
            self.controller / "control.sock", {"op": op, "token": self.token, **params}
        )

    def apply(self, method, **params):
        request = {
            "op": "apply",
            "token": self.token,
            "operation_id": str(uuid.uuid4()),
            "command": {"method": method, "params": params},
        }
        # Freeze the exact identity/payload before the first call, so operators
        # can inspect uncertain creation/start without inventing a new operation.
        audit = dict(request)
        del audit["token"]
        write_new(self.root / ("request-" + request["operation_id"] + ".json"), audit)
        for retry in range(3):
            try:
                return rpc(self.controller / "control.sock", request)
            except (OSError, TimeoutError):
                if retry == 2:
                    raise Failure(
                        "uncertain API response; exact request retained for reconciliation"
                    ) from None
        raise AssertionError("unreachable")

    def page(self, collection):
        result, cursor = [], None
        while True:
            page = self.call(
                "run_page", run=self.run, collection=collection, cursor=cursor, limit=50
            )
            for row in page["items"]:
                require(
                    row.get("record") is None,
                    "acceptance record exceeds expected bounded shape",
                )
                result.append(row["value"])
            cursor = page["next_cursor"]
            if cursor is None:
                return result

    def preflight(self):
        private(self.root, True)
        for path in (self.controller, self.node, self.bundle, self.repo):
            inside(self.root, path)
        private(self.controller, True)
        private(self.node, True)
        private(self.bundle, True)
        require(
            not (self.root / "acceptance-evidence.json").exists(),
            "use a new acceptance root",
        )
        intent = read_json(private(self.bundle / "intent.json"))
        spec = intent["spec"]
        require(
            intent["controller"] == str(self.controller)
            and intent["bundle"] == str(self.bundle)
            and spec["node_state"] == str(self.node),
            "bundle state does not match the test plan",
        )
        require(
            spec["repositories"] == {str(self.repo): str(self.repo)},
            "use only the isolated repository in this bundle",
        )
        inside(self.root, spec["worktree_root"])
        self.node_name = spec["node"]
        self.config = read_json(private(self.bundle / "launch.json"))
        require(
            self.config == spec["native"],
            "launch configuration differs from the frozen bundle",
        )
        require(
            self.config["supervisor_binary"] == self.plan["binary"],
            "supervisor binary differs from the reviewed plan",
        )
        for field in ("herdr_socket", "mailer_state", "mailer_admin_credential"):
            inside(self.root, self.config[field])
        require(
            self.plan["runtime"] != "claude"
            or self.config["claude_development_channel"] is True,
            "Claude acceptance requires explicit channel opt-in in the prepared bundle",
        )
        with readonly(self.controller / "control.sqlite") as db:
            require(
                db.execute("SELECT count(*) FROM runs").fetchone()[0] == 0,
                "controller contains existing runs",
            )
            require(
                db.execute("SELECT count(*) FROM nodes WHERE active=1").fetchone()[0]
                == 1,
                "acceptance requires exactly one enrolled test node",
            )
        with readonly(self.node / "node.sqlite") as db:
            require(
                db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 0,
                "node contains existing attempts",
            )
        snapshot = rpc(
            self.config["herdr_socket"],
            {"id": str(uuid.uuid4()), "method": "session.snapshot", "params": {}},
            native=True,
        )["snapshot"]
        require(
            snapshot["protocol"] >= 22 and snapshot["agents"] == [],
            "native test server has existing agents or incompatible protocol",
        )
        for pane in snapshot["panes"]:
            if Path(pane["cwd"]) != self.root:
                inside(self.root, pane["cwd"])
        diagnostic = json.loads(
            self.command(
                [
                    self.plan["binary"],
                    "control",
                    "--state",
                    str(self.controller),
                    "check-local",
                    "--bundle",
                    str(self.bundle),
                ]
            )
        )
        require(
            diagnostic["checks_passed"] is True,
            "prepared test services are not healthy",
        )
        require(
            self.git("status", "--porcelain", "--untracked-files=all") == "",
            "test repository is not clean",
        )
        require(
            self.git("rev-list", "--count", "HEAD") == "1",
            "use a fresh one-commit test repository",
        )
        require(
            not (self.repo / "acceptance.txt").exists(),
            "acceptance result already exists",
        )
        self.base = self.git("rev-parse", "HEAD")
        self.token = private(self.controller / "admin.token").read_text().strip()

    def execute(self):
        self.preflight()
        write_new(self.root / "acceptance-plan.json", self.plan)
        check = self.root / "check-acceptance.py"
        with open(check, "x") as stream:
            stream.write(
                "from pathlib import Path\nassert Path('acceptance.txt').read_bytes() == "
                + repr((self.nonce + "\n").encode())
                + "\n"
            )
        check.chmod(0o600)
        objective = (
            "Native acceptance: create exactly ONE code task on node "
            + self.node_name
            + ", profile acceptance. Its author must add only acceptance.txt containing exactly "
            + self.nonce
            + " followed by one newline, commit it and submit through the scoped API. Require the acceptance check "
            "and select preferences in the task's memory_topics. "
            "The worker must first ask_question whether to preserve this exact contract; answer through answer_question. "
            "Require it to propose a preferences memory note with evidence from this task; inspect and approve that note, "
            "then have the worker read and acknowledge the new memory version. Dispatch ONE independent reviewer using "
            "the same profile. Review and verify the exact candidate, integrate only with valid evidence, stop both "
            "attempts and complete_run. Do not create additional tasks or attempts, bypass permissions, change global "
            "configuration, publish, or modify files outside the assigned worktree. Observe the configured deadline."
        )
        policy = {
            "max_active": 2,
            "reserved_reviewers": 1,
            "max_attempts": 1,
            "duration_secs": self.plan["duration_secs"],
            "require_review": True,
            "profiles": {
                "acceptance": {
                    "runtime": self.plan["runtime"],
                    "argv": self.plan["profile_args"],
                    "limit": 2,
                }
            },
            "checks": {"acceptance": [sys.executable, str(check)]},
        }
        created = self.apply(
            "create_run",
            project="native-acceptance",
            repo=str(self.repo),
            base_oid=self.base,
            objective=objective,
            policy=policy,
        )
        self.run = created["id"]
        write_new(
            self.root / "acceptance-run.json",
            {
                "run": self.run,
                "base": self.base,
                "nonce_sha256": hashlib.sha256(self.nonce.encode()).hexdigest(),
            },
        )
        self.launch = self.apply(
            "start_coordinator",
            run=self.run,
            expected_revision=created["revision"],
            profile="acceptance",
            config=self.config,
        )["id"]
        deadline = time.monotonic() + self.plan["duration_secs"] + 60
        while time.monotonic() < deadline:
            snapshot = self.call("snapshot", run=self.run)
            require(
                len(snapshot["tasks"]) <= 1 and len(snapshot["attempts"]) <= 2,
                "coordinator exceeded the reviewed acceptance scope",
            )
            require(
                snapshot["run"]["state"] != "cancelled", "acceptance run was cancelled"
            )
            if snapshot["run"]["state"] == "completed":
                return self.evaluate(snapshot)
            require(
                not any(
                    launch["id"] == self.launch and launch["state"] == "exited"
                    for launch in self.page("coordinator_launches")
                ),
                "native coordinator exited before completing the run; inspect its private pane/receipt",
            )
            time.sleep(1)
        raise Failure("native acceptance deadline elapsed")

    def evaluate(self, snapshot):
        require(
            snapshot["run"]["state"] == "completed"
            and snapshot["active_attempts"] == 0,
            "run or processes are unfinished",
        )
        require(
            len(snapshot["tasks"]) == 1
            and snapshot["tasks"][0]["state"] == "integrated",
            "expected one integrated code task",
        )
        attempts = snapshot["attempts"]
        require(
            len(attempts) == 2
            and sorted(a["role"] for a in attempts) == ["reviewer", "worker"],
            "independent author/reviewer attempts are missing",
        )
        require(
            all(
                a["state"] == "stopped"
                and a["resource"]["runtime"] == self.plan["runtime"]
                for a in attempts
            ),
            "native runtime or exit scope differs",
        )
        sessions = [a["resource"]["session"] for a in attempts]
        require(
            len(set(sessions)) == 2
            and len({a["resource"]["worktree"] for a in attempts}) == 2,
            "native sessions or worktrees are not isolated",
        )
        integrations = [i for i in snapshot["integrations"] if i["state"] == "applied"]
        require(
            len(integrations) == 1 and snapshot["checks"] and snapshot["reviews"],
            "verified independent integration is missing",
        )
        integration = integrations[0]
        candidate = integration["candidate_oid"]
        reviewer = next(a for a in attempts if a["role"] == "reviewer")
        checks = [
            c
            for c in snapshot["checks"]
            if c["name"] == "acceptance"
            and c["candidate"] == candidate
            and c["policy_hash"] == integration["policy_hash"]
        ]
        require(
            len(checks) == 1
            and checks[0]["output"]["exit_code"] == 0
            and checks[0]["output"]["timed_out"] is False
            and "checkout_error" not in checks[0],
            "exact acceptance check did not pass",
        )
        require(
            any(
                r["id"] == reviewer["id"]
                and r["candidate"] == candidate
                and r["contract_hash"] == integration["contract_hash"]
                and r["checks_digest"] == integration["checks_digest"]
                and r["approve"] is True
                for r in snapshot["reviews"]
            ),
            "exact independent review is missing",
        )
        launches = self.page("coordinator_launches")
        require(
            len(launches) == 1
            and launches[0]["id"] == self.launch
            and launches[0]["runtime"] == self.plan["runtime"],
            "expected native coordinator is missing",
        )
        sessions.append(launches[0]["native_session"])
        require(len(set(sessions)) == 3, "coordinator reused a worker session")
        for session in sessions:
            uuid.UUID(session)
        with readonly(self.controller / "control.sqlite") as db:
            authors = db.execute(
                "SELECT p.role FROM journal j JOIN principals p ON p.id=j.principal WHERE j.kind='create_task' AND j.aggregate_id=?",
                (snapshot["tasks"][0]["id"],),
            ).fetchall()
            require(
                authors == [("supervisor",)],
                "task was not created by the scoped native coordinator",
            )
        require(
            snapshot["run"]["integration_oid"] == candidate,
            "run candidate differs from applied integration",
        )
        require(
            self.git("rev-parse", "refs/heads/hs/integration/" + self.run) == candidate,
            "owned integration ref differs",
        )
        require(
            self.git("diff", "--name-only", self.base, candidate) == "acceptance.txt",
            "result changed files outside the acceptance contract",
        )
        require(
            self.git("show", candidate + ":acceptance.txt") == self.nonce,
            "result content differs from challenge",
        )
        questions = self.page("questions")
        author = next(a for a in attempts if a["role"] == "worker")
        require(
            any(
                q["attempt"] == author["id"] and q["answer"] is not None
                for q in questions
            ),
            "worker clarification was not resolved",
        )
        with readonly(self.node / "node.sqlite") as db:
            memory = db.execute(
                "SELECT version,acknowledged FROM worker_memory WHERE attempt=?",
                (author["id"],),
            ).fetchone()
            require(
                memory and memory[0] > 0 and memory[0] == memory[1],
                "live memory has no explicit worker acknowledgement",
            )
        require(
            any(
                p["source_attempt"] == author["id"] and p["status"] == "approved"
                for p in self.page("memory_proposals")
            ),
            "worker memory provenance/approval is missing",
        )
        return {
            "status": "passed",
            "run": self.run,
            "runtime": self.plan["runtime"],
            "candidate": candidate,
            "native_sessions": sessions,
            "question_resolved": True,
            "memory_acknowledged": True,
            "model_processing_evidence": "native participants submitted scoped readiness/results/review; driver did not supply them",
            "remote_host_acceptance": False,
        }

    def close_run(self):
        if self.run is None:
            return {
                "status": "no_known_run",
                "uncertain_creation_requests_retained": True,
            }
        try:
            snapshot = self.call("snapshot", run=self.run)
            if snapshot["run"]["state"] not in ("completed", "cancelled"):
                self.apply(
                    "cancel",
                    run=self.run,
                    expected_revision=snapshot["run"]["revision"],
                )
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                snapshot = self.call("snapshot", run=self.run)
                launches = self.page("coordinator_launches")
                if (
                    snapshot["active_attempts"] == 0
                    and len(launches) == 1
                    and launches[0]["id"] == self.launch
                    and launches[0]["state"] == "exited"
                ):
                    return {
                        "status": "recorded_native_exits",
                        "resources_retained": True,
                    }
                time.sleep(1)
        except (Failure, OSError, KeyError):
            pass
        return {
            "status": "unresolved",
            "resources_retained": True,
            "action": "inspect the owned run and retained request files; do not infer exit or reuse capacity",
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("spec", type=Path)
    parser.add_argument(
        "--execute",
        action="store_true",
        help="start real model sessions; requires separately authorized execution",
    )
    args = parser.parse_args()
    prepared = plan(read_json(args.spec, 16384))
    if not args.execute:
        print(json.dumps(prepared, indent=2))
        return
    live = Live(prepared)
    report = {"status": "failed", "runtime": prepared["runtime"]}
    try:
        report = live.execute()
    except (
        Failure,
        OSError,
        ValueError,
        KeyError,
        subprocess.SubprocessError,
        KeyboardInterrupt,
    ) as error:
        # Details remain in private service state. Do not echo provider output.
        report["reason"] = (
            str(error) if isinstance(error, Failure) else type(error).__name__
        )
    finally:
        cleanup = live.close_run()
    report["cleanup"] = cleanup
    if report["status"] == "passed" and cleanup["status"] != "recorded_native_exits":
        report["status"] = "incomplete"
    if live.run is not None:
        write_new(live.root / "acceptance-evidence.json", report)
    print(json.dumps(report, indent=2))
    if report["status"] != "passed":
        raise SystemExit(1)


if __name__ == "__main__":
    main()
