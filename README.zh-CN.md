# Target Ops

**面向本机与 SSH 主机、由策略严格约束的 MCP 控制平面。**

[English](README.md) · **简体中文**

Target Ops 是一个自包含的 Rust 服务，使 MCP 客户端能够在本机或已配置的
SSH 主机上执行命令、管理文件、持续读取后台任务输出，以及打开持久终端。
每个目标都可以独立配置权限、可访问目录、超时时间与输出上限。

它既可以通过 stdio 服务本地 MCP 客户端，也可以通过 HTTP 服务远程 MCP
客户端与 ChatGPT。HTTP 模式还内置 OAuth、受严格限制的 GPT Actions REST
接口、ChatGPT 文件导入/导出元数据、用于目标与下游 MCP 服务器列表的紧凑卡片、
用于 `exec` 的非交互式命令结果 MCP App，以及用于直观审阅文件变更的 MCP App。

> [!CAUTION]
> Target Ops 能够执行命令和修改文件。除非确有需要，否则应保持本机目标禁用；
> SSH 使用低权限账户；严格限制 `allowed_roots`；写操作要求显式目标；对外提供
> HTTP 服务时必须同时使用 HTTPS 与身份验证。

## 核心能力

- 本机与 SSH 目标共用一套包含 32 个工具的接口
- 普通远程命令与文件操作复用持久 OpenSSH worker
- 支持前台命令，以及可取消、可增量读取输出的后台任务
- 支持持久 PTY 终端和实时窗口尺寸调整
- 原子文件写入、精确文本替换、删除预览、SHA-256 比较并交换，以及带回滚的
  多文件补丁
- 从文件解析秘密并注入环境变量，秘密值不会出现在 MCP 参数中
- 支持 ChatGPT/连接器文件导入导出，并限制传输大小
- 自包含 MCP App：目标/MCP 服务器列表使用紧凑卡片；`exec` 使用非交互式命令
  结果界面并安全渲染 ANSI 样式；文件变更对新增/删除显示完整内容，对修改显示
  编辑器式差异
- 仅允许访问预先配置服务器的下游 Streamable HTTP MCP 网关
- 同时支持 stdio、HTTP、静态 Bearer Token、PKCE OAuth 2.0，以及持久化的
  轮换刷新令牌
- 自动生成 OpenAPI 3.1 文档，GPT Actions 仅暴露精简且受限的接口面

## 架构

```text
MCP / ChatGPT 客户端
        │
        ├── stdio ─────────────────────────────┐
        └── HTTP + Bearer 或 OAuth ────────────┤
                                               ▼
                                        MCP / Actions 层
                                               │
                                      目标解析与策略检查
                                               │
                   ┌───────────────────────────┴───────────────────────────┐
                   ▼                                                       ▼
                本机进程                                               OpenSSH worker
          命令 / 文件 / PTY                                      命令 / 文件 / PTY

下游 MCP 服务通过独立的白名单 Streamable HTTP 网关访问；调用者不能临时传入
任意服务端地址。
```

目标解析顺序如下：

1. 工具参数中显式传入的 `target`
2. 通过 `target_select` 选择的进程级活动目标
3. `server.default_target`

默认目标只是回退值，不会被报告为活动目标。当
`require_explicit_target_for_write = true` 时，通过活动目标或默认目标解析仍不足以
执行文件写操作，调用者必须显式传入 `target`。

### 架构边界

本次重构有意保留四个顶层边界，因为它们与系统的安全模型和依赖边界一致：

- `core` 负责配置不变量、目标身份、策略、OAuth 状态和进程级注册表，不感知 HTTP
  路由或远程 shell 语法。
- `protocol` 把 MCP、HTTP、OAuth 和 GPT Actions 请求转换为既有操作；路由、授权
  页面渲染、表单解码、响应生成与 OpenAPI 生成分别位于独立模块。
- `tooling` 负责操作语义，并在接触后端前完成目标解析与策略检查；文件请求/响应
  类型和多文件补丁解析已与文件操作实现分离。
- `transport` 负责 OpenSSH 进程行为；持久 worker 生命周期与 POSIX 远程文件协议
  均封装在 SSH facade 后面。

