# Test fixtures

Golden fixtures that every future binary must still read, or refuse with the documented error (ARCHITECTURE §11.7,
TEST-106):

| Directory | Contents | Owned by |
|---|---|---|
| `wire/` | golden wire encodings of a canonical tuple set, per released wire version | M4.4 |
| `storage/` | checkpoints, WAL segments and snapshots produced by every released storage version | M12.3 |
| `pki/` | inputs of the throwaway test TLS PKI (TEST-107); certificates are generated at test time with `rcgen` | M8.7 |

Fixtures are normative once committed: changing one requires a version bump of the format it pins and a line in
`docs/DECISIONS.md`. Never regenerate a fixture to make a test pass.
