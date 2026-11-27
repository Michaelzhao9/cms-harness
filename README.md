<p align="center">
  <img src="docs/images/banner.svg" alt="CMS Harness: say what you want, the AI builds it as a draft, you publish." width="100%">
</p>

<h3 align="center">An AI builds your whole website. It can't publish a thing.</h3>

<p align="center">
  The site, the admin, the AI chat and the API in <b>one 8 MB download</b> and <b>one SQLite file</b>.<br>
  No Node, no Postgres, no Redis, no build step.
</p>

<p align="center">
  <a href="https://github.com/Michaelzhao9/cms-harness/releases"><img alt="Latest release" src="https://img.shields.io/github/v/release/Michaelzhao9/cms-harness?color=f5b700&labelColor=1d1d1f"></a>
  <a href="https://github.com/Michaelzhao9/cms-harness/actions/workflows/ci.yml"><img alt="Tests" src="https://img.shields.io/github/actions/workflow/status/Michaelzhao9/cms-harness/ci.yml?branch=main&label=tests&labelColor=1d1d1f"></a>
  <a href="https://github.com/Michaelzhao9/cms-harness/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/Michaelzhao9/cms-harness?style=flat&color=f5b700&labelColor=1d1d1f"></a>
  <a href="LICENSE"><img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-1d1d1f"></a>
  <img alt="Rust 1.85+" src="https://img.shields.io/badge/rust-1.85%2B-1d1d1f">
  <img alt="MCP server built in" src="https://img.shields.io/badge/MCP-built%20in-f5b700?labelColor=1d1d1f">
</p>

<p align="center">
  <a href="#-quick-start">Quick start</a> ·
  <a href="#-what-you-get">Features</a> ·
  <a href="#-connect-your-agent">Connect your agent</a> ·
  <a href="#-reference">Reference</a> ·
  <a href="README.zh-CN.md">中文</a>
</p>

<p align="center">
  <img src="docs/images/demo.gif" alt="Typing a request in the admin chat, the AI building a studio home page as a draft with the changes marked in the preview, then publishing it and viewing the live site." width="100%">
</p>

Open the admin and say *"turn the home page into a studio page"*, *"add a booking form"* or *"let visitors sign up"*. The built-in assistant writes the content, the pages and the admin screens to manage them. Everything it does is a **draft**: the live site does not move until a person reviews the changes and presses **Publish**.

Not happy with a detail? Don't write a prompt for it. **Drag it, resize it or retype it right on the page.** And your own agent (Claude Code, Codex, any MCP client) can do the same work through the built-in MCP server.

## Why it's different

| 🛡️ The AI can't publish | 📦 Nothing to set up | 🌐 A real website, not a template |
| --- | --- | --- |
| Content, pages, settings and admin screens are all drafts. You see every change, old against new, and uncheck what you don't want. Every release is kept and can be put back. | One executable serves the site, the admin, the chat, member accounts and the API. Data is one SQLite file. Back the whole site up as one zip. | Pages are HTML, CSS and JavaScript you own, with templates, JSX and TypeScript, React or Vue from an import map, and no bundler. Or export everything as static files. |

## 🚀 Quick start

```sh
curl -O https://raw.githubusercontent.com/Michaelzhao9/cms-harness/main/docker-compose.yml
docker compose up -d
docker compose exec cms-harness cms-harness admin you@example.com   # prints the admin password once
```

Sign in at `http://localhost:4200/admin`. The site is at `http://localhost:4200/`.

Then open **Settings → Chat model**, pick a service, paste a key and press **Detect**. That's it: start talking.

<details>
<summary><b>No Docker? Download a binary or build from source</b></summary>

<br>

