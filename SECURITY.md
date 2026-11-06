# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's [private vulnerability reporting](https://github.com/pederbe/udpstp/security/advisories/new), never in a public issue.

Include what an attacker can do, the steps or PDUs that reproduce it, and the version or commit. The maintainer aims to acknowledge a report within a week, agree a fix and a disclosure date with you, and credit you in the advisory unless you prefer otherwise.

## Supported versions

The latest published release receives security fixes. Update to that release when reporting a vulnerability, or include the version affected if you cannot update.

## Scope

[docs/security.md](docs/security.md) describes the threat model: what udpstp defends against, what is planned, and what is out of scope. Examples of what counts:

- A way to make a Server or Client send load to a third party.
- Bypassing authentication, the replay protection or the Operator-only rules.
- A crash, hang or resource exhaustion triggered by crafted PDUs.
- Leaking key material.

Weaknesses of the protocol itself, such as the lack of encryption, are out of scope here. You can raise them as public issues.