服务仍有意保持为小型阻塞式 Rust 程序，并使用系统 OpenSSH 客户端。当前负载是有
明确上限的运维操作，而不是高吞吐 Web 流量；现在引入异步运行时、通用后端 trait
体系或远端语言运行时，只会增加复杂度，并不会改善现有信任边界。未来如改用原生
SSH/SFTP，只需替换 `transport/ssh`，无需改变 MCP 工具或策略语义。

## 运行要求

- 从源码编译时需要 Rust stable，以及 `rustfmt`、`clippy`
- 使用 SSH 目标时需要系统中的 `ssh` 可执行文件
- 远程环境应具有兼容 POSIX 的 shell，以及 `cat`、`mktemp`、`mv`、`rm`、
  `chmod`、`mkdir`、`stat` 等常用工具
- 对公网发布 HTTP 服务时需要 HTTPS 反向代理

仓库通过 [`rust-toolchain.toml`](rust-toolchain.toml) 固定 stable 工具链及所需组件。

## 快速开始

### 1. 编译

```bash
git clone git@github.com:abwuge/mcp-target-ops.git
cd mcp-target-ops
cargo build --locked --release
```

生成的程序位于 `target/release/mcp-target-ops`。

### 2. 创建配置

```bash
mkdir -p ~/.config/mcp-target-ops
cp examples/config.toml ~/.config/mcp-target-ops/config.toml
```

一个最小的 SSH 目标配置如下：

```toml
[server]
default_target = "ssh:dev"

[targets.dev]
kind = "ssh"
host = "dev.example.com"
user = "ubuntu"
identity_file = "/home/me/.ssh/id_ed25519"
extra_args = ["-o", "BatchMode=yes"]

[targets.dev.policy]
allow_exec = true
allow_terminal = true
allow_file_read = true
allow_file_write = true
allow_select_active = true
require_explicit_target_for_write = true
allowed_roots = ["/home/ubuntu/app", "/tmp"]
default_timeout_ms = 30000
max_output_bytes = 200000
```

`[targets.dev]` 对外对应目标 ID `ssh:dev`。`[targets.local]` 专用于唯一的本机目标。

### 3. 运行

默认使用 stdio：

```bash
./target/release/mcp-target-ops \
  --config ~/.config/mcp-target-ops/config.toml
```

启用 HTTP：

```bash
./target/release/mcp-target-ops \
  --config ~/.config/mcp-target-ops/config.toml \
  --http 127.0.0.1:8765
```

通用的 stdio MCP 客户端配置示例：

```json
{
  "mcpServers": {
    "target-ops": {
      "command": "/absolute/path/to/mcp-target-ops",
      "args": [
        "--config",
        "/absolute/path/to/config.toml"
      ]
    }
  }
}
```

不同客户端的配置格式可能不同。若客户端不继承交互式 shell 环境，请始终使用
绝对路径。

## 命令行参数

```text
mcp-target-ops [--config PATH] [--http ADDR]
```

| 参数 | 含义 |
| --- | --- |
| `-c`、`--config PATH`、`--config=PATH` | TOML 配置路径 |
| `--http ADDR`、`--http-addr ADDR` | 使用 HTTP 而非 stdio |
| `-V`、`--version` | 输出程序版本 |
| `-h`、`--help` | 输出帮助信息 |

未指定 `--config` 时，Target Ops 依次检查 `MCP_TARGET_OPS_CONFIG`、
`~/.config/mcp-target-ops/config.toml`；若均不存在，则使用默认拒绝配置，其中本机
目标保持禁用。

## 配置

完整且带注释的配置示例位于
[`examples/config.toml`](examples/config.toml)。

### 服务设置

