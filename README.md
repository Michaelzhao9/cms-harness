# CMS Harness

[中文](#中文) · [English](#english)

## 中文

一个完全独立运行、让 AI 来管理的网站后台。部署一个可执行文件，就有了网站、后台和内置的对话助手：在后台里直接说“首页标题改成……”“加一个预约表单”“网站加个会员登录”，AI 就会去改内容、改页面、建后台界面。你自己的 agent（Claude Code、Codex 或任何支持 MCP 的客户端）也能接进来做同样的事。

所有修改先存为草稿，只在后台的预览里看得到；点「发布」才上线。每次发布都留有历史，可以恢复。

### 快速开始

需要 Rust 1.85 或更新版本。

```sh
cargo build --release
./target/release/cms-harness admin 你的邮箱                           # 新建后台管理员，打印密码，只显示一次
./target/release/cms-harness                                          # 启动，监听 127.0.0.1:4200
```

打开 `http://127.0.0.1:4200/admin` 登录。网站本身在 `http://127.0.0.1:4200/`。

对外提供服务时，请在前面放一个反向代理（例如 nginx）负责 HTTPS。服务只监听本机地址。

### 对话助手用的模型

在 后台 → 设置 → 对话用的模型 里选一家服务、填密钥、点「检测」，就能看到这把密钥能用的所有型号，选一个保存。内置 Anthropic Claude、OpenAI、Google Gemini、DeepSeek、通义千问、Kimi、智谱 GLM、豆包、硅基流动、Mistral、xAI、Groq、OpenRouter，以及本机的 Ollama、LM Studio 和任何兼容 OpenAI 的接口。也可以用环境变量 `LLM_PROVIDER`（`anthropic` 或 `openai`）、`LLM_API_KEY`、`LLM_MODEL`、`LLM_BASE_URL` 配置，环境变量优先。不配置模型时，后台和接口照常可用，只是没有对话。

对话助手不能发布：它做的每一处修改都是草稿，由人点「发布」上线。它把记录、网页、上传文件里的文字当数据，不当指令。

### 网站

网站由“网站文件”组成：HTML（可以用 `{{ }}` 取内容，比如 `collection("news")`、`record("news", 3)`、当前会员 `member`）、CSS、JavaScript，以及 JSX、TypeScript、TSX（程序自动转换）。React、Vue、Three.js 等库通过 import map 从 `https://esm.sh` 加载，不需要打包；用 Vite 等工具打包好的网站也可以整个上传（工具 `upload_site_bundle`，图片、字体一并上传）。没有对应文件的地址会用内置的简单页面显示公开的集合。

网站永远只显示已发布的内容，不管谁来访问、带什么登录信息。草稿只在后台的预览里看得到。

### 会员和访问规则

工具 `set_member_login` 打开会员注册登录后，网站就有 `/account/signup`、`/account/login`、`/account/logout` 页面和 `/api/v1/members/*` 接口，后台左边自动出现「用户管理」；关掉后它自动消失。会员和后台管理员是两套账号，会员登录不能进后台。

每个集合可以设定：
- 谁能看（`read`）：`public` 所有人、`members` 登录会员、`owner` 每个会员只看自己的、`private` 只有后台和接口密钥（默认）。
- 谁能从网站提交（`create`）：`none`（默认）、`members`、`public`。提交的内容直接上线并记下提交人，后台左边会显示“N 条新”。网站表单提交到 `POST /<集合>/new`，接口提交到 `POST /api/v1/content/<集合>`。

### 后台界面

左边的后台界面由对话助手按需要建：网站加了会产生数据的功能（表单、会员、评论、订单……），同一轮里就会建好对应的管理界面；功能删掉时界面也一起删掉。界面可以是表格、卡片或列表，可以按单选字段分组（工具 `set_view`）。

### 技能

后台 → 设置 → 技能 里可以安装技能（一个带 `SKILL.md` 的文件夹：GitHub 文件夹链接、zip 或上传），存在后台的数据库里。对话遇到相关的事会自己去读。技能里的脚本只当参考，不会被运行；技能只能由管理员安装，对话助手自己不能装。

### 网址

后台 → 设置 → 网址 可以选网站和后台用同一个网址（默认），或者后台用单独的网址（比如 `admin.example.com`）。网站会运行不是你自己写的脚本时，建议分开，原因见 [SECURITY.md](SECURITY.md)。

### 接入 agent

支持 HTTP 传输的 MCP 客户端，配置如下：

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

也可以直接用 REST：`GET /api/v1/tools` 列出这把密钥能用的工具和参数说明，`POST /api/v1/tools/<工具名>` 调用。

### 前台读取内容

`GET /api/v1/content/<集合>` 返回已发布的记录，`GET /api/v1/content/<集合>/<编号>` 返回一条。按集合的“谁能看”规则：公开的不需要密钥；会员能看的需要会员登录或密钥；只看自己的，会员只拿到自己的记录；其余需要有效密钥。

| 参数 | 作用 |
| --- | --- |
| `字段=值` | 等于 |
| `字段__ne` `__gt` `__gte` `__lt` `__lte` `__contains` | 不等于、大于、大于等于、小于、小于等于、包含 |
| `sort=字段`、`sort=-字段` | 排序，`-` 表示倒序 |
| `limit`、`offset` | 分页，`limit` 为 1 到 100 |
| `expand=字段` | 把关联字段展开成目标记录 |

只有声明了 `index` 或 `unique` 的字段能用来筛选和排序。展开关联时，如果目标集合不公开而请求没有带密钥，这个字段保持为编号。

### 上传文件

`POST /api/v1/media`，multipart 表单，文件放在名为 `file` 的字段里，需要带 write 权限的密钥，单个文件最大 20 MB。也可以用工具 `upload_media` 以 base64 传（最大 5 MB）。支持 JPEG、PNG、GIF、WebP 和 PDF，按文件内容判断格式，不看扩展名；图片最大四千万像素。

返回的路径形如 `/uploads/<32 位十六进制>.png`，直接用 `GET` 读取。文件名由内容决定，同一个文件只存一份，长期缓存。上传的文件是公开的，不随集合的公开设置变化，不要上传需要保密的文件。

### 工具

| 工具 | 权限 | 作用 |
| --- | --- | --- |
| `list_collections` | read | 列出集合 |
| `get_collection` | read | 查看一个集合的定义 |
| `create_collection` | structure | 新建集合 |
| `update_collection` | structure | 增改删字段、改集合设置 |
| `delete_collection` | structure | 删除集合（记录保留，可恢复） |
| `create_record` | write | 新建记录 |
| `update_record` | write | 修改记录 |
| `delete_record` | write | 删除记录（可恢复） |
| `get_record` | read | 读一条记录 |
| `query_records` | read | 查询记录 |
| `list_pending` | read | 列出待发布的改动 |
| `publish` | publish | 发布 |
| `list_versions` | read | 查看发布历史 |
| `restore_version` | write | 把历史版本放回草稿 |
| `discard_draft` | write | 丢弃草稿 |
| `list_views` | read | 列出后台界面声明 |
| `set_view` | structure | 新建或修改后台界面声明 |
| `delete_view` | structure | 删除后台界面声明 |
| `get_site_notes` | read | 读站点说明 |
| `set_site_notes` | structure | 修改站点说明 |
| `upload_media` | write | 上传图片或 PDF |
| `list_site_files` `get_site_file` | read | 列出、读取网站文件 |
| `write_site_file` `delete_site_file` `upload_site_bundle` | structure | 写、删网站文件，上传整个打包好的网站 |
| `get_site_settings` | read | 读网站的 import map 和脚本来源 |
| `set_site_settings` | structure | 设 import map 和脚本来源（立即生效） |
| `set_member_login` | structure | 打开或关闭会员登录（草稿） |
| `list_members` | read | 列出会员（不含密码） |
| `set_member_status` | write | 停用或恢复会员 |

改动结构、界面、网站文件和站点说明需要 structure 权限。read 权限总是包含。用 `cms-harness key <名字> [write] [structure] [publish]` 生成密钥。

### 环境变量

| 变量 | 默认值 | 作用 |
| --- | --- | --- |
| `HC_PORT` | `4200` | 端口 |
| `HC_DATA` | `data` | 数据目录，上传的文件也在这里 |
| `HC_DB` | `<HC_DATA>/harness.db` | 数据库文件 |
| `HC_ORIGIN` | `http://127.0.0.1:<端口>` | 对外地址 |
| `HC_SITE_NAME` | `CMS Harness` | 站点名称 |
| `HC_ADMIN_PASSWORD` | 无 | `cms-harness admin` 使用的密码，不设则随机生成 |
| `HC_TRUST_PROXY` | 无 | 设为 `1` 时按反向代理加的 `X-Forwarded-For` 计算登录频率限制 |
| `LLM_PROVIDER` `LLM_API_KEY` `LLM_MODEL` `LLM_BASE_URL` | 无 | 对话助手用的模型，见上文；`LLM_API_KEY` 不设时读 `ANTHROPIC_API_KEY` |
| `HC_ADMIN_ORIGIN` | 无 | 后台单独的网址（比如 `https://admin.example.com`），设了就是“分开网址”；设成 `off` 强制“同一个网址”。会盖过 设置 → 网址 里的选择。进不去后台时也可以运行 `cms-harness admin-origin off` 切回同一个网址，正在运行的服务马上生效 |

### 许可证

本项目以 [AGPL-3.0](LICENSE) 发布。用它搭建的网站、通过接口读取内容的前台、存在数据库里的内容，都不是本项目的衍生作品，不受这份许可证约束；只有修改了本项目代码并对外提供服务时，才需要公开你的修改。

贡献代码需要签署贡献者授权协议，见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## English

A website backend that runs entirely on its own and is managed by AI. Deploy one binary and you have a website, an admin and a built-in chat: say "change the home headline", "add a booking form" or "add member login" and it edits content, pages and admin screens. Your own agent (Claude Code, Codex, any MCP client) can connect and do the same.

Every change is a draft that only the admin's preview shows; publishing makes it live. Every release keeps history you can restore from.

### Quick start

Requires Rust 1.85 or newer.

```sh
cargo build --release
./target/release/cms-harness admin you@example.com                 # creates an admin, prints the password once
./target/release/cms-harness                                        # serves on 127.0.0.1:4200
```

The admin is at `/admin`, the website at `/`. The chat's model is set in 设置 → 对话用的模型 (pick a service, paste a key, detect its models) or with `LLM_PROVIDER` (`anthropic`|`openai`), `LLM_API_KEY`, `LLM_MODEL`, `LLM_BASE_URL`. The chat never publishes.

The website is made of site files: HTML with `{{ }}` content tags, CSS, JS, and JSX/TS/TSX transpiled on the fly; React, Vue or Three.js load through an import map from esm.sh, and a built site (Vite and the like) uploads as a zip. The live site serves only published content to everyone; drafts show only in the admin's preview.

`set_member_login` turns on member sign-up and login (`/account/*`, `/api/v1/members/*`) and the admin gains 用户管理 while it is on. Collections set who may read (`public`, `members`, `owner`, `private`) and who may add from the site (`none`, `members`, `public`; live at once, owner stamped). Skills (folders with a `SKILL.md`) are installed in 设置 and read by the chat; nothing in them runs. See [SECURITY.md](SECURITY.md) for site and admin addresses.

Put a reverse proxy (nginx, Caddy) in front for HTTPS. The server only listens on the loopback address.

### Connect an agent

For an MCP client with HTTP transport:

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

Or use REST: `GET /api/v1/tools` lists the tools your key may call with their JSON Schemas; `POST /api/v1/tools/<name>` calls one.

### Read content from your site

`GET /api/v1/content/<collection>` returns published records; `GET /api/v1/content/<collection>/<id>` returns one, following the collection's read rule (public: anyone; members: a logged-in member or a key; owner: a member's own records or a key; private: a key).

