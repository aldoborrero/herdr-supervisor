#!/usr/bin/env python3
"""Prepare/start an isolated real-service lab; model execution is a separate step."""

import argparse
import json
import os
import secrets
import shlex
import shutil
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import time
from contextlib import closing
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def require(value, message):
    if not value:
        raise RuntimeError(message)


def write(path, value):
    data = json.dumps(value, indent=2) + "\n" if not isinstance(value, str) else value
    with open(path, "x", encoding="utf-8") as stream:
        os.chmod(path, 0o600)
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())


def read(path):
    return json.loads(Path(path).read_text())


def lab(path):
    root = Path(path)
    require(
        root.is_absolute() and root.resolve() == root,
        "lab root must be canonical and absolute",
    )
    require(
        root.name.startswith("herdr-supervisor-live."),
        "use a dedicated herdr-supervisor-live.NAME root",
    )
    info = root.lstat()
    require(
        stat.S_ISDIR(info.st_mode)
        and info.st_uid == os.getuid()
        and info.st_mode & 0o077 == 0,
        "lab root must be private and owned",
    )
    return root


def environment(root, config):
    env = {
        k: v
        for k, v in os.environ.items()
        if k
        in (
            "PATH",
            "LANG",
            "TERM",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "NIX_SSL_CERT_FILE",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "NO_PROXY",
        )
    }
    env.update(
        {
            "HOME": str(root / "home"),
            "XDG_CONFIG_HOME": str(root / "config"),
            "XDG_STATE_HOME": str(root / "state"),
            "XDG_CACHE_HOME": str(root / "cache"),
            "XDG_RUNTIME_DIR": str(root / "run"),
            "CODEX_HOME": str(root / "codex"),
            "CLAUDE_CONFIG_DIR": str(root / "claude"),
            "PI_CODING_AGENT_DIR": str(root / "pi"),
            "HERDR_CONFIG_PATH": str(root / "herdr.toml"),
            "HERDR_SOCKET_PATH": config["herdr_socket"],
            "HERDR_SUPERVISOR_LAB": str(root),
            "SHELL": config["shell"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_GLOBAL": str(root / "gitconfig"),
        }
    )
    # Mailer invokes codex by name; all native processes must share the lab IPC.
    env["PATH"] = os.pathsep.join(
        [
            str(root / "bin"),
            str(Path(config["mailer"]).parent),
            str(Path(config["supervisor"]).parent),
            env.get("PATH", ""),
        ]
    )
    return env


def call(root, config, argv):
    result = subprocess.run(
        list(map(str, argv)),
        env=environment(root, config),
        cwd=root,
        capture_output=True,
        timeout=45,
        check=False,
    )
    if result.returncode:
        with open(root / "logs/setup-errors.log", "ab") as stream:
            stream.write(result.stderr)
        raise RuntimeError(
            "service setup command failed; inspect private logs/setup-errors.log"
        )
    return result.stdout.decode()


def prepare(args):
    os.umask(0o077)
    root = (
        Path(args.root)
        if args.root
        else Path(tempfile.mkdtemp(prefix="herdr-supervisor-live."))
    )
    if args.root:
        require(
            root.is_absolute()
            and root.resolve() == root
            and root.name.startswith("herdr-supervisor-live."),
            "invalid fresh root",
        )
        root.mkdir(mode=0o700)
    root = lab(root)
    config = {
        key: str(Path(getattr(args, key)).absolute())
        for key in ("supervisor", "mailer", "herdr", "codex", "nats", "compose")
    }
    for value in config.values():
        require(
            Path(value).is_file() and os.access(value, os.X_OK),
            "a required binary is missing",
        )
    config.update(
        runtime=args.runtime,
        shell=shutil.which("bash"),
        herdr_socket=str(root / "config/herdr/sessions/lab/herdr.sock"),
    )
    require(config["shell"], "bash is required")
    if args.runtime != "codex":
        selected = getattr(args, args.runtime)
        require(selected, "provide the selected --pi or --claude executable explicitly")
        selected = Path(selected).absolute()
        require(
            selected.is_file() and os.access(selected, os.X_OK),
            "selected runtime is unavailable",
        )
        config[args.runtime] = str(selected)
    for name in (
        "home",
        "config",
        "state",
        "cache",
        "run",
        "logs",
        "bin",
        "codex",
        "claude",
        "pi",
        "repo",
    ):
        (root / name).mkdir(mode=0o700)
    wrapper = root / "bin/codex"
    write(
        wrapper,
        "#!"
        + config["shell"]
        + "\nexec "
        + shlex.join([sys.executable, str(REPO / "dev/codex_lab.py")])
        + ' "$@"\n',
    )
    wrapper.chmod(0o700)
    if args.runtime == "pi":
        (root / "bin/pi").symlink_to(config["pi"])
    elif args.runtime == "claude":
        claude_wrapper = root / "bin/claude"
        write(
            claude_wrapper,
            "#!"
            + config["shell"]
            + "\nexec "
            + shlex.join([sys.executable, str(REPO / "dev/claude_lab.py")])
            + ' "$@"\n',
        )
        claude_wrapper.chmod(0o700)
    write(
        root / "gitconfig",
        "[user]\n name = Supervisor acceptance\n email = acceptance@example.invalid\n[commit]\n gpgsign = false\n",
    )
    write(
        root / "herdr.toml",
        "onboarding = false\n[terminal]\ndefault_shell = "
        + json.dumps(config["shell"])
        + '\nshell_mode = "non_login"\nnew_cwd = "home"\n[update]\nversion_check = false\nmanifest_check = false\n[session]\nresume_agents_on_restore = false\n',
    )
    if args.use_local_auth:
        source = Path(args.auth_home).expanduser().resolve()
        auth = {
            "codex": (source / ".codex/auth.json", root / "codex/auth.json"),
            "claude": (
                source / ".claude/.credentials.json",
                root / "claude/.credentials.json",
            ),
            "pi": (source / ".pi/agent/auth.json", root / "pi/auth.json"),
        }[args.runtime]
        require(auth[0].is_file(), "selected local authentication cache is absent")
        # Private copy, never logged, committed or written back to the source.
        fd = os.open(auth[1], os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(fd, "wb") as out, open(auth[0], "rb") as source_stream:
            shutil.copyfileobj(source_stream, out)
        if args.runtime == "claude" and (source / ".claude.json").is_file():
            prior = read(source / ".claude.json")
            if prior.get("hasCompletedOnboarding") is True:
                write(
                    root / "claude/.claude.json",
                    {"hasCompletedOnboarding": True, "theme": "dark"},
                )
    write(
        root / "codex/config.toml",
        'cli_auth_credentials_store = "file"\n[projects.'
        + json.dumps(str(root))
        + ']\ntrust_level = "trusted"\n',
    )
    git = shutil.which("git")
    require(git, "git is required")
    call(root, config, [git, "-C", root / "repo", "init", "-q"])
    write(root / "repo/baseline.txt", "Isolated supervisor acceptance.\n")
    call(root, config, [git, "-C", root / "repo", "add", "."])
    call(root, config, [git, "-C", root / "repo", "commit", "-qm", "baseline"])
    call(
        root,
        config,
        [config["supervisor"], "control", "--state", root / "controller", "init"],
    )
    policy = json.loads(
        call(
            root,
            config,
            [
                config["supervisor"],
                "control",
                "--state",
                root / "controller",
                "broker-policy",
                "--node",
                "local",
            ],
        )
    )
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        config["nats_port"] = sock.getsockname()[1]
    users = []
    for role in ("admin", "controller", "node"):
        password = secrets.token_hex(32)
        entry = {"user": role, "password": password}
        if role != "admin":
            entry["permissions"] = (
                policy["controller"]
                if role == "controller"
                else policy["nodes"]["local"]
            )["permissions"]
        users.append(entry)
        write(root / (role + "-creds.json"), {"user": role, "password": password})
    write(
        root / "nats.conf",
        {
            "listen": "127.0.0.1:" + str(config["nats_port"]),
            "jetstream": {
                "store_dir": str(root / "jetstream"),
                "max_file_store": 2147483648,
                "max_memory_store": 8388608,
            },
            "authorization": {"users": users},
        },
    )
    call(root, config, [config["mailer"], "--state", root / "mailer", "init"])
    write(
        root / "mailer-admin.token",
        read(root / "mailer/owner.json")["adminToken"] + "\n",
    )
    native = {
        "herdr_binary": config["herdr"],
        "herdr_socket": config["herdr_socket"],
        "supervisor_binary": config["supervisor"],
        "mailer_state": str(root / "mailer"),
        "mailer_admin_credential": str(root / "mailer-admin.token"),
        "codex_binary": str(wrapper),
        "mailer_runtime_binary": str(
            Path(config["mailer"]).parent / "herdr-mailer-runtime"
        ),
        "claude_development_channel": args.runtime == "claude",
    }
    write(
        root / "services-spec.json",
        {
            "node": "local",
            "node_state": str(root / "node"),
            "max_active": 2,
            "grant_expires_at": int(time.time()) + 86400,
            "repositories": {str(root / "repo"): str(root / "repo")},
            "worktree_root": str(root / "worktrees"),
            "check_timeout_secs": 30,
            "broker": {
                "url": "nats://127.0.0.1:" + str(config["nats_port"]),
                "admin_credentials": str(root / "admin-creds.json"),
                "controller_credentials": str(root / "controller-creds.json"),
                "node_credentials": str(root / "node-creds.json"),
                "private_network_plaintext": True,
            },
            "native": native,
            "notifier_credential": str(root / "notifier.token"),
        },
    )
    write(
        root / "acceptance-spec.json",
        {
            "root": str(root),
            "binary": config["supervisor"],
            "runtime": args.runtime,
            "duration_secs": args.duration,
            "profile_args": ["--model", args.model] if args.model else [],
        },
    )
    write(root / "lab.json", config)
    call(root, config, [config["herdr"], "integration", "install", args.runtime])
    if args.runtime == "codex":
        from codex_lab import install_hook_trust

        install_hook_trust(root, config, environment(root, config))
    print(
        json.dumps(
            {
                "root": str(root),
                "runtime": args.runtime,
                "services_started": False,
                "models_started": False,
                "auth_cache_copied": args.use_local_auth,
            }
        )
    )


def probe(root, config, name):
    if name == "nats":
        with socket.create_connection(
            ("127.0.0.1", config["nats_port"]), timeout=1
        ) as sock:
            require(sock.recv(16).startswith(b"INFO "), "not a NATS server")
    else:
        path = {
            "mailer": root / "mailer/daemon.sock",
            "herdr": Path(config["herdr_socket"]),
            "controller": root / "controller/control.sock",
            "node": root / "node/worker.sock",
        }[name]
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(1)
            sock.connect(str(path))


def service(root, config, name):
    os.umask(0o077)
    if name == "bootstrap":
        if not (root / "notifier.token").exists():
            call(
                root,
                config,
                [
                    config["mailer"],
                    "--state",
                    root / "mailer",
                    "register",
                    "supervisor-notifier",
                    "--runtime",
                    "none",
                    "--output",
                    root / "notifier.token",
                ],
            )
        call(
            root,
            config,
            [
                config["supervisor"],
                "control",
                "--state",
                root / "controller",
                "setup-local",
                "--spec",
                root / "services-spec.json",
                "--bundle",
                root / "services",
                "--provision",
            ],
        )
        print("Local bundle provisioned; no models started.", flush=True)
        return
    if name in ("controller", "node"):
        argv = read(root / "services/commands.json")[name]
    else:
        argv = {
            "nats": [config["nats"], "-c", str(root / "nats.conf")],
            "mailer": [config["mailer"], "--state", str(root / "mailer"), "daemon"],
            "herdr": [config["herdr"], "--session", "lab", "server"],
        }[name]
    os.chdir(root)
    os.execve(argv[0], argv, environment(root, config))


def require_idle(root):
    database = root / "controller/control.sqlite"
    if not database.exists():
        return
    with closing(sqlite3.connect(database.as_uri() + "?mode=ro", uri=True)) as db:
        active = db.execute("SELECT count(*) FROM attempts WHERE active=1").fetchone()[
            0
        ]
        launches = db.execute(
            "SELECT count(*) FROM coordinator_launches WHERE state<>'exited'"
        ).fetchone()[0]
        require(
            not active and not launches,
            "cancel the owned run and reconcile native exits before stopping services",
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    prepare_parser = sub.add_parser("prepare")
    prepare_parser.add_argument("--root")
    for key in ("supervisor", "mailer", "herdr", "codex", "nats", "compose"):
        prepare_parser.add_argument("--" + key, required=True)
    prepare_parser.add_argument(
        "--runtime", choices=["codex", "claude", "pi"], default="codex"
    )
    prepare_parser.add_argument("--duration", type=int, default=600)
    prepare_parser.add_argument("--model")
    prepare_parser.add_argument("--pi")
    prepare_parser.add_argument("--claude")
    prepare_parser.add_argument("--use-local-auth", action="store_true")
    prepare_parser.add_argument("--auth-home", default=str(Path.home()))
    for name in ("up", "down", "status", "check", "test"):
        sub.add_parser(name).add_argument("--root", required=True)
    for name in ("service", "probe"):
        sub.add_parser(name).add_argument("name")
    args = parser.parse_args()
    if args.command == "prepare":
        require(120 <= args.duration <= 1800, "duration must be 120..1800 seconds")
        return prepare(args)
    root = lab(getattr(args, "root", None) or os.environ["HERDR_SUPERVISOR_LAB"])
    config = read(root / "lab.json")
    if args.command == "service":
        return service(root, config, args.name)
    if args.command == "probe":
        return probe(root, config, args.name)
    env = environment(root, config)
    if args.command == "check":
        argv = [
            config["supervisor"],
            "control",
            "--state",
            str(root / "controller"),
            "check-local",
            "--bundle",
            str(root / "services"),
        ]
    elif args.command == "test":
        argv = [
            sys.executable,
            str(REPO / "tests/live_acceptance.py"),
            str(root / "acceptance-spec.json"),
            "--execute",
        ]
    else:
        argv = [
            config["compose"],
            "-U",
            "-u",
            str(root / "process-compose.sock"),
            "-L",
            str(root / "logs/compose.log"),
        ]
        if args.command == "up":
            argv += [
                "--disable-dotenv",
                "--ordered-shutdown",
                "-f",
                str(REPO / "process-compose.yaml"),
                "up",
                "-t=false",
            ]
        elif args.command == "down":
            require_idle(root)
            argv += ["down"]
        else:
            argv += ["process", "list", "-o", "json"]
    os.umask(0o077)
    os.chdir(REPO)
    os.execve(argv[0], argv, env)


if __name__ == "__main__":
    try:
        main()
    except (
        RuntimeError,
        OSError,
        ValueError,
        KeyError,
        subprocess.SubprocessError,
    ) as error:
        print(
            str(error) if isinstance(error, RuntimeError) else type(error).__name__,
            file=sys.stderr,
        )
        raise SystemExit(1)