| 配置项 | 默认值 | 用途 |
| --- | --- | --- |
| `name` | `mcp-target-ops` | 向客户端公布的服务名 |
| `version` | 软件包版本 | 向客户端公布的版本 |
| `default_target` | 无 | 没有显式目标和活动目标时使用的回退目标 |
| `terminal_ring_buffer_bytes` | `524288` | 每个终端保留的输出大小 |
| `runtime_dir` | 系统临时目录下的 `mcp-target-ops` | 启动时创建的运行目录 |
| `http_bearer_token` | 无 | 保护 HTTP 路由的静态 Bearer Token |
| `public_base_url` | 无 | 用于元数据与 OpenAPI 的外部 HTTPS 源站地址 |
| `oauth_enabled` | `false` | 启用内置 OAuth 授权服务器 |
| `oauth_authorization_password` | 无 | 授权页的可选密码门禁 |
| `oauth_scopes` | `["mcp:tools"]` | OAuth 公布并校验的 scope |
| `oauth_allow_dynamic_client_registration` | `true` | 启用 `/oauth/register` |
| `oauth_authorization_code_ttl_secs` | `600` | 授权码有效期 |
| `oauth_access_token_ttl_secs` | `3600` | 访问令牌有效期 |
| `oauth_refresh_token_ttl_secs` | `2592000` | 刷新令牌有效期 |
| `oauth_state_file` | `~/.config/mcp-target-ops/oauth-state.json` | 持久化 OAuth 客户端和令牌 |

部署相关值也可以通过环境变量提供：

| 环境变量 | 用途 |
| --- | --- |
| `MCP_TARGET_OPS_CONFIG` | 默认配置文件路径 |
| `MCP_TARGET_OPS_HTTP_TOKEN` | 静态 HTTP Bearer Token |
| `MCP_TARGET_OPS_OAUTH` | 使用 `1`、`true`、`yes` 或 `on` 启用 OAuth |
| `MCP_TARGET_OPS_PUBLIC_BASE_URL` | 外部 HTTPS 源站地址，不含末尾 `/` |
| `MCP_TARGET_OPS_OAUTH_PASSWORD` | 授权页密码 |
| `MCP_TARGET_OPS_OAUTH_SCOPES` | 以空格分隔的 OAuth scope |
| `MCP_TARGET_OPS_OAUTH_STATE_FILE` | OAuth 状态文件路径 |

### 目标

本机目标必须写为 `[targets.local]` 且使用 `kind = "local"`，默认禁用。其他目标
必须使用 `kind = "ssh"`；名为 `[targets.<name>]` 的配置对外显示为
`ssh:<name>`。

本机目标字段：

| 配置项 | 用途 |
| --- | --- |
| `enabled` | 是否启用本机目标 |
| `shell` | 命令与终端使用的可选 shell 程序 |
| `policy` | 该目标的权限与限制 |

SSH 目标字段：

| 配置项 | 用途 |
| --- | --- |
| `enabled` | 是否启用该 SSH 目标 |
| `host` | SSH 主机名或地址 |
| `port` | SSH 端口，默认 `22` |
| `user` | 可选 SSH 用户名 |
| `identity_file` | 传递给 OpenSSH 的可选私钥路径 |
| `extra_args` | 额外 OpenSSH 参数，例如 `BatchMode=yes` |
| `shell` | 可选远程 shell 程序 |
| `policy` | 该目标的权限与限制 |

主机密钥校验、SSH Agent、跳板机、Kerberos 等行为仍由 OpenSSH 负责。应优先在
常规 SSH 配置中设置；使用 `extra_args` 时需仔细审查。

### 目标策略

所有能力默认拒绝，必须逐项启用。

| 配置项 | 默认值 | 用途 |
| --- | --- | --- |
| `allow_exec` | `false` | 允许前台和后台命令 |
| `allow_terminal` | `false` | 允许持久 PTY 终端 |
| `allow_file_read` | `false` | 允许读取文件和目录 |
| `allow_file_write` | `false` | 允许修改、导入、移动、chmod 和 mkdir |
| `allow_select_active` | `false` | 允许通过 `target_select` 选中该目标 |
| `require_explicit_target_for_write` | `true` | 拒绝通过活动/默认目标解析的写操作 |
| `allowed_roots` | 空 | 文件工具可访问的绝对路径根目录 |
| `default_timeout_ms` | `30000` | 默认命令和远程操作超时 |
| `max_output_bytes` | `200000` | 默认命令输出上限 |

即使启用了文件读写权限，空的 `allowed_roots` 仍会拒绝所有文件访问。