Uploads: `POST /api/v1/media` (multipart, field `file`, write scope, up to 20 MB) or the `upload_media` tool (base64, up to 5 MB). JPEG, PNG, GIF, WebP and PDF are accepted, detected from the content. Files are served at `/uploads/<32 hex>.<ext>`, named by content, cached long-term, and public regardless of collection settings.

Query parameters: `field=value` (equals); `field__ne`, `__gt`, `__gte`, `__lt`, `__lte`, `__contains`; `sort=field` or `sort=-field`; `limit` (1 to 100) and `offset`; `expand=field` for relation fields. Only fields declared with `index` or `unique` can be filtered or sorted on.

### Tools

Collections, records, publishing and history, admin views (`set_view` with table, cards or list layouts), site notes, uploads, site files (`list_site_files`, `get_site_file`, `write_site_file`, `delete_site_file`, `upload_site_bundle`), site settings (`get_site_settings`, `set_site_settings`) and members (`set_member_login`, `list_members`, `set_member_status`). Scopes are `read` (always included), `write`, `structure` and `publish`; the table in the Chinese section lists which tool needs which.

### Environment

`HC_PORT` (4200), `HC_DATA` (`data`), `HC_DB` (`<HC_DATA>/harness.db`), `HC_ORIGIN` (`http://127.0.0.1:<port>`), `HC_SITE_NAME` (`CMS Harness`), `HC_ADMIN_PASSWORD` (used by `cms-harness admin`; random when unset), `HC_TRUST_PROXY` (`1` behind a reverse proxy). `HC_ADMIN_ORIGIN`: the admin's own origin (e.g. `https://admin.example.com`) for separate site and admin addresses, or `off` to force one shared address; it overrides 设置 → 网址. `cms-harness admin-origin <url|off>` changes the saved choice and applies to a running server at once.

### License

[AGPL-3.0](LICENSE). Sites built with it, front ends that read content over the API, and the content stored in it are not derivative works and are not covered by this license; only changes to this project's own code, offered as a service, must be shared.

Contributions require a contributor license agreement; see [CONTRIBUTING.md](CONTRIBUTING.md).
