default:
    @just --list

test *args:
    cargo test --locked {{ args }}

lint:
    cargo clippy --locked --all-targets -- -D warnings

fmt:
    nix fmt

check:
    nix flake check --log-format bar-with-logs

build:
    nix build .#herdr-supervisor --log-format bar-with-logs

run *args:
    cargo run --locked -- {{ args }}
