# udpstp-key-permissions

Application support for [udpstp](https://github.com/pederbe/udpstp)'s Windows
Shared Key loading. It assesses the owner and DACL of a borrowed open file
against [ADR 0015](https://github.com/pederbe/udpstp/blob/main/docs/adr/0015-windows-shared-key-private-file-policy.md).

This is an implementation helper, outside the protocol core and network runtime.
Its safe interface returns a policy match, a mismatch, or an unverified result.
Only its Windows native inspection module permits `unsafe`; the descriptor
decoder and policy forbid it. The caller reads the same handle, protects the
containing storage and decides how to report the assessment.

Local regular NTFS files and ordinary allow/deny entries are supported. This
structural policy does no group expansion or effective-access analysis, and
inspection does not freeze the ACL. Unsupported forms are unverified.

Dual-licensed under MIT or Apache-2.0.
