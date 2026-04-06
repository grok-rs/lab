# Security Policy

## Supported Versions

| Version | Supported          |
|---------|--------------------|
| latest  | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Do not** open a public GitHub issue
2. Email security concerns to the maintainers via GitHub's private vulnerability reporting:
   - Go to **Security** > **Advisories** > **New draft advisory**
3. Include steps to reproduce, impact assessment, and suggested fix if possible

We aim to respond within 48 hours and release a fix within 7 days for critical issues.

## Security Measures

- Secrets are stored with `chmod 600` outside project directories (`~/.local/share/lab/`)
- Secret values are masked in job output (including base64 variants)
- Per-job secret scoping prevents exfiltration of unused secrets
- MCP server redacts secret values in variable expansion responses
- File path validation prevents path traversal in MCP tools
- No secrets are passed as Docker environment variables (mounted as read-only files)