### 下游 MCP 网关

Target Ops 可以初始化和调用预先配置的 Streamable HTTP MCP 服务。推荐将端点与
凭据放在 Target Ops 独占、权限为 `0600` 的文件中：

```toml
# ~/.config/mcp-target-ops/config.toml
[mcp_servers.memory]
enabled = true
config_file = "/home/me/.config/mcp-target-ops/mcp/memory.toml"
timeout_ms = 20000
max_response_bytes = 1048576
```

```toml
# ~/.config/mcp-target-ops/mcp/memory.toml
url = "https://memory.example.com/mcp"

[headers]
Authorization = "Bearer replace-me"
```

在 Unix 上，若凭据文件可被组或其他用户读取，服务会拒绝使用。网关禁用重定向，
因此不会把认证头转发到重定向目标。`mcp_server_list` 只返回安全元数据，不暴露
URL 或秘密值。高级部署仍可使用内联 URL 或文件秘密引用，详见示例配置。

## 工具列表

Target Ops 当前发布 33 个工具描述：32 个模型可见工具，以及 1 个仅供 App 使用的 `exec_stream`。

| 类别 | 工具 |
| --- | --- |
| 服务 | `server_info` |
| 目标 | `target_list`、`target_current`、`target_select`、`target_connect`、`target_disconnect` |
| 下游 MCP | `mcp_server_list`、`mcp_tools_list`、`mcp_tool_call` |
| 命令与任务 | `exec`、`exec_batch`、`exec_start`、`job_poll`、`job_output`、`job_cancel`；仅 App：`exec_stream` |
| 文件与目录 | `file_read`、`file_list`、`file_find`、`file_edit`、`file_write`、`file_delete`、`file_import`、`file_export`、`file_patch`、`file_move`、`file_chmod`、`directory_create` |
| 终端 | `terminal_open`、`terminal_send`、`terminal_read`、`terminal_resize`、`terminal_close` |

每个工具都声明对象形式的 `outputSchema`，成功结果通过 `structuredContent` 返回，
并包含标准 MCP annotations。启用 OAuth 后，工具描述还会公布已配置的 OAuth
scope。

### 目标与 MCP 服务器列表卡片

`target_list` 与 `mcp_server_list` 共用稳定的
`ui://target-ops/inventory/v1.html` MCP App。该紧凑卡片会展示目标类型、active/enabled
状态、允许的操作与目录，或下游 MCP 服务器的启用状态、配置来源、超时、响应大小
限制和请求头数量信息。

### 命令与后台任务

`exec` 执行一个受限的非交互式 shell 命令或脚本。`exec_batch` 可在一次工具调用中
执行最多 32 个彼此独立的命令，默认顺序执行，也可以显式请求并行执行，并为每条命令
返回独立的结构化结果；顺序模式还可在首次失败时停止。若多条命令需要共享 `cd`、变量、
管道或控制流等 shell 状态，应继续使用 `exec`；若只是为了减少工具调用而批量进行互不
依赖的检查，应优先使用 `exec_batch`，而不是用 shell 分隔符强行拼接。

两者都绑定到稳定的 `ui://target-ops/exec-terminal/v1.html` MCP App。前台 `exec`
运行时，App 会通过仅 App 可见的 `exec_stream` 附着到对应会话，并使用相互独立的
stdout/stderr 序号游标轮询增量输出。输出在运行期间默认展开；成功结束后约 3 秒自动
折叠，失败和超时保持展开。批量命令同样先展开，成功项结束后再延迟折叠。ANSI SGR
颜色/样式会被安全渲染。资源 URI 保持不变以兼容客户端缓存与资源发现。

`exec` 与 `exec_start` 现在共用同一套 `CommandSession`，统一处理进程生命周期、
stdout/stderr 捕获、超时、取消与增量输出。`exec` 会等待会话结束，并在未显式设置
超时时继续使用目标策略的默认 timeout；`exec_start` 会立即返回任务 ID，未指定时不
设置运行时 timeout。子进程 stdin 与 MCP 控制流隔离。SSH 命令会话使用独立 OpenSSH
进程；远程文件操作继续复用每个目标的持久 SSH worker。

