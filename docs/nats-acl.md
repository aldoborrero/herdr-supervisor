# NATS permissions for supervisor runtimes

Use a separate broker credential for the controller and for every execution node. The credential used by `control provision` is separate from those runtime credentials. All subjects below belong to the configured owner and one NATS account; this implementation uses the default JetStream API prefix, without a JetStream domain.

## Generate the exact permissions

After initializing controller state, print the permissions for the intended node routes:

```bash
herdr-supervisor control --state CONTROLLER_DIR broker-policy \
  --node worker-a --node worker-b > broker-policy.json
```

Without `--node`, the command selects the controller's active enrolled nodes. Explicit routes also allow preparing broker authorization before enrollment. `controller` is reserved; duplicate routes, wildcards and malformed names are rejected. The output contains public routing information and permissions, not passwords, bearer tokens or private keys. It does not configure a broker or issue a delegation.

Copy `controller.permissions` into the controller NATS user's `permissions` block, and `nodes["worker-a"].permissions` into the user whose credential is installed on `worker-a`. Repeat for each node. Usernames need not match node IDs: the broker credential chooses the authorization policy, while `--node` and its signed delegation establish the supervisor route. Use independently provisioned credentials for each user.

For example, this prints the exact node permissions for the current owner:

```bash
python3 - <<'PY'
import json
from pathlib import Path
policy = json.loads(Path("broker-policy.json").read_text())
print(json.dumps(policy["nodes"]["worker-a"]["permissions"], indent=2))
PY
```

With `H` standing for the SHA-256 hash of the owner's DID, the node permissions have this structure. Use the command's concrete output when configuring the server:

```json
{
  "publish": {
    "allow": [
      "hs.H.to.controller",
      "$JS.API.STREAM.INFO.HS_H",
      "$JS.API.CONSUMER.INFO.HS_H.worker-a",
      "$JS.API.CONSUMER.MSG.NEXT.HS_H.worker-a",
      "$JS.ACK.HS_H.worker-a.>"
    ]
  },
  "subscribe": {
    "allow": ["_HS_INBOX.H.node.worker-a.>"]
  }
}
```

The controller receives the corresponding permissions for its own `controller` consumer and publishes only to the explicitly listed node command subjects. Runtime users have no stream/consumer creation, deletion, purge or direct-message-read permission. Provision streams and durable consumers with the separate broker administrator credential, then run services with their scoped credentials. When adding a node, regenerate and apply the affected runtime permissions as well as provisioning its consumer.

NATS evaluates publish and subscribe permissions separately. An allow-list restricts that direction to its listed subjects. These generated lists are nonempty, including when no nodes are enrolled. See the [NATS authorization reference](https://docs.nats.io/learn/security/authorization).

## Reply inbox isolation

Clients select their reply prefix automatically from their role and owner:

| Process | Reply inbox prefix |
| --- | --- |
| Provisioning command | `_HS_INBOX.H.provisioner` |
| Controller runtime | `_HS_INBOX.H.controller` |
| Node `worker-a` | `_HS_INBOX.H.node.worker-a` |

The client adds unique suffixes under that prefix. The matching subscription grant therefore ends in `.>`. A node called `provisioner` still uses `.node.provisioner`, distinct from the provisioning command.

This separation matters because JetStream pull responses contain the actual assignment or event. Restricting the pull API alone does not protect responses delivered on a shared `_INBOX.>` namespace. The regression test observes broker permission errors for foreign node/controller/provisioner/owner inboxes and checks that no foreign payload reaches those subscriptions. It also exercises authorized deliveries, durable ACKs and reconnects with the generated policy.

Existing configurations that allowed `_INBOX.>` for runtime users need their permissions replaced with the generated role-specific policy. The updated clients use only the new prefix; they do not fall back to a shared inbox. Keep administrator credentials with the provisioning operator, rather than installing them in runtime services. The application cannot enforce isolation if the deployed broker grants broader permissions than this policy.

TLS remains required by the client unless `private_network_plaintext` is explicitly enabled for a private network. NetBird supplies connectivity; each process still needs its own broker credential and the appropriate signed owner/node identity. This command does not generate certificates, configure NetBird or activate a server. Node credential rotation/revocation and grant renewal remain separate pending lifecycle work.
