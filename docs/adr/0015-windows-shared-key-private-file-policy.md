# Windows Shared Key files use a bounded private-file policy and a native inspection adapter

**Status: Accepted** (2026-10-05). Production refusal (2026-10-06), following the separately accepted transition.

[ADR 0018](0018-one-application-crate.md) supersedes the separate helper-package layout. The native adapter now lives in the application. This record's permission policy and safety invariants remain in force.

Authenticated Clients and Server administrators need accidental permission grants
caught when a Shared Key file is loaded. Unix already refuses untrusted ownership
and group/other read or write access. Windows now assesses permissions at startup.
RFC 9946 does not prescribe a Windows algorithm. This decision supplies the
supported policy for both TOML Key Tables and OB-UDPST Shared Key files.

## Policy

Trust the running process account, SYSTEM (`S-1-5-18`) and built-in Administrators
(`S-1-5-32-544`). A virtual service account is trusted when it is the running
account, including ADR 0014's `NT SERVICE\udpstp-server`. Compare binary SIDs:
do not resolve account names, expand groups, consult a domain, or trust an
arbitrary group because the running account belongs to it.

Assess the owner and discretionary access-control list (DACL) of a regular file
on a local NTFS volume. Return owned Rust data and one of three outcomes:

- **Meets the supported policy:** trusted owner and no applicable ordinary allow
  entry giving an untrusted identity dangerous rights.
- **Does not meet the supported policy:** untrusted owner, absent/null DACL, or
  an applicable ordinary allow entry granting dangerous rights to an untrusted
  identity. This describes a policy mismatch; it does not prove effective access.
- **Cannot be verified:** inspection failure, malformed or oversized descriptor,
  unsupported storage, ACE type, flags or rights. Unsupported data takes
  precedence over a mismatch so a partly understood ACL is never called verified.

Dangerous rights include reading, writing or appending file content, executing
it, writing extended attributes or file attributes, changing its DACL or owner,
and deleting it. File execution is included conservatively because executable
sections can expose content. Reading attributes, extended attributes or
permissions, and synchronization alone do not read a Shared Key. Map
`GENERIC_READ`, `GENERIC_WRITE`, `GENERIC_EXECUTE` and `GENERIC_ALL` to their file
rights before classification using the Windows file generic mapping.

Ordinary allow and deny ACEs are supported, including inherited entries and
inheritance propagation flags. An inherit-only ordinary entry does not grant
access to this file. Deny entries do not cancel outsider allow entries under
this structural policy, even when Windows would deny the concrete access.
Unknown ACE types, including callback/conditional and object-specific entries,
are unverified even if marked inherit-only. Special identities are not silently
trusted: an unfamiliar SID with a dangerous allow entry is a mismatch. An empty
DACL meets the grant policy with a trusted owner, but may prevent a new content
read. Successful assessment never overrides an ordinary open/read failure.

## Native boundary and safety

Allow udpstp's own `unsafe` only inside a Windows native Shared Key permission
adapter. Its safe interface accepts a borrowed open `File` or handle and returns
owned descriptor data or an inspection error. Process-token inspection supplies
the running account's owned SID. The application policy and descriptor decoder
remain safe Rust beside key-file loading; the protocol core, wire code and
network runtime retain `forbid(unsafe_code)`. This is a specific exception to the
previous blanket prohibition, not authorization for other native features.

The application uses `windows-sys` for Win32 bindings. Its crate root denies
`unsafe`, and only `src/windows_permissions/native.rs` allows it. The decoder,
policy and ordinary application modules explicitly forbid it. No raw pointer
or borrowed SID escapes the module, and safe callers cannot supply invalid
handles. The adapter is an internal module under ADR 0018.

Safety invariants:

- The caller owns the file; its borrow lasts through each native call. Inspection
  neither closes nor replaces the file handle. A separately opened process token
  has one Rust `OwnedHandle`, closing it on every success and error path.