两个命令工具都支持 `secret_env`。值可以来自纯文本文件，也可以来自 TOML/JSON
中以点号路径选中的标量：

```json
{
  "target": "local",
  "command": "./client",
  "secret_env": {
    "SERVICE_AUTH": {
      "path": "/home/me/service.toml",
      "format": "toml",
      "key": "client.headers.Authorization"
    }
  }
}
```

秘密来源必须通过普通文件读取策略。秘密值在 Target Ops 内部解析，不会出现在
MCP 请求参数中；但被启动的命令仍可通过输出环境或主动发送数据而泄露该值。

### 文件安全与补丁

- `file_read` 返回 SHA-256 元数据，并支持以 1 为起点的 UTF-8 行范围。它既兼容原有单 `path` 模式，也支持一次最多 32 个独立 `files[]` 读取；同一路径可用不同范围重复出现，从而一次读取多个区间。批量模式中的单项失败彼此隔离，整批共享一个有界输出预算。
- `file_find` 执行有界字面量搜索，可返回上下文。
- `file_write` 原子写入；覆盖已有文件时必须提供 `expected_sha256` 或
  `overwrite = true`。
- `file_edit`、单文件 `file_patch` 与 `file_delete` 支持 SHA-256 比较并交换；
  适用操作还支持 dry-run/预览。
- 未显式指定新权限时，已有 Unix 文件模式会被保留。
- `file_delete` 拒绝删除目录，并返回删除前的完整内容以供审阅。
- 多文件 `file_patch` 将 `path` 视为基目录；写入前验证所有 UTF-8 文件；拒绝逃逸
  基目录的路径；后续写入失败时回滚之前已写入的文件。多文件创建、删除和重命名
  section 会被明确拒绝。
- `file_move` 默认不覆盖；`file_chmod` 和 `directory_create` 使用与其他写操作
  相同的策略。

### 终端

`terminal_open` 启动持久 PTY，并永久绑定到当时解析出的目标。`terminal_read` 支持
增量读取，`terminal_send` 写入输入，`terminal_resize` 调整真实 PTY 尺寸。终端
输出使用有界环形缓冲区，所有会话均为进程内状态。

### ChatGPT 文件集成与审阅界面

`file_read` 绑定 `ui://target-ops/file-read/v1.html`，以带行号的代码阅读界面展示文本、范围与哈希元数据，并以可展开的紧凑列表展示批量结果。相比通过 `exec` 调用 `cat`/`sed`，结构化读取更容易审阅。

`file_import` 接受经运行时重写后的 ChatGPT/连接器文件参数，可以是已挂载本地
路径或 HTTPS 文件引用，并通过普通原子/CAS 写入策略落盘；HTTPS 重定向被禁用。
`file_export` 返回标准 MCP embedded resource 与 ChatGPT 兼容输出元数据。两个方向
默认上限均为 25 MiB，硬上限为 100 MiB。

文件变更工具绑定到稳定的 MCP App 资源：

```text
ui://target-ops/file-change/v1.html
```

该自包含组件会：

- 对新增和删除文件显示完整内容；
- 对修改后的 UTF-8 文件显示带行号的编辑器式行内差异；
- 在一个界面中展示多文件变更；
- 将哈希、大小、编码与目标信息收纳在折叠详情中；
- 请求带边框展示，并声明禁止外部网络和资源加载的 CSP；
- 配置 `server.public_base_url` 时，将其同时用作组件域名。

命令结果、文件读取、文件变更与 inventory 四个 MCP App 现在共用同一套带边框卡片体系：统一的 10 px 外圆角、标题区间距、等宽辅助文字、编辑器式内容区、可展开详情，以及一致的成功/警告/错误状态胶囊。OAuth 授权页仍保留完整页面所需的信息层级，但字体族、等宽字体与圆角处理也与工具界面保持一致。

修改工具描述或 App 元数据后，应在 ChatGPT 中重新连接或刷新应用，使客户端重新
获取 `tools/list` 和 MCP resources。

## HTTP 模式

