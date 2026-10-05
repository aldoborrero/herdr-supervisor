#!/bin/sh
# Build this fork from its locked sources; never download upstream Projects binaries.
set -eu
cargo build --release --locked
sh scripts/link-command.sh
