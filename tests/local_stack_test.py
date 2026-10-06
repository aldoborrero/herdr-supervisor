"""Isolated orchestration boundary checks; no providers or host services."""

import os
import sqlite3
import sys
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "dev"))
import codex_lab
import local_stack as stack


class LocalStackTests(unittest.TestCase):
    def test_codex_trust_is_persistent_and_confined_to_lab(self):
        cwd = self.root / 'owned "quoted"'
        cwd.mkdir()
        original = ["--approve-for-me", "--cd", str(cwd), "--model", "gpt-6-astra"]
        self.assertEqual(codex_lab.workspace(self.root, original), cwd)
        (self.root / "codex").mkdir()
        config = self.root / "codex/config.toml"
        stack.write(config, 'cli_auth_credentials_store = "file"\n')
        codex_lab.trust(self.root, cwd)
        codex_lab.trust(self.root, cwd)
        import tomllib

        self.assertEqual(
            tomllib.loads(config.read_text())["projects"],
            {str(cwd): {"trust_level": "trusted"}},
        )
        self.assertEqual(config.stat().st_mode & 0o777, 0o600)
        self.assertIsNone(codex_lab.workspace(self.root, ["queue", "id"]))
        with self.assertRaises(RuntimeError):
            codex_lab.workspace(self.root, ["--approve-for-me", "--cd", "/tmp"])

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="herdr-supervisor-live.")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_runtime_environment_excludes_parent_sessions_and_control_capabilities(
        self,
    ):
        parent = {
            "HOME": "/foreign",
            "CODEX_HOME": "/foreign/codex",
            "HERDR_SOCKET_PATH": "/foreign/herdr.sock",
            "HERDR_SUPERVISOR_CREDENTIAL": "/foreign/admin",
            "HERDR_MAILER_CONFIG": "/foreign/mailer.json",
            "CODEX_THREAD_ID": "foreign",
            "OPENAI_API_KEY": "parent-secret",
            "PATH": "/tools",
        }
        config = {
            "shell": "/tools/bash",
            "herdr_socket": str(self.root / "herdr.sock"),
            "mailer": "/mailer/bin/herdr-mailer",
            "supervisor": "/supervisor/bin/herdr-supervisor",
        }
        with patch.dict(os.environ, parent, clear=True):
            env = stack.environment(self.root, config)
        for name in (
            "HERDR_SUPERVISOR_CREDENTIAL",
            "HERDR_MAILER_CONFIG",
            "CODEX_THREAD_ID",
            "OPENAI_API_KEY",
        ):
            self.assertNotIn(name, env)
        self.assertTrue(env["CODEX_HOME"].startswith(str(self.root)))
        self.assertEqual(env["HERDR_SOCKET_PATH"], config["herdr_socket"])
        self.assertNotIn("parent-secret", str(env))

    def test_stop_requires_proven_attempt_and_coordinator_exits(self):
        (self.root / "controller").mkdir()
        with closing(sqlite3.connect(self.root / "controller/control.sqlite")) as db:
            db.executescript(
                "CREATE TABLE attempts(active INTEGER); CREATE TABLE coordinator_launches(state TEXT); INSERT INTO attempts VALUES(1); INSERT INTO coordinator_launches VALUES('exited');"
            )
            with self.assertRaises(RuntimeError):
                stack.require_idle(self.root)
            db.execute("UPDATE attempts SET active=0")
            db.execute("UPDATE coordinator_launches SET state='launching'")
            db.commit()
            with self.assertRaises(RuntimeError):
                stack.require_idle(self.root)
            db.execute("UPDATE coordinator_launches SET state='exited'")
            db.commit()
            stack.require_idle(self.root)

    def test_lab_refuses_public_or_aliased_roots_and_writes_exclusively(self):
        self.assertEqual(stack.lab(self.root), self.root)
        alias = self.root / "herdr-supervisor-live.alias"
        alias.symlink_to(self.root)
        with self.assertRaises(RuntimeError):
            stack.lab(alias)
        self.root.chmod(0o755)
        with self.assertRaises(RuntimeError):
            stack.lab(self.root)
        self.root.chmod(0o700)
        path = self.root / "private.json"
        stack.write(path, {"retained": True})
        with self.assertRaises(FileExistsError):
            stack.write(path, {"changed": True})
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(stack.read(path), {"retained": True})


if __name__ == "__main__":
    unittest.main()
