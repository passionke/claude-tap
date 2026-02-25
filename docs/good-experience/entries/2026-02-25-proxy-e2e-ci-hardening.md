# Proxy-Aware E2E and CI Hardening

**Date:** 2026-02-25  
**Tags:** e2e, proxy, oauth, ci, reliability

## Context

Real E2E coverage was blocked by environment-dependent behavior:

- OAuth flows failed with `403 Request not allowed` in forward-proxy runs.
- CI failed on Python 3.13 due to stricter TLS certificate validation in a forward proxy test.
- Test expectations assumed `/v1/messages` was always the first API call, which is not true for OAuth preflight.

## What Worked

1. **Respect system proxy env in upstream forwarding**
   - Set `trust_env=True` in the aiohttp session used by `claude-tap` upstream forwarding.
   - This allowed traffic to follow local proxy/VPN tooling (e.g., Clash) instead of bypassing it.

2. **Make forward/OAuth assertions robust**
   - Do not assume the first traced request is `/v1/messages`.
   - Search records for at least one `/v1/messages` call and validate response content there.

3. **Harden test certificates for Python 3.13**
   - Add both `SubjectKeyIdentifier` and `AuthorityKeyIdentifier` extensions in self-signed test certs.
   - This resolved `CERTIFICATE_VERIFY_FAILED: Missing Authority Key Identifier` in CI.

4. **Fix trace stats null-safety**
   - Guard against `request.body is None` when collecting model usage metrics.

## Why This Matters

- Real E2E can pass in proxy-heavy developer environments without hidden network-path mismatches.
- CI becomes stable across Python versions.
- Tests validate behavior rather than incidental request ordering.

## Operational Notes

- Real E2E remains opt-in via `--run-real-e2e`.
- Browser integration tests require Playwright installation and browser binaries.
- When debugging proxy issues, verify both legs:
  - Claude CLI -> claude-tap
  - claude-tap -> upstream (must honor proxy env when required)
