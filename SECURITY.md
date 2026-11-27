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

## Site and admin addresses (设置 → 后台的网址)

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

## Protection

Every request passes one door before any handler sees it, so whatever is built in the environment is behind it
without doing anything. It is always on. It has no page and no setting in the admin, and no tool, API key, site
file or admin account can change it: its level is set in the code (`Config::default` in `src/shield.rs`), and
changing it means building and deploying again. Nothing saved in the database is read for it.

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
- **Block list.** Addresses listed in the code get nothing, except the admin, so an admin cannot be locked out.
- **Record.** What was stopped (time, reason, address, path, user agent) is kept for 7 days in the database
  (`shield_events`): one record a minute per client and reason, at most 60 a minute in all. The admin does not
  show it.

The admin, the preview frames and uploaded files are outside these counts: sessions, the login limit and preview
tokens guard them.

What this does not do. A flood from thousands of addresses has to be stopped before it reaches this program, by the
host or a CDN. A crawler that drives real browsers through many addresses gets through, slower and at a cost. A
crawler can claim to be a search engine: it then passes the crawler rules but not the rate limit. Behind a reverse
proxy on another machine, set `HC_TRUST_PROXY=1`, or every visitor looks like the proxy.

## Server functions

A site file under `_fn/` is code the server runs. What keeps it in its box:

- **It gets in the same way as everything else.** A function is a draft until a person publishes it, shows in the
  publish review, and can be restored. The chat cannot publish.
- **The engine.** QuickJS, on a thread of its own, with no file, network, process or timer API. The only thing a
  script can do to the outside is ask the server, by message, to read or write records.
- **Limits.** 500 ms of wall time (an interrupt stops loops and runaway regular expressions), 32 MB of memory,
  a 512 KB stack, 200 data calls, a 64 KB request body, a 1 MB answer.
- **One run is one transaction.** An exception, a cut-off, or an answer of 400 and up rolls back everything the run
  wrote. A trial run (`run_function`, or a page in the admin's preview) always rolls back.
- **Its writes are ordinary writes.** Field checks, unique values, relations and cleaned rich text apply, and each
  write is in history under `fn:<file>`.
- **Its source is never served**, on the live site or in the preview, and a static export leaves it out.
- **A visitor never sees its errors.** They get a generic message; the script's own words go to the log.
- Requests to a function pass the protection above and the origin check like any other write. A function inside a
  members-only surface is behind that surface's door.

What a function is trusted with: all of the environment's data. It is not bound by a collection's access rules,
because it is where access logic too involved for a rule is written. Read a function before publishing it as you
would read any code that can change your data. While a function runs it holds the database, so other requests
wait for it: at most the 500 ms above.

## Notifications (webhooks)

A builder can have the server send a message to a Feishu or WeCom group robot, or to an address of their own, when
something happens. What keeps that from being turned against the server or the group:

- **Only a builder adds a destination**, in the admin's settings. There is no tool for it: the chat and API keys
  cannot point messages anywhere. A function's `notify()` only reaches the destinations a builder set.
- **Never into the private network.** Before each send the name is resolved, every address it has must be public,
  and the connection goes to the address that was checked. Redirects are not followed. `HC_FETCH_ALLOW_PRIVATE=1`
  lifts this for a deployment that must reach its own network.
- **The address is a secret** (whoever has it can write into the group). Once saved it is never shown again, only
  its host, and an error never repeats it. Editors do not see destinations at all. It is in the database, and so in
  a backup, like the chat model's key.
- **A visitor cannot mention the whole group.** `<` is taken out of every message's text, so a form field cannot
  carry a robot's mention markup. An address of your own gets the untouched values in `data`.
- **Signed.** With a secret, Feishu's own signature check is used; an address of your own gets
  `HMAC-SHA256(secret, "<timestamp>.<body>")` and the timestamp, so an old request does not pass for a new one.
- **Bounded.** One message a round (3 seconds) for each destination, at most 100 waiting for one; more are not
  written down. A flood of form posts is a trickle in the group.
- **Tied to what it tells of.** A message is written in the same transaction as the event: a submission that was
  refused, a publish that failed, or a function run that was rolled back tells nobody.

## Uploaded pictures

A JPEG, PNG or WebP that carries EXIF (a phone's photo: where and when it was taken, the device) is stored without
it, upright, so a published photo does not give away a location. The copy a chat attachment sends to the model
provider is made the same way, at 1568 pixels at most. A PNG that is kept byte for byte (small, or see-through) keeps
its text chunks. Files uploaded before this was added are as they were.

## Draft preview

The live site always serves published content to everyone, whatever cookies or headers a request carries.

Drafts are rendered only under `/_hc/preview/<token>/…` on the admin's address:
- The token is random, bound to one admin session, dies at logout, expires after an hour without use, and lives at
  most 8 hours whatever happens.
- The workbench frames the preview in a sandbox without `allow-same-origin`. Every preview response also carries a
  `sandbox` CSP, so draft scripts run on an opaque origin and cannot reach the admin, even when the preview is opened in
  its own tab.
