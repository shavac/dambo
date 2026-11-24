# Security Policy

## Reporting a Vulnerability

Please **do not** open a public GitHub Issue for security vulnerabilities, credential/cookie handling flaws, XSS bypasses in web plugins, or path-traversal bugs.

Instead, please report security issues privately using one of the following channels:
- **GitHub Private Vulnerability Reporting**: Use the **Security → Advisories → Report a vulnerability** button on this repository (`https://github.com/shavac/dambo/security/advisories/new`).

Please include:
- A clear description of the vulnerability and its potential impact.
- Step-by-step reproduction instructions or a minimal proof-of-concept.
- Affected version/commit hash and OS environment (macOS / Windows / Linux).

## Sensitive Data Reminder

- Never attach `var/dambo.redb` (or legacy `var/session.json`), `SESSDATA`, `bili_jct`, or `*_raw.jsonl` files containing personal cookies or private session tokens to public issues or pull requests.

