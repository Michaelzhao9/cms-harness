# CMS Harness

[中文](#中文) · [English](#english)

## 中文

一个让 AI 来管理的内容后台。部署一个可执行文件，你的 agent（Claude Code、Codex 或任何支持 MCP 的客户端）就能建结构、写内容、发布；你的网站通过内容接口读取已发布的内容。

所有修改先存为草稿，发布是单独的一步、单独的权限。每次发布都留有历史，可以恢复。

### 快速开始

需要 Rust 1.85 或更新版本。

```sh
cargo build --release
./target/release/cms-harness key 我的agent write structure publish   # 打印一把密钥，只显示一次
./target/release/cms-harness                                          # 启动，监听 127.0.0.1:4200
```

对外提供服务时，请在前面放一个反向代理（例如 nginx）负责 HTTPS。服务只监听本机地址。

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

`GET /api/v1/content/<集合>` 返回已发布的记录，`GET /api/v1/content/<集合>/<编号>` 返回一条。集合设为公开时不需要密钥，否则带上任意有效密钥。

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

改动结构、界面和站点说明还需要 structure 权限。read 权限总是包含。

### 环境变量

| 变量 | 默认值 | 作用 |
| --- | --- | --- |
| `HC_PORT` | `4200` | 端口 |
| `HC_DATA` | `data` | 数据目录，上传的文件也在这里 |
| `HC_DB` | `<HC_DATA>/harness.db` | 数据库文件 |
| `HC_ORIGIN` | `http://127.0.0.1:<端口>` | 对外地址 |
| `HC_SITE_NAME` | `CMS Harness` | 站点名称 |
| `HC_ADMIN_PASSWORD` | 无 | `cms-harness admin` 使用的密码，不设则随机生成 |

### 当前范围

这一版只有接口，没有后台界面，也没有内置的对话框。后台界面和对话工作台是下一阶段的工作。

### 许可证

本项目以 [AGPL-3.0](LICENSE) 发布。用它搭建的网站、通过接口读取内容的前台、存在数据库里的内容，都不是本项目的衍生作品，不受这份许可证约束；只有修改了本项目代码并对外提供服务时，才需要公开你的修改。

贡献代码需要签署贡献者授权协议，见 [CONTRIBUTING.md](CONTRIBUTING.md)。

## English

A content backend built to be run by AI. Deploy one binary; your agent (Claude Code, Codex, or any MCP client) defines structure, writes content and publishes; your site reads published content over the content API.

Every change is a draft first. Publishing is a separate step with a separate permission. Every release keeps history you can restore from.

### Quick start

Requires Rust 1.85 or newer.

```sh
cargo build --release
./target/release/cms-harness key my-agent write structure publish   # prints a key once
./target/release/cms-harness                                        # serves on 127.0.0.1:4200
```

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

`GET /api/v1/content/<collection>` returns published records; `GET /api/v1/content/<collection>/<id>` returns one. Public collections need no key; others need any valid key.

Uploads: `POST /api/v1/media` (multipart, field `file`, write scope, up to 20 MB) or the `upload_media` tool (base64, up to 5 MB). JPEG, PNG, GIF, WebP and PDF are accepted, detected from the content. Files are served at `/uploads/<32 hex>.<ext>`, named by content, cached long-term, and public regardless of collection settings.

Query parameters: `field=value` (equals); `field__ne`, `__gt`, `__gte`, `__lt`, `__lte`, `__contains`; `sort=field` or `sort=-field`; `limit` (1 to 100) and `offset`; `expand=field` for relation fields. Only fields declared with `index` or `unique` can be filtered or sorted on.

### Tools

21 tools: collections (`list_collections`, `get_collection`, `create_collection`, `update_collection`, `delete_collection`), records (`create_record`, `update_record`, `delete_record`, `get_record`, `query_records`), publishing and history (`list_pending`, `publish`, `list_versions`, `restore_version`, `discard_draft`), admin views (`list_views`, `set_view`, `delete_view`), site notes (`get_site_notes`, `set_site_notes`), and `upload_media`. Scopes are `read` (always included), `write`, `structure` and `publish`; the table in the Chinese section lists which tool needs which.

### Environment

`HC_PORT` (4200), `HC_DATA` (`data`), `HC_DB` (`<HC_DATA>/harness.db`), `HC_ORIGIN` (`http://127.0.0.1:<port>`), `HC_SITE_NAME` (`CMS Harness`), `HC_ADMIN_PASSWORD` (used by `cms-harness admin`; random when unset).

### Scope of this release

API only. The admin interface and the built-in chat workspace are the next stage.

### License

[AGPL-3.0](LICENSE). Sites built with it, front ends that read content over the API, and the content stored in it are not derivative works and are not covered by this license; only changes to this project's own code, offered as a service, must be shared.

Contributions require a contributor license agreement; see [CONTRIBUTING.md](CONTRIBUTING.md).
