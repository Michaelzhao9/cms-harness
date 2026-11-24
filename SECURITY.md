# Security notes

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
