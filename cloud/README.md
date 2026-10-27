# SolidCraft cloud MCP: private owner preview

A Rust HTTP gateway for the existing Rust CAD engine. The web CAD viewer and this service
are separate: tool calls do not update an already-open browser design.

## Connect

Deploy `cloud/Dockerfile` from the repository root as a Docker web service. Set:

* `STUDIO_PUBLIC_URL`: the service HTTPS origin, without a trailing path.
* `STUDIO_ADMIN_PASSWORD`: a unique owner password of at least 20 characters. Enter it in
  Render Environment; never put it in source control or chat. Login stays disabled until set.
* `STUDIO_OAUTH_SIGNING_KEY`: a persistent random secret of at least 32 characters. Keep it unchanged across deployments so registered ChatGPT client IDs stay valid. Rotating it requires recreating the ChatGPT connection.
* `PORT`: Render supplies this, default 10000.

In ChatGPT Plugins, add a custom MCP server with URL `<STUDIO_PUBLIC_URL>/mcp`, use OAuth,
and dynamic client registration (leave client ID/secret empty when supported). Sign in on
this service's authorization page with the owner password and approve CAD access.
The only allowed OAuth callbacks are ChatGPT's documented HTTPS callback routes.
A different MCP client requires an explicit callback allowlist change before use.

Test prompt: “Create a 20 x 30 x 10 mm box, verify its volume, export part.step and part.stl,
and call download_file for both files.” Expected volume is 6000 cubic millimetres.

## Limits and retention

This is a **single-owner private preview**, not a public multi-user product or a security
boundary for running untrusted native code. OAuth uses authorization codes, S256 PKCE,
resource binding, one-use codes, rotating refresh tokens, owner consent and a login rate
limit. Client registrations are signed with the persistent signing key and remain valid after restart. Access/refresh tokens are in memory; a restart can require signing in again.

Each MCP session owns a distinct CLI child process, temporary directory, HOME, XDG data
and temp directory. Children receive no server environment secrets. The existing engine's
file scope confines CAD file operations. There is no desktop or remote-control port.
Workers run as an unprivileged container user. This is process separation and scoped file
access, not a separate OS/container sandbox per tenant.

* Two active CAD sessions maximum, one CAD operation at a time. Busy requests get 429.
* 90-second CAD operation timeout; a timed-out worker is killed and its session discarded.
* 1 MiB request limit, 16 MiB worker reply limit, 64 MiB per downloaded file.
* Sessions expire after 30 minutes idle, checked every 30 seconds.
* Temporary download links expire after 30 minutes or when their session ends. A link grants
  access to that file to anyone holding it; treat links as private.
* Saved/exported designs and tokens do **not** survive a Render restart,
  redeploy, free-service sleep or session expiration. Download your work promptly.
* The browser viewer's local designs are not automatically synchronized with these sessions.
* Free hosting can sleep; cold starts may affect ChatGPT connection attempts.

Before public launch: managed identity and per-user accounts, durable encrypted design storage,
per-user quotas and billing, container-level tenant isolation, queued workers and load tests.
Do not infer capacity for thousands of users from this preview.

## Verify locally

Requires Rust 1.95+ and a built native `solidcraft-cli`:

```sh
cargo fmt --manifest-path cloud/Cargo.toml --check
cargo clippy --manifest-path cloud/Cargo.toml --all-targets --locked -- -D warnings
STUDIO_TEST_CLI="$PWD/target/ci/solidcraft-cli" cargo test --manifest-path cloud/Cargo.toml --locked
```

Without `STUDIO_TEST_CLI`, the real CAD integration test is omitted with a diagnostic.
Other tests cover OAuth discovery, resource/callback validation, replay rejection, refresh,
origin checks and filesystem names. The real CAD test covers the full OAuth flow, the MCP
lifecycle, 6000 mm³ box, STEP/STL downloads, denied file access, independent designs,
cross-grant access rejection and session deletion.

`GET /health` checks HTTP readiness. `POST /mcp` requires bearer authentication and both
JSON and SSE in Accept. Notifications return 202; requests return JSON. `GET /mcp` returns
405 (no server-initiated SSE); `DELETE /mcp` ends a session. No request or secret logging.

## ChatGPT account preview

The account service and redesigned connector pages are documented in
[ChatGPT sign-in](../docs/chatgpt-sign-in.md). Identity-only sign-in is disabled
until OpenAI issues an approved `OPENAI_CLIENT_ID`. No ChatGPT password or API key
is collected by Studio. The existing owner-only MCP authorization remains separate.
Deploy the frontend reverse proxy and gateway together for same-origin sessions.

The `account_test_key.pem` and `account_test_jwks.json` files are public test-only
fixtures for signature-validation tests, not service credentials or signing keys.
They are only included under `cfg(test)` and are never used by the running service.

## Owner sign-in troubleshooting

The owner password is the exact `STUDIO_ADMIN_PASSWORD` value on the gateway service
in Render. It is separate from Google, ChatGPT, and the OpenAI client secret.
A wrong password now keeps the same unexpired form available for another attempt;
password input is never echoed back. The form expires after five minutes and is
consumed only after successful authorization. Restarting the gateway also expires
pending forms. Start Connect again from the existing ChatGPT plugin when that happens.
Ten attempts in a minute trigger a one-minute cooldown. Error pages distinguish
password mismatch, expired forms, rejected origins, and the cooldown.

A secure ChatGPT credential prompt returning declined is not evidence that the
server rejected a password. Check the actual site result before diagnosing it.
Never paste passwords into chat or disable OAuth to work around a sign-in failure.

Browser consent responses use `Referrer-Policy: strict-origin`: this keeps OAuth
query parameters out of referrers while allowing browsers to send the actual
Origin on the form POST. `no-referrer` can turn that Origin into `null` and cause
an otherwise correct password submission to fail before password verification.
Null and foreign origins remain rejected. Reopen Connect after an upgrade so
an older page does not keep its cached no-referrer policy.