Rust 服务应只监听本机，再通过 HTTPS 反向代理发布。若既未配置静态 Bearer Token，
也未启用 OAuth，受保护路由实际上不会要求身份验证，只适用于受信任的本地环境。

静态 Bearer Token：

```bash
MCP_TARGET_OPS_HTTP_TOKEN='replace-me' \
  ./target/release/mcp-target-ops --http 127.0.0.1:8765
```

客户端发送：

```text
Authorization: Bearer <token>
```

静态 Bearer 与 OAuth 可以同时启用，任一有效令牌都能通过受保护请求的认证。

### 面向远程 MCP 与 ChatGPT 的 OAuth

```toml
[server]
oauth_enabled = true
public_base_url = "https://mcp.example.com"
oauth_scopes = ["mcp:tools"]
oauth_authorization_password = "replace-me"
oauth_allow_dynamic_client_registration = true
oauth_authorization_code_ttl_secs = 600
oauth_access_token_ttl_secs = 3600
oauth_refresh_token_ttl_secs = 2592000
oauth_state_file = "/home/me/.config/mcp-target-ops/oauth-state.json"
```

内置服务器支持：

- OAuth protected-resource 与 authorization-server 元数据
- 公共动态客户端注册
- 强制 PKCE S256 的授权码流程
- HTTPS 回调地址和 HTTP loopback 回调地址
- 可选密码门禁、响应式布局和明暗模式的现代授权页，并显示已注册客户端信息，
  支持明确批准或取消
- 不透明访问令牌
- 绑定客户端、资源与 scope 的刷新令牌轮换

客户端、访问令牌和刷新令牌会原子持久化到 OAuth 状态文件。在 Unix 上，该文件
权限必须为 `0600`。授权码仍只保存在内存中且寿命很短。每次成功刷新都会返回新
刷新令牌并使旧令牌失效。由于客户端和令牌状态已持久化，ChatGPT 完成一次授权后，
服务重启通常仍可正常刷新，无需再次输入授权密码。删除状态文件会有意撤销所有已
持久化授权状态。

### HTTP 端点

| 端点 | 访问方式与用途 |
| --- | --- |
| `GET /` | 公开的服务摘要与端点发现 |
| `GET /health` | 公开健康状态 |
| `GET /favicon.ico` | 公开应用图标 |
| `POST /` 或 `POST /mcp` | MCP JSON-RPC；配置认证后受保护 |
| `DELETE /mcp` | 确认关闭 MCP session；配置认证后受保护 |
| `GET /openapi.json` | 公开 GPT Actions OpenAPI 3.1 文档 |
| `/actions/v1/*` | 受保护且严格限额的 GPT Actions 接口 |
| `/.well-known/oauth-protected-resource` | 启用 OAuth 时的资源元数据 |
| `/.well-known/oauth-authorization-server` | 启用 OAuth 时的授权服务器元数据 |
| `GET|POST /oauth/authorize` | 授权页面，以及批准或拒绝请求 |
| `POST /oauth/token` | 授权码与刷新令牌交换 |
| `POST /oauth/register` | 启用时的动态客户端注册 |

### GPT Actions 接口

Actions API 仅暴露六个操作，并要求每个主机操作都显式指定目标。

| Operation ID | 端点 | Consequential |
| --- | --- | --- |
| `listTargets` | `GET /actions/v1/targets` | 否 |
| `executeCommand` | `POST /actions/v1/commands/execute` | 是 |
| `readFile` | `POST /actions/v1/files/read` | 否 |
| `listDirectory` | `POST /actions/v1/directories/list` | 否 |
| `previewFileEdits` | `POST /actions/v1/files/edits/preview` | 否 |
| `applyFileEdits` | `POST /actions/v1/files/edits/apply` | 是 |

该接口比 MCP 工具应用更严格的限制：请求体 64 KiB、操作超时 30 秒、命令输出
24 KiB、文件读取和 diff 32 KiB、目录最多返回 100 项、最终响应最多 90,000 个
字符。应用修改时还必须提供 `expected_sha256`。

Bearer 身份验证只代表一个共享服务身份，不是多用户授权系统，也不会额外提供按
用户划分的目标权限。

## 安全模型

Target Ops 使用多层控制，而不是把认证当作唯一边界：

