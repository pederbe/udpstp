# Key files

Both the Client and the Server read Shared Keys from a key file, never from the command line. A file whose name ends in `.toml` is a TOML Key Table. Any other file is read as an OB-UDPST `udpst.keys` file.

## TOML Key Tables

A TOML Key Table has one `[[key]]` row per Shared Key:

```toml
[[key]]
admin_key_name = "customers-2026"
key_id = 1
key_hex = "5b0c0e8d6f7a41c2b9d3e4f5061728394a5b6c7d8e9fa0b1c2d3e4f506172839"
accept_lifetime_end = "20270101000000Z"
send_lifetime_end = "20270101000000Z"
tests_per_quota_period = 12
```

| Field | Meaning |
|---|---|
| `admin_key_name` | A name for the row, unique in the file. Errors name the row by it, never by the key |
| `key_id` | The keyId, 0 to 255, that Clients send |
| `key_hex` or `key_text` | The key, as hexadecimal digits or as UTF-8 text. Exactly one |
| `send_lifetime_start`, `send_lifetime_end`, `accept_lifetime_start`, `accept_lifetime_end` | Optional, `YYYYMMDDHHMMSSZ` in UTC |
| `kdf`, `alg_id` | Optional. `HMAC-SHA-256` if present |
| `operator`, `max_test_interval` | An Operator Key, and its longest Test Interval in seconds (60 or more). See [Operator features](guides/operator-features.md) |
| `tests_per_quota_period`, `bytes_per_quota_period` | This row's Quotas, which an Operator Key can't have. See [Hosting a Server](guides/hosting-a-server.md#limit-what-clients-can-use) |

Unknown fields, empty keys and malformed values are errors. udpstp sets no minimum key length, but use at least 32 random bytes.

A file may start with `format = 1`, the format described here, which is also what a file without it has. A valid file keeps its meaning in later versions of udpstp: they may add columns, but never change what one means. A column this version doesn't know, or a higher `format`, is refused rather than half read, so a file written for a newer udpstp fails to load instead of losing a restriction ([Stable interfaces](interfaces.md#key-files)).

## OB-UDPST key files

An OB-UDPST file has one `<keyId>,<key>` line per key, with keys of up to 64 bytes:

```text
1,a-long-random-secret-for-consumers
```

Its rows have no lifetimes, Operator Keys or Quotas, so they are always valid.

## Lifetimes and rotation

- The Server accepts a keyId only through a row whose accept lifetime includes the Server's clock.
- The Client sends with the valid row whose send lifetime started most recently. If several rows tie, the Client refuses to guess, and `--key-id` must pick one.
- Two rows may share a keyId only if neither their send nor their accept lifetimes overlap. Otherwise the Key Table is refused at startup.
- A Test Connection keeps its keys when its row expires during a Test.
- The Server reads the file once at startup. Add rows in advance, with future lifetimes, to rotate keys without a restart. Give the new key a new keyId whose lifetimes overlap the old one's, then retire the old keyId.

## Permissions

A key file must be private, or udpstp refuses it with exit status 51 before reading any key.

- **Linux and macOS**: a regular file of mode `0600` or stricter, owned by the account running udpstp or by root.
- **Windows**: a local NTFS file that only the running account, SYSTEM and Administrators can access. [udpstp on Windows](guides/windows.md#key-file-permissions) shows how to set this up.

On every platform the file must be at most 1 MiB. Keep it in a folder only its owner can write.