- `GetKernelObjectSecurity` copies owner and DACL into a self-relative descriptor
  in initialized, pointer-aligned Rust storage. Query the required length first;
  accept at most 128 KiB, and reject any failed call or changed/out-of-range size.
  Rust frees this allocation. There is no OS descriptor allocation to free in the
  production inspection path, and no retry turns an inspection error into a pass.
- `GetTokenInformation(TokenUser)` follows the same bounded allocation pattern.
  Validate the returned structure length and its SID pointer as an offset inside
  the returned buffer before reading SID bytes. Copy the bounded SID while the
  buffer lives. Do not dereference a returned pointer outside that buffer.
- Parse self-relative offsets, ACL/ACE lengths and SID revision/subauthority
  count with checked Rust slices. Limit the descriptor to 1,024 ACEs and SIDs to
  15 subauthorities; recognize only ordinary ACE layouts and documented flags.
  No native pointer walking is needed for descriptor decoding.
- Check each Win32 return convention and capture the error before cleanup calls
  can overwrite it. File metadata, filesystem and resolved-volume checks refer
  to the borrowed handle. Remote volumes and unsupported cases are unverified.
- Test-only SDDL installation owns the converter's OS allocation and releases it
  once with `LocalFree`, including when installation fails. This writable helper
  is excluded from the production adapter.

## Loading and limits

Open a Shared Key file for reading with `FILE_SHARE_READ` only. Inspect and read
that same handle, preserving the existing file-size bound and wiping content
buffers. An already open content writer prevents this protected open; while the
reader lives, new content writers and delete/replacement opens fail sharing
checks. Another reader can coexist.

This is a startup assessment. It does not freeze the security descriptor:
`WRITE_DAC` can still change the ACL while the content reader is open. It does
not protect a writable containing directory or guarantee lasting protection.
Protected containing storage is a deployment assumption. Privileged bypass,
pre-existing writable mappings, identity impersonation, comprehensive effective
access, group expansion and unusual filesystems are outside this bounded proof.

Only a file that meets the supported policy proceeds to content reading. Policy
mismatches and unverified permissions are distinct key file errors (exit status
51), with the file, reason and supported remediation, never its content. Neither
outcome claims proven exposure. Client errors go to stderr; Server errors also
reach its configured startup log as StartupError. Read and parse errors retain
their existing handling.

Ordinary `--print-config` still loads and validates keys. The additive
`--print-config --settings-only` preview validates settings and prints only their
TOML, without loading keys, opening logs or listening; it cannot accompany
`--windows-service`. The installer uses this preview to obtain ports, then the
real trial start validates keys as the virtual service account. ADR 0014 defines
installation failure containment. The prior warning reasons are removed before
0.1.0, under ADR 0013; no elapsed warning release is required.

## Considered options

- **Runtime PowerShell:** adds process startup, can be restricted by deployment
  policy, and inspects a path separately from the file read. Rejected.
- **Full effective-access evaluation:** requires identity/group and potentially
  domain knowledge, plus substantially wider permission-form support. Rejected
  for the initial policy; a mismatch must not be reported as proven exposure.
- **Unmaintained safe ACL wrappers:** do not justify moving this boundary into
  an unmaintained dependency. Direct maintained bindings keep the adapter visible.
- **Path inspection followed by reopening:** permits object substitution between
  checking and reading. Rejected in favor of one borrowed handle.

## Sources

- [Microsoft: file security and access rights](https://learn.microsoft.com/en-us/windows/win32/fileio/file-security-and-access-rights).
- [Microsoft: DACLs and ACEs](https://learn.microsoft.com/en-us/windows/win32/secauthz/dacls-and-aces).
- [Microsoft: GetKernelObjectSecurity](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-getkernelobjectsecurity).
- [Microsoft: GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation).
- [Microsoft: GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew).
- [ADR 0014: The Server runs as a Windows service of its own, under a virtual account, logging to daily files](0014-the-server-as-a-windows-service.md).