1. 目标必须存在且已启用。
2. 每类操作必须在目标策略中单独启用。
3. 文件路径必须位于已配置根目录中。
4. 写操作默认要求显式目标。
5. 覆盖已有文件时可使用 SHA-256 比较并交换。
6. 命令时间、保留输出、文件传输与下游响应均有限额。
7. 下游 MCP 端点只能由服务端预配置，且禁止重定向。
8. 对外 HTTP 部署应同时使用 HTTPS 与 Bearer/OAuth 身份验证。

服务进程应使用专用账户，每个远程环境也应使用独立低权限账户。OpenSSH 参数与
主机密钥应在 Target Ops 之外审查。不要把秘密直接写进命令或日志；任何被允许的
命令都能使用其操作系统账户拥有的全部权限。

## Docker 与 CI

仓库中的多阶段 [`Dockerfile`](Dockerfile) 会构建 release 程序，并在 Debian 运行
镜像中安装 `openssh-client`：

```bash
docker build -t mcp-target-ops .
```

部署时应显式挂载配置、SSH 材料以及可写的 OAuth 状态目录，不能把凭据打进镜像。

GitHub Actions 包含：

- `CI`：格式检查、测试与 Clippy
- `Linux binaries`：x86_64 与 aarch64 GNU release 构建产物
- `Docker images`：手动触发的多架构 GHCR 发布

## 开发

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
```

源码结构：

```text
assets/
  exec-terminal.html     MCP App 非交互式 exec 结果界面
  file-change.html       MCP App 文件审阅界面
  file-read.html         MCP App 单文件/批量文件读取界面
  inventory-card.html    MCP App 目标与 MCP 服务器列表卡片
  oauth-authorize.html   OAuth 授权页

src/core/
  config.rs              配置类型、默认值与加载
  config/validation.rs   配置不变量与目标校验
  oauth.rs               持久 OAuth 状态与令牌轮换
  policy.rs              目标权限与路径约束
  state.rs               进程内注册表与目标解析

src/protocol/
  actions/mod.rs         受限 GPT Actions 路由与响应限额
  actions/openapi.rs     从 MCP 工具目录派生 OpenAPI 文档
  apps.rs                MCP App 资源元数据
  html.rs                共用 HTML 转义
  http/mod.rs            HTTP 路由与公开端点发现
  http/auth.rs           Bearer Token 授权
  http/form.rs           URL 编码表单与查询解析
  http/oauth.rs          OAuth 元数据、注册、授权与令牌交换
  http/response.rs       HTTP 响应、重定向、CORS 与安全头
  oauth_page.rs          授权页面渲染
  mcp.rs                 stdio/HTTP 上的 MCP JSON-RPC

src/tooling/
  tools/catalog.rs       工具描述与输入 Schema
  tools/dispatch.rs      工具分发与目标连接操作
  tools/schema.rs        输出 Schema
  exec.rs, job.rs        前台和后台命令
  fs.rs                  带策略校验的文件操作语义
  fs/backend.rs          本机/SSH 字节 I/O 与本机原子写入
  fs/types.rs            文件请求与响应类型
  fs/patch_format.rs     多文件 unified diff 解析与路径校验
  file_bridge.rs         ChatGPT 与连接器文件桥
  terminal.rs            持久 PTY 终端
  mcp_client.rs          白名单下游 MCP 客户端
  secret.rs              文件秘密解析

src/transport/
  ssh/mod.rs             SSH facade 与命令/PTY 构造
  ssh/session.rs         持久 OpenSSH worker 生命周期
  ssh/files.rs           POSIX 远程文件协议
```

## 已知边界

- 活动目标、后台任务、终端和授权码均为进程内状态；配置状态文件后，OAuth 客户端
  与令牌会持久化。
- SSH 支持有意依赖系统 OpenSSH 客户端和类 POSIX 远程环境。
- 多文件补丁只能修改已有 UTF-8 文件，创建、删除和重命名 section 会被拒绝。
- 内置 OAuth 面向私有服务部署，不是带用户账户和管理式撤销功能的完整身份提供方。

## 许可证

[MIT](LICENSE)
