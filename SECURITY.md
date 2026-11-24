# Security notes

CMS Harness is one program that serves a website, its admin, a chat assistant and an API. This page says what is
protected and how, in one place.

## Reporting a vulnerability

Please report privately: use the repository's private vulnerability reporting ("Report a vulnerability" on the
Security tab) rather than a public issue. Include the version, what you did and what happened. We aim to answer
within a week.

## Admin accounts and sessions

- Passwords are hashed with Argon2id. Login is limited to 5 failures per email and per client address in 15 minutes;
  unknown emails and wrong passwords get the same answer in the same time.
- The session cookie holds a random token; only its SHA-256 is stored. It is `HttpOnly`, `SameSite=Strict`, `Secure` on
  https, and lasts 30 days. Logout deletes it and ends the chat conversation.
- Every state-changing admin request is a POST with the session's CSRF token, and POSTs whose `Origin` is not the
  admin's address are refused. GET never changes anything.
- Builders manage structure, keys, admins, the model and skills; editors write and publish content.
- Admin pages send `Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; …` with no inline
  scripts or styles, plus `X-Frame-Options`, `Referrer-Policy` and `X-Content-Type-Options`; HSTS on https.

## The chat assistant

- It runs every change through the same tool layer as API keys, with the logged-in admin's permissions, minus
  `publish` and `set_member_status`: everything it does is a draft until a person publishes, including the site's
  import map and script origins, and it cannot act on members. Asking it to publish, or text in a record that
  tells it to, gets a refusal from the tool layer, not from the model's good will.
- Records, pages and uploads are data to it, never instructions; the system prompt says so, and the tool layer enforces
  the parts that matter (no publish, no keys, no admins, no passwords).
- It never sees passwords, API keys or session tokens. The model key is stored server side and never rendered back.
- Skills are text the chat may read. Nothing in a skill is executed. Only a builder can install one, from https on a
  public host, size-capped; the chat cannot install skills.
- Model calls run outside the database lock, so a slow or failing model never blocks the admin.

## Members (the website's visitors)

- Members are a separate table with separate sessions (`hc_member`, `HttpOnly`, `SameSite=Lax`). A member session grants
  nothing in the admin or the API's write tools.
- Passwords: 10+ characters, Argon2id. Limits: 5 failed logins per email or client in 15 minutes, 10 sign-ups per
  client per hour, 20 public submissions per client and 60 per member per hour. Disabling a member ends their sessions.
- Read rules are enforced on the server for the content API and the built-in site: another member's `owner` record is
  a 404; members-only data is empty for visitors.
- Sign-up reveals whether an email is already registered (the usual trade-off for a site's sign-up form).

## Uploads and site files

- Uploads are checked by content (JPEG, PNG, GIF, WebP, PDF), stored under a content hash and served publicly. Do not
  upload files that must stay private.
- Site files are validated on write (HTML templates parse, scripts transpile) and rendered with an instruction budget so
  a runaway template cannot hang the server. Rich text is cleaned of scripts and event attributes before it goes live.

## Site and admin addresses (设置 → 网址)

CMS Harness serves the website and the admin from one program. The owner picks where the admin lives:

- **同一个网址 (default):** site and admin share one origin, for example `https://example.com` and `https://example.com/admin`.
  The site may run JavaScript (written by you, by the chat, or copied from elsewhere). On this setup, that script runs on the
  admin's origin. In a browser where you are logged in to the admin, it can read admin pages and their form tokens, and so
  act as you: edit content, publish, create API keys. Put only scripts you trust on the site. 设置 shows this warning
  while the mode is on.
- **分开网址 (recommended when the site runs scripts you did not write yourself):** the admin gets its own origin, for example
  `https://admin.example.com`. Requests are routed by `Host`:
  - Admin routes answer only on the admin host: `/admin…`, `/api/v1/tools`, `/api/v1/site`, `/api/v1/media`, `/mcp` and the
    preview.
  - Site pages answer only on the site host. On the admin host they redirect to the site.
  - `/uploads/` and the public content API answer on both hosts.
  - Admin cookies are host-only on the admin host, so site scripts never see or send them.

Switching to 分开 in 设置 first checks that `<admin origin>/_hc/ping` reaches this server, using a one-time nonce. After the
switch, the old address keeps answering admin requests for 10 minutes and redirects page loads to the new one.

There are two ways to override or recover from a bad setting:
- `HC_ADMIN_ORIGIN=<origin|off>` overrides the saved choice.
- `cms-harness admin-origin off` switches back to one address. It takes effect on a running server at once.

## Draft preview

The live site always serves published content to everyone, whatever cookies or headers a request carries.

Drafts are rendered only under `/_hc/preview/<token>/…` on the admin's address:
- The token is random, bound to one admin session, dies at logout, and expires after an hour without use.
- The workbench frames the preview in a sandbox without `allow-same-origin`. Every preview response also carries a
  `sandbox` CSP, so draft scripts run on an opaque origin and cannot reach the admin, even when the preview is opened in
  its own tab.
