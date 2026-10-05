# Herdr Supervisor

Rust adaptation of eliasstravik/herdr-projects at 4e4548c3c43888e6be1c96906215998f07dc63f0. Preserve MIT attribution and inherited UI/resource management.

Work only in the implementation worktree. No push, deployment, global configuration, live user sessions, or additional workers. Local commits are authorized.

SQLite is authoritative for managed tasks, attempts, operations, memory revisions and outboxes. Herdr owns resources, mailer owns messages and native delivery. Never treat broker ACK or runtime acceptance as a model result. Separate owner DID, host, logical agent, attempt, UUID and epoch. Persist intent before side effects, reconcile uncertainty, reserve capacity transactionally, and accept evidence only for its exact contract and commit.

NATS is the coordination transport from the initial design; single-host development uses the same node protocol as remote execution. NetBird supplies connectivity, not application authorization. No federation or ATProto compatibility claim.

Commands: cargo test --locked; cargo clippy --all-targets -- -D warnings; nix fmt; nix build .#herdr-supervisor; nix flake check. Tests use isolated state, fake model interactions, real Git and isolated NATS as appropriate. Nix follows numtide/blueprint prefix=nix and includes tests offline.

See IMPLEMENTATION.md for deliverables and evidence status. Do not mark the goal complete while any required layer remains pending.
