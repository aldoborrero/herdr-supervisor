"""No model calls: test the opt-in driver's plan, framing and evidence gates."""

import copy
import socket
import sqlite3
import tempfile
import threading
import unittest
import uuid
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

import live_acceptance as live


class AcceptanceDriverTests(unittest.TestCase):
    def test_local_stack_isolation_and_shutdown_boundaries(self):
        import local_stack_test

        suite = unittest.defaultTestLoader.loadTestsFromModule(local_stack_test)
        result = unittest.TestResult()
        suite.run(result)
        self.assertEqual(result.testsRun, 6)
        self.assertTrue(result.wasSuccessful(), result.errors + result.failures)

    def test_exited_coordinator_fails_without_waiting_for_deadline(self):
        driver = live.Live(live.plan(self.spec))
        driver.node_name = "local"
        driver.config = {}
        driver.base = "base"
        driver.preflight = lambda: None
        operations = []

        def apply(method, **params):
            operations.append(method)
            return (
                {"id": "run", "revision": 1}
                if method == "create_run"
                else {"id": "launch"}
            )

        driver.apply = apply
        driver.call = lambda *args, **kwargs: {
            "run": {"state": "active"},
            "tasks": [],
            "attempts": [],
        }
        driver.page = lambda collection: [{"id": "launch", "state": "exited"}]
        with (
            patch.object(
                live.time, "sleep", side_effect=AssertionError("must fail promptly")
            ),
            self.assertRaisesRegex(live.Failure, "coordinator exited"),
        ):
            driver.execute()
        self.assertEqual(operations, ["create_run", "start_coordinator"])

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="herdr-supervisor-live.")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.spec = {
            "root": str(self.root),
            "binary": "/unused/supervisor",
            "runtime": "codex",
        }

    def test_plan_has_no_process_socket_or_credential_effects(self):
        with (
            patch.object(live.subprocess, "run", side_effect=AssertionError("process")),
            patch.object(live.socket, "socket", side_effect=AssertionError("socket")),
        ):
            for runtime in ("codex", "claude", "pi"):
                plan = live.plan({**self.spec, "runtime": runtime})
                self.assertFalse(plan["models_started"])
                self.assertEqual(plan["policy_limits"]["max_active_attempts"], 2)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_plan_refuses_escape_permission_override_and_unbounded_scope(self):
        invalid = [
            {"root": "/home/aldo"},
            {"binary": "relative"},
            {"runtime": "unknown"},
            {"duration_secs": 0},
            {"duration_secs": 1801},
            {"duration_secs": True},
            {"profile_args": ["--sandbox", "danger-full-access"]},
            {"profile_args": ["--model"]},
            {"profile_args": ["--model", "--danger"]},
            {"raw_token": "must-not-be-accepted"},
        ]
        for fields in invalid:
            with self.subTest(fields=fields), self.assertRaises(live.Failure):
                live.plan({**self.spec, **fields})

    def test_preflight_refuses_foreign_bundle_before_opening_any_service(self):
        for directory in ("controller", "node", "services", "repo"):
            (self.root / directory).mkdir(mode=0o700)
        live.write_new(
            self.root / "services/intent.json",
            {
                "controller": "/foreign/controller",
                "bundle": str(self.root / "services"),
                "spec": {"node_state": str(self.root / "node")},
            },
        )
        driver = live.Live(live.plan(self.spec))
        with (
            patch.object(live, "rpc", side_effect=AssertionError("service")),
            patch.object(live.subprocess, "run", side_effect=AssertionError("process")),
            self.assertRaises(live.Failure),
        ):
            driver.preflight()
        self.assertIsNone(driver.run)
        self.assertFalse((self.root / "acceptance-plan.json").exists())

    def test_rpc_uses_actual_data_envelope_and_rejects_partial_or_secret_errors(self):
        path = self.root / "server.sock"
        replies = [
            b'{"data":{"run":"example"}}\n',
            b'{"error":"private-token-marker"}\n',
            b'{"data":{}}',
        ]
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(path))
        listener.listen()
        listener.settimeout(5)
        self.addCleanup(listener.close)

        def serve():
            for response in replies:
                stream, _ = listener.accept()
                with stream:
                    stream.recv(4096)
                    stream.sendall(response)

        worker = threading.Thread(target=serve)
        worker.start()
        try:
            self.assertEqual(live.rpc(path, {"op": "fixture"}), {"run": "example"})
            for _ in range(2):
                with self.assertRaises(live.Failure) as error:
                    live.rpc(path, {"op": "fixture"})
                self.assertNotIn("private-token-marker", str(error.exception))
        finally:
            worker.join(6)
        self.assertFalse(worker.is_alive())

    def evidence_fixture(self):
        driver = live.Live(live.plan(self.spec))
        driver.run = "run"
        driver.launch = "coordinator"
        driver.base = "base"
        for directory in (driver.controller, driver.node):
            directory.mkdir(mode=0o700)
        with closing(sqlite3.connect(driver.controller / "control.sqlite")) as db:
            db.executescript(
                "CREATE TABLE principals(id TEXT,role TEXT); INSERT INTO principals VALUES('p','supervisor'); CREATE TABLE journal(principal TEXT,kind TEXT,aggregate_id TEXT); INSERT INTO journal VALUES('p','create_task','task');"
            )
        with closing(sqlite3.connect(driver.node / "node.sqlite")) as db:
            db.executescript(
                "CREATE TABLE worker_memory(attempt TEXT,version INTEGER,acknowledged INTEGER); INSERT INTO worker_memory VALUES('author',1,1);"
            )
        (driver.controller / "control.sqlite").chmod(0o600)
        (driver.node / "node.sqlite").chmod(0o600)
        driver.page = lambda collection: {
            "coordinator_launches": [
                {
                    "id": driver.launch,
                    "runtime": "codex",
                    "native_session": str(uuid.uuid4()),
                }
            ],
            "questions": [{"attempt": "author", "answer": "preserve contract"}],
            "memory_proposals": [{"source_attempt": "author", "status": "approved"}],
        }[collection]
        driver.git = lambda *args: {
            "rev-parse": "candidate",
            "diff": "acceptance.txt",
            "show": driver.nonce,
        }[args[0]]
        snapshot = {
            "run": {"state": "completed", "integration_oid": "candidate"},
            "active_attempts": 0,
            "tasks": [{"id": "task", "state": "integrated"}],
            "attempts": [
                {
                    "id": name,
                    "role": role,
                    "state": "stopped",
                    "resource": {
                        "runtime": "codex",
                        "session": str(uuid.uuid4()),
                        "worktree": str(self.root / name),
                    },
                }
                for name, role in (("author", "worker"), ("reviewer", "reviewer"))
            ],
            "integrations": [
                {
                    "state": "applied",
                    "candidate_oid": "candidate",
                    "policy_hash": "policy",
                    "contract_hash": "contract",
                    "checks_digest": "checks",
                }
            ],
            "checks": [
                {
                    "name": "acceptance",
                    "candidate": "candidate",
                    "policy_hash": "policy",
                    "output": {"exit_code": 0, "timed_out": False},
                }
            ],
            "reviews": [
                {
                    "id": "reviewer",
                    "candidate": "candidate",
                    "contract_hash": "contract",
                    "checks_digest": "checks",
                    "approve": True,
                }
            ],
        }
        return driver, snapshot

    def test_evidence_requires_exact_review_check_and_distinct_native_sessions(self):
        driver, snapshot = self.evidence_fixture()
        self.assertEqual(driver.evaluate(snapshot)["status"], "passed")
        self.assertFalse(driver.evaluate(snapshot)["remote_host_acceptance"])
        changes = [
            lambda s: s.update(active_attempts=1),
            lambda s: s["reviews"][0].update(candidate="foreign"),
            lambda s: s["checks"][0]["output"].update(exit_code=1),
            lambda s: s["checks"][0]["output"].update(timed_out=True),
            lambda s: s["attempts"][1]["resource"].update(
                session=s["attempts"][0]["resource"]["session"]
            ),
            lambda s: s["attempts"][1]["resource"].update(runtime="pi"),
            lambda s: s["attempts"][1]["resource"].update(
                worktree=s["attempts"][0]["resource"]["worktree"]
            ),
        ]
        for change in changes:
            candidate = copy.deepcopy(snapshot)
            change(candidate)
            with self.subTest(change=change), self.assertRaises(live.Failure):
                driver.evaluate(candidate)
        with closing(sqlite3.connect(driver.node / "node.sqlite")) as db:
            db.execute("UPDATE worker_memory SET acknowledged=NULL")
            db.commit()
        with self.assertRaises(live.Failure):
            driver.evaluate(snapshot)


if __name__ == "__main__":
    unittest.main()
