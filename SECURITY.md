# Security policy

VibeLens is a database client: it holds connection credentials and runs statements against your
databases, so security reports are taken seriously and handled first.

## Reporting a vulnerability

Please report it privately. Do not open a public issue, pull request or discussion for it.

Use GitHub's private reporting form:
**[Report a vulnerability](https://github.com/heinM/VibeLens/security/advisories/new)**
(repository → Security → Advisories → Report a vulnerability). Only the maintainers can see the
report.

It helps to include:

- the VibeLens version (About dialog, or the installer file name) and your operating system;
- the database engine and version involved, if any;
- what an attacker needs (for example: a hostile server, a crafted file, local access) and what
  they gain;
- steps to reproduce, or a proof of concept.

Never include real credentials, connection strings or production data. Redact them, or reproduce
against a throwaway database.

## What to expect

- An acknowledgement within 7 days.
- An assessment and, if the report is accepted, a planned fix within 30 days. Fixes are released
  as a new version and credited to the reporter in the release notes unless you ask otherwise.
- Please give us until the fix is released before disclosing publicly; we aim for no more than
  90 days from the report.

## Supported versions

Only the latest release receives security fixes. VibeLens is in prerelease (0.x); older
prereleases are not patched.

## Scope

In scope: the desktop app and its Rust core in this repository, including the vendored code under
`VibeLens/vendor/`, the build and release workflows, and the published installers.

Out of scope: vulnerabilities in the database servers or cloud services VibeLens connects to, and
issues that need an attacker who already controls your user account or machine.

The design rules the app is held to are described in the README ("Security model") and in
[docs/DESIGN.md](docs/DESIGN.md).
