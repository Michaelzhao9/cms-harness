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
  public host (every redirect hop checked), size-capped; the chat cannot install skills.
- What the admin is looking at (a page, picked elements) reaches the model as quoted data in the user turn, capped
  at 500 characters, never in the system prompt.
- Model calls run outside the database lock, so a slow or failing model never blocks the admin.

## Members (the website's visitors)

- Members are a separate table with separate sessions (`hc_member`, `HttpOnly`, `SameSite=Lax`). A member session grants
  nothing in the admin or the API's write tools.
- Passwords: 10+ characters, Argon2id. Limits: 5 failed logins per email or client in 15 minutes, 10 sign-ups per
  client per hour, 20 public submissions per client and 60 per member per hour. Disabling a member ends their sessions.
- Read rules are enforced on the server for the content API and the built-in site: another member's `owner` record is
  a 404; members-only data is empty for visitors.
- Sign-up reveals whether an email is already registered (the usual trade-off for a site's sign-up form). A
  submission that clashes with a unique value gets a generic message, never another record's id or value.
- Rich text a visitor submits loses its images, so opening it in the admin contacts no outside server.
- Every browser POST (admin, member forms, site submissions) is refused when its `Origin` is another site.

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
switch, the old address redirects page loads to the new one for 10 minutes and refuses admin POSTs, so a script that
collected a form token earlier cannot keep using it there.

There are two ways to override or recover from a bad setting:
- `HC_ADMIN_ORIGIN=<origin|off>` overrides the saved choice.
- `cms-harness admin-origin off` switches back to one address. It takes effect on a running server at once.

## Protection (后台 → 防护)

Every request passes one door before any handler sees it, so whatever is built in the environment is behind it
without doing anything. Its settings apply at once and are not drafts. Only a builder changes them, in the admin:
no tool, API key or site file can.

- **Rate limit.** A client gets 240 requests a minute (5 times that on the bearer-key API). Going over answers 429;
  going over in three separate minutes shuts the client out for 10 minutes.
- **Scanners.** A request for a path only a vulnerability scanner asks for (`/wp-login.php`, `/.env`, `/.git/…`)
  shuts the client out for an hour.
- **Crawlers that say who they are.** Search engines always pass. AI crawlers (GPTBot, ClaudeBot, Bytespider and
  others, also listed in `robots.txt`) and scripting tools (python-requests, Scrapy, curl, headless browsers) get no
  pages. The API is not affected.
- **Challenge.** A client that opens more than 40 pages in a minute, or every visitor in strict mode, first solves
  a proof of work in the browser (about a second, once every 12 hours). The pass is signed, bound to the client's
  address and browser, and checked without server state. No outside service is involved.
- **Block list.** Listed addresses get nothing, except the admin, so an admin cannot lock themselves out.
- **Record.** What was stopped (time, reason, address, path, user agent) is kept for 7 days and shown in the admin:
  one record a minute per client and reason, at most 60 a minute in all.

The admin, the preview frames and uploaded files are outside these counts: sessions, the login limit and preview
tokens guard them.

What this does not do. A flood from thousands of addresses has to be stopped before it reaches this program, by the
host or a CDN. A crawler that drives real browsers through many addresses gets through, slower and at a cost. A
crawler can claim to be a search engine: it then passes the crawler rules but not the rate limit. Behind a reverse
proxy on another machine, set `HC_TRUST_PROXY=1`, or every visitor looks like the proxy.

## Draft preview

The live site always serves published content to everyone, whatever cookies or headers a request carries.

Drafts are rendered only under `/_hc/preview/<token>/…` on the admin's address:
- The token is random, bound to one admin session, dies at logout, expires after an hour without use, and lives at
  most 8 hours whatever happens.
- The workbench frames the preview in a sandbox without `allow-same-origin`. Every preview response also carries a
  `sandbox` CSP, so draft scripts run on an opaque origin and cannot reach the admin, even when the preview is opened in
  its own tab.
