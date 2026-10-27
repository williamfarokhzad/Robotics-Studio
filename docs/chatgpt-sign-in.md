# ChatGPT account and connector access

The application bar has a small, keyboard-accessible ChatGPT button. Click it to
open an anchored account panel. Escape and clicks outside dismiss it. The account
page opens in the system browser on desktop and a new browser tab on the web.

## Current activation status

The identity-only implementation is ready for an OpenAI-issued web OAuth client.
Without `OPENAI_CLIENT_ID`, the account page explicitly disables sign-in. It never
creates a pretend account, asks for a ChatGPT password, or requests AI/inference
permissions. OpenAI currently limits website sign-in registration to selected
partners: https://developers.openai.com/siwc/website

Request and register the application through:
https://developers.openai.com/siwc/request-client-id

Application: Robotics Studio by WIFA Robotics
Source: https://github.com/williamfarokhzad/Robotics-Studio
Website: https://robotics-studio.onrender.com
Exact callback: https://robotics-studio.onrender.com/auth/openai/callback
Requested scopes: openid profile email
Client type: public, or confidential with client_secret_basic as registered by OpenAI
Privacy: https://robotics-studio.onrender.com/account
(The account explanation is not a substitute for a complete published privacy policy.)

Set on the cloud service, without committing credentials:

- `OPENAI_CLIENT_ID`: issued by OpenAI, never a fabricated or borrowed Codex ID.
- `OPENAI_CLIENT_SECRET`: only for a registered confidential client.
- `STUDIO_LOGIN_ORIGIN`: https://robotics-studio.onrender.com

The frontend nginx server proxies `/account` and `/auth/` to the Rust cloud service.
This keeps browser cookies on the Studio origin. On a static host without that
proxy these routes are unavailable; use the Render website for account sign-in.
The frontend and cloud commits must both be deployed.

## Security and limits

OIDC discovery is restricted to auth.openai.com and HTTPS. Identity tokens use
RS256 verification against current JWKS, with issuer, audience, expiration,
issued-at, subject, nonce and authorized-party checks. State, PKCE and a browser
cookie bind a one-use five-minute transaction. App sessions use opaque identifiers
in Secure, HttpOnly, SameSite=Lax host-only cookies and expire after eight hours.
Logout requires the exact Studio Origin and a per-session CSRF token. Tokens are
not returned to the UI or persisted in browser storage. Responses are not cached.
No OpenAI identity tokens are kept after verification.

This remains a private, single-instance preview. Transactions and app sessions
are in memory, capped at 128 each, and disappear on restart/free-instance sleep.
Public rollout needs a shared durable account/session store, rate limiting and
operational controls. OpenAI identity does not grant access to owner CAD sessions.

The desktop button currently manages the hosted browser account. It does not
transfer a browser session into the native process, enable AI, or sign the native
process into an OpenAI workload client. Native token storage/device callback
integration is a separate capability; the UI does not claim it is connected.

## MCP

The connector keeps its existing owner password and CAD grant isolation. Its
landing page provides setup steps and a copy-address button. The consent page
uses a responsive layout and hides the long client identifier in expandable
connection details. Native/web account sign-in and connector authorization are
independent transactions. Do not substitute an OpenAI ID token for an MCP token.