Download a binary for Linux (x86_64, arm64), macOS (Apple Silicon) or Windows from [Releases](https://github.com/Michaelzhao9/cms-harness/releases), or build it (Rust 1.85 or newer):

```sh
git clone https://github.com/Michaelzhao9/cms-harness.git
cd cms-harness
cargo build --release
./target/release/cms-harness admin you@example.com    # creates an admin and prints its password once
./target/release/cms-harness                          # serves on 127.0.0.1:4200
```

Sign in at `http://127.0.0.1:4200/admin`.
</details>

<details>
<summary><b>Putting it on a real domain</b></summary>

<br>

The server speaks plain HTTP, so put a reverse proxy (Caddy, nginx) in front for HTTPS, and set `HC_ORIGIN` to the public address exactly as the browser shows it (`https://example.com`). In `docker-compose.yml` that is the `HC_ORIGIN` line. The binary on its own only listens on the local address.

The chat model can come from the environment instead of Settings: `LLM_PROVIDER` (`anthropic` or `openai`), `LLM_API_KEY`, `LLM_MODEL` and `LLM_BASE_URL`. Without a model, the admin and the API still work, but there is no chat.
</details>

## ✨ What you get

### Build with AI, safely

- 🤖 **AI that builds, people who publish.** The assistant cannot publish. It treats text in records, pages and uploads as data, never as instructions.
- 📝 **Drafts everywhere.** Content, collections, pages, admin screens and settings are drafts until published. The live site always shows the published version, whoever visits. Compare draft and live side by side.
- 🗂️ **Admin screens that follow the site.** When the site gains a form, members or orders, the assistant builds the screen to manage them in the same turn. The admin nav groups collections by the site's own sections. A collection no page uses is marked *Not on the site yet*, with one button that asks the chat to put it there.
- 🧠 **Any model.** Claude, OpenAI, Gemini, DeepSeek, Qwen, Kimi, GLM, Doubao, SiliconFlow, Mistral, xAI, Groq, OpenRouter, a local Ollama or LM Studio, or any OpenAI-compatible API. Paste a key and it lists the models you can use.
- 🔌 **Your agent, too.** A built-in MCP server and a REST API with 41 tools and scoped keys (`read`, `write`, `structure`, `publish`).
- 🌍 **Web search for the chat (off by default).** `web_search` and `fetch_page` through a self-hosted SearXNG, Brave or Tavily, with any model. Pages come back as material, never instructions, and private or local addresses are refused at every redirect.
- 📚 **Skills.** Install a skill folder (with a `SKILL.md`) from GitHub or a zip. The chat reads it when the task needs it.

### Edit by hand, right on the page &nbsp;<sup>new in 0.2</sup>

- ✏️ **Click anything in the preview to change it.** A small bar sets size, weight, colours, alignment, corners, shadow and padding. Double-click to retype text.
- 🖱️ **Drag, resize, copy, delete.** Elements snap to the edges and centres of their neighbours. Arrow keys nudge, Cmd/Ctrl+Z undoes.
- 🖊️ **A pen that adds things where you click.** Text, rectangles, circles, lines, buttons and uploaded images.
- 📱 **Per-width edits.** Change something for desktop, tablet or phone only, and reset a width back to what desktop shows.
- ⚠️ **It warns, it doesn't guess.** Hand-edited elements that end up off screen, on top of other content or with text under 12 px are counted in the toolbar, with a jump to each.
- 🤝 **The AI knows what you did.** It sees your hand edits and carries them through its rewrites. A rewrite that would lose one is refused until you agree.

Hand edits are drafts too, like everything else.

### A real website

- 🔗 **A page for every record, automatically.** Write one detail template and every record of a collection gets its own page at a clean address (`/news/drum-contest`; Chinese titles stay Chinese). Change a slug or a category and the old address redirects forever. Links in text point at the record, not the address, so they never break, and a publish that would leave a dead link is stopped.
- 🌏 **Any number of languages.** One record, many languages: text is translated; images, dates and prices are shared. Each language gets its own addresses (`/en/…` or its own domain), fallbacks, hreflang, and localized dates and money. Out-of-date translations are flagged, and the chat can keep every language in sync as drafts.
- ⚡ **Dynamic, at static speed.** Live pages are cached, and a publish recomputes exactly the pages it touched. `cms-harness export` writes the whole site as static files for any CDN.
- 🧩 **Your stack, no build step.** HTML with `{{ }}` templates, plus CSS, JavaScript, JSX and TypeScript transpiled on the fly. React, Vue or Three.js load through an import map. You can also upload a site built with Vite.
- 🖼️ **Images in the right size.** `{{ item.cover|thumb(640) }}` serves a smaller copy, made once and kept.
- 👥 **Members and access rules.** Sign-up and login for visitors, kept apart from admin accounts. Each collection says who can read it (public, members, each member their own, private) and who can submit to it.
- 🧩 **One environment, many surfaces.** The site is the first surface. When students and teachers, or customers and staff, need different screens, the assistant builds a surface for each (`/teacher/`, `/student/`), with its own pages and its own rule for who may enter, over data the whole environment shares. A members-only surface is guarded by the system, so its pages never check for themselves. The admin lists every surface, and **Environment** shows them all with the data they share.
- 🛡️ **Protection built in.** Every page and endpoint built in the environment is behind one door: rate limits, vulnerability scanners shut out, no pages for AI crawlers and scripting tools (search engines still pass), and an in-browser check for clients that read page after page, with no outside service. **Protection** in the admin shows what was stopped; under attack, switch to strict and it applies at once. Only a builder changes these settings: neither the chat nor an API key can.
- 🗂️ **Manage any data directly.** Every collection's list can be searched (text anywhere in a record, drafts too), exported as CSV, and filled from a CSV whose rows arrive as drafts. Every row is checked before anything comes in: one that does not fit and nothing does. In an export, text a spreadsheet would run as a formula is defused.
- 🔎 **Full-text search.** `search("query")` in a page, `GET /api/v1/search` and a `search` tool. Chinese works without a dictionary, and visitors only ever find what they may open.

### Run it your way

- ⏰ **Scheduled publishing.** Pick a time on the review page. It goes through the same checks and history as a normal publish.
- 💾 **Backup and restore.** The whole site (database and uploads) as one zip, from **Settings → Backup** or `cms-harness backup`. A restore checks the archive and saves the current site first.
- 🗺️ **SEO basics, done.** `sitemap.xml` and `robots.txt` follow every publish, in every language.
- 🈶 **Chinese and English.** The admin switches in Settings, and the chat replies in the admin's language.

<table>
  <tr>
    <td width="50%"><img src="docs/images/workbench-en.webp" alt="The admin: the chat on the left has turned the home page into a studio page, the preview on the right marks what changed, and the Publish button shows 2 pending changes."></td>
    <td width="50%"><img src="docs/images/publish-en.webp" alt="Publish review: each pending change with its old and new value."></td>
  </tr>
  <tr>
    <td align="center"><sub>Chat on the left, the draft on the right, changes marked.</sub></td>
    <td align="center"><sub>Review before publishing: every change, old against new.</sub></td>
  </tr>
  <tr>
    <td width="50%"><img src="docs/images/table-en.webp" alt="A collection as a table, with filters for changed and draft records."></td>
    <td width="50%"><img src="docs/images/welcome.webp" alt="A fresh site: one welcome page with its name and a link to the admin."></td>
  </tr>
  <tr>
    <td align="center"><sub>Content tables the assistant builds and keeps in step with the site.</sub></td>
    <td align="center"><sub>A fresh deploy: one page, until you ask for more.</sub></td>
  </tr>
</table>

## 🔌 Connect your agent

Create a key with `cms-harness key <name> [write] [structure] [publish]`, then add the server to any MCP client that speaks HTTP:

```json
{
  "mcpServers": {
    "cms-harness": {
      "type": "http",
      "url": "http://127.0.0.1:4200/mcp",
      "headers": { "Authorization": "Bearer hc_..." }
    }
  }
}
```

Leave `publish` off the key and your agent can build all day while the live site stays exactly as it is.

Or use REST: `GET /api/v1/tools` lists the tools this key can call with their parameters. `POST /api/v1/tools/<name>` calls one.

## 🔧 Under the hood

- About 16,000 lines of Rust: [axum](https://github.com/tokio-rs/axum), SQLite (bundled, with full-text search), [minijinja](https://github.com/mitsuhiko/minijinja) for templates and [oxc](https://oxc.rs) to transpile JSX and TypeScript.
- 360 tests, run with clippy on every push to main.
- The admin is server-rendered HTML with plain JavaScript and a strict Content-Security-Policy. There is no front-end build.
- Passwords are hashed with Argon2id. The details are in [SECURITY.md](SECURITY.md).

## 📖 Reference

<details>
<summary><b>Command line</b></summary>

| Command | What it does |
| --- | --- |
| `cms-harness` | start the server |
| `cms-harness admin <email> [builder\|editor]` | create an admin, or reset their password |
| `cms-harness key <name> [write] [structure] [publish]` | create an API key (shown once) |
| `cms-harness admin-origin <address\|off>` | give the admin its own address; `off` puts it back with the site |
| `cms-harness backup [file.zip]` | the whole site (database and uploads) as one zip |
| `cms-harness restore <file.zip>` | replace the site with a backup (stop the server first) |
| `cms-harness export <dir>` | write the live site as static files (set `HC_ORIGIN` to where they will be served) |

Builders manage structure, keys, admins, the model and skills. Editors write and publish content.
</details>

<details>
<summary><b>Website pages</b></summary>

A fresh site has one built-in page: a welcome with the site's name and a link to the admin. Every other page comes from **site files** that the assistant (or your agent) writes:

- HTML with templates: `collection("news", limit=10, sort="-date")`, `count("news", filter={...})` for page numbers (`collection()` returns at most 100 at a time), `record("news", 3)`, `search(request.query.q, limit=20)` (each hit has `_collection` and `_snippet`), the signed-in `member`, `site.name`.
- CSS, JavaScript, and JSX / TypeScript / TSX, which are transpiled when served.
- Libraries from `https://esm.sh` through an import map (`set_site_settings`), so no bundler is needed. A site built elsewhere can be uploaded in one piece (`upload_site_bundle`).

The live site always shows the published version, whatever cookies or headers a visitor sends. Drafts only appear in the admin's preview.

**Record pages.** A collection visitors may read gets a page per record once the site has a detail template: `<collection>/_item.html`, or `_item.html` for every collection. The template reads the record as `item`. Addresses come from the collection's `url` pattern, `/{collection}/{slug}` by default; `{id}`, a field (`{year}`) or a relation's slug (`/products/{category.slug}/{slug}`) work too. Each record gets its slug from its title when it is created and keeps it when the title changes. A new slug is a draft; once live, the old address keeps redirecting. Link to records with `item.url` (or `/_r/<id>`): links pasted into rich text are stored that way automatically and always render at the record's current address.

**Languages.** Set the site's languages in **Settings → Site languages** (or `set_site_settings` with `languages`); the first is the default. Other languages live under `/<code>/` or on their own domain, and may fall back to others. Text and rich text fields are translated (`translate: false` keeps one shared); a collection with `untranslated: "hide"` leaves untranslated records out of other languages. Translations are written with `set_translation` or in the record editor, kept in the record as drafts, and `translation_status` lists what is missing or out of date. Templates get `lang`, `dir`, `languages` (this page in every language, for a switcher) and `suggested_lang`; `{{ "…"|t }}` reads the site's string table; `|date`, `|number` and `|money("EUR")` format per language. A link a template writes as `/about` leads to `/en/about` on an English page. hreflang links and a multilingual sitemap are added for you.

**Images.** `/uploads/<name>?w=640` serves a copy no wider than one of five widths (384, 640, 828, 1200, 1920), made once and kept. Templates ask for it with `|thumb(640)`. PNG stays PNG, JPEG and WebP become JPEG, and nothing is enlarged.

**Cache and export.** Live pages for visitors who are not signed in are cached until something they read is published again (the `x-cms-cache` header says hit or miss). `cms-harness export <dir>` writes every public page, record pages included, plus site files, uploads, `sitemap.xml`, `robots.txt` and `404.html`; set `HC_ORIGIN` to the address the files will be served from. Forms and sign-in still need the server; the command lists the pages that use them.

`/sitemap.xml` lists the published pages (not layouts or fragments that other files extend or include, nor names starting with `_`), each with the date it last went live, and `/robots.txt` points to it. Both follow every publish. A site file with the same name replaces them.
</details>

<details>
<summary><b>Editing by hand</b></summary>

In the draft preview, select an element with a click on something that does nothing, a long press or a right-click. The bar above it sets size, weight, slant, underline, colours, alignment, a style preset, corners, shadow and padding, for all widths or only the current one. It also duplicates, deletes, moves the element a layer up or down, and hides it at this width. A double-click edits text written in the template.

- **Move and resize.** Drag an element to move it and pull its handles to resize it. Both snap to nearby elements; hold Alt to turn snapping off. Arrow keys nudge by 1 px, Shift+arrow by 10 px. Cmd/Ctrl+Z and Shift+Cmd/Ctrl+Z undo and redo. Delete removes the element and Cmd/Ctrl+D duplicates it.
- **The pen** (bottom right of the preview) adds text, a rectangle, a circle, a line, a button or an uploaded image where you next click. A selected image can be replaced.
- **Shared elements.** An element in a template other pages use is marked *Shared · N pages*. Its edits can be limited to the current page.
- **Widths.** Edits are saved for the width being looked at. The desktop preview is never narrower than 1024 px; a narrower pane shows it scaled down.
- **With the AI.** `get_site_file` returns `hand_edits`, and file rewrites carry them along. A rewrite that would lose one is refused unless it passes `replace_hand_edits: true`.

Properties and values come from fixed lists, typed text is escaped so it can never become template code, and only uploaded files go into templates.
</details>

<details>
<summary><b>Members and access rules</b></summary>

`set_member_login` turns visitor accounts on. The site then has `/account/signup`, `/account/login` and `/account/logout`, and the `/api/v1/members/*` endpoints. **Members** appears in the admin. Members cannot sign in to the admin.

Each collection sets:
- who can read it (`read`): `public`, `members`, `owner` (each member sees their own), or `private` (default: admin and keys only);
- who can submit to it from the site (`create`): `none` (default), `members` or `public`. Submissions go live at once, record who sent them, and show as "N new" in the admin. Forms post to `POST /<collection>/new`; the API takes `POST /api/v1/content/<collection>`.
- who can change (`update`) and remove (`delete`) a record from the site: `none` (default), `owner` (the member who added it), `members`, or a condition such as `record.owner == member.id and data.status == record.status`. A condition can use `record.<field>`, `record.id`, `record.owner`, `member` (logged in), `member.id`, `member.email`, `member.name`, `data.<field>` (what the record would become), `== != < <= > >=`, `and or not` and brackets. Changes and removals are live at once, and history keeps what the record was. The API takes `PATCH` and `DELETE /api/v1/content/<collection>/<id>`.

A collection can also sit in a `group` (the site's section) with a `position`: the admin nav lists collections in that order, and each group folds.
</details>

<details>
<summary><b>Reading content from your own front end</b></summary>

`GET /api/v1/content/<collection>` returns published records and `GET /api/v1/content/<collection>/<id>` returns one, following the collection's `read` rule.

| Parameter | Meaning |
| --- | --- |
| `field=value` | equals |
| `field__ne` `__gt` `__gte` `__lt` `__lte` `__contains` | not equal, greater, at least, less, at most, contains |
| `sort=field`, `sort=-field` | sort, `-` for descending |
| `limit`, `offset` | paging, `limit` from 1 to 100 |
| `expand=field` | replace a relation id with the record |

Only fields declared with `index` or `unique` can filter and sort.
</details>

<details>
<summary><b>Uploads</b></summary>

`POST /api/v1/media` with a multipart field named `file` and a key with `write`, up to 20 MB, or the `upload_media` tool (base64, up to 5 MB). JPEG, PNG, GIF, WebP and PDF are accepted, judged by content rather than extension. Files are stored by content hash at `/uploads/<hash>.<ext>` and cached for a long time; add `?w=640` for a smaller copy of an image. Uploads are public, so do not upload private files.
</details>

<details>
<summary><b>Tools</b></summary>

| Tool | Permission | What it does |
| --- | --- | --- |
| `list_collections` `get_collection` | read | list collections, read one |
| `create_collection` `update_collection` `delete_collection` | structure | create, change (fields, access, `url` pattern, nav `group` and `position`), delete (records are kept) |
| `get_record` `query_records` `search` | read | read records, full-text search |
| `create_record` `update_record` `delete_record` | write | write records (and their `slug`) |
| `list_pending` `list_versions` | read | pending changes, release history |
| `publish` | publish | publish |
| `schedule_publish` `cancel_schedule` / `list_schedules` | publish / read | publish at a set time |
| `restore_version` `discard_draft` | write | put a version back as a draft, drop a draft |
| `list_views` / `set_view` `delete_view` | read / structure | admin screens |
| `get_site_notes` / `set_site_notes` | read / structure | notes for the assistant |
| `check_page` | read | check a draft page: render errors, dead links, missing files, omissions |
| `list_site_files` `get_site_file` | read | read site files (with their hand edits) |
| `write_site_file` `delete_site_file` `upload_site_bundle` | structure | write site files, upload a built site |
| `get_site_settings` / `set_site_settings` | read / structure | site name, languages, template text, import map, script origins |
| `translation_status` / `set_translation` | read / write | what is missing or out of date; write a translation |
| `set_member_login` | structure | turn member accounts on or off |
| `list_ends` / `set_end` `delete_end` | read / structure | list surfaces; create, change (title, who may enter), delete a surface with its files |
| `list_members` / `set_member_status` | read / write | list members, disable or enable one |
| `upload_media` | write | upload an image or PDF |

Every key can read. Changing structure, screens, site files and notes needs `structure`.
</details>

<details>
<summary><b>Environment</b></summary>

| Variable | Default | Meaning |
| --- | --- | --- |
| `HC_HOST` | `127.0.0.1` | address to listen on (`0.0.0.0` in the Docker image) |
| `HC_PORT` | `4200` | port |
| `HC_DATA` | `data` | data folder, uploads included (`/data` in the Docker image) |
| `HC_DB` | `<HC_DATA>/harness.db` | database file |
| `HC_ORIGIN` | `http://127.0.0.1:<port>` | the public address, exactly as the browser shows it |
| `HC_SITE_NAME` | `CMS Harness` | the name until one is set in Settings |
| `HC_ADMIN_PASSWORD` | random | password for `cms-harness admin` |
| `HC_TRUST_PROXY` | unset | `1` when the reverse proxy runs on another machine |
| `HC_ADMIN_ORIGIN` | unset | a separate admin address (`https://admin.example.com`), or `off` |
| `LLM_PROVIDER` `LLM_API_KEY` `LLM_MODEL` `LLM_BASE_URL` | unset | the chat model; overrides Settings |

With nginx, add `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;` and `proxy_set_header Host $host;` so rate limits tell visitors apart. Caddy does this by default.
</details>

## 🔒 Security

Read [SECURITY.md](SECURITY.md) before you run scripts on the site that you did not write. It explains when to give the admin its own address (**Settings → Address**).

## 📄 License

[AGPL-3.0](LICENSE). Sites built with CMS Harness, front ends that read its API, and the content in its database are not derivative works. You only need to share your changes if you modify CMS Harness itself and offer it as a service. Contributions need a CLA; see [CONTRIBUTING.md](CONTRIBUTING.md).

---

<p align="center">
  If CMS Harness is the kind of CMS you wanted, a ⭐ helps other people find it.
</p>
