# Target Ops

**面向本机与 SSH 主机、由策略严格约束的 MCP 控制平面。**

[English](README.md) · **简体中文** · [兼容性债务](COMPATIBILITY.md)

Target Ops 是一个自包含的 Rust 服务，使 MCP 客户端能够在本机或已配置的
SSH 主机上执行命令、管理文件、持续读取后台任务输出，以及打开持久终端。
每个目标都可以独立配置权限、可访问目录、超时时间与输出上限。

它既可以通过 stdio 服务本地 MCP 客户端，也可以通过 HTTP 服务远程 MCP
客户端与 ChatGPT。HTTP 模式还内置 OAuth、ChatGPT 文件导入/导出元数据、用于
目标与下游 MCP 服务器列表的紧凑卡片、用于 `exec` 的非交互式命令结果 MCP App，
以及用于直观审阅文件变更的 MCP App。

> [!CAUTION]
> Target Ops 能够执行命令和修改文件。除非确有需要，否则应保持本机目标禁用；
> SSH 使用低权限账户；严格限制 `allowed_roots`；写操作要求显式目标；对外提供
> HTTP 服务时必须同时使用 HTTPS 与身份验证。

## 核心能力

- 本机与 SSH 目标共用一套包含 39 个模型可见工具与 2 个 App-only 辅助工具的接口
- 普通远程命令与文件操作复用持久 OpenSSH worker
- 支持前台命令，以及可取消、可增量读取输出的后台任务
- 支持持久 PTY 终端和实时窗口尺寸调整
- 原子文件写入、精确文本替换、删除预览、SHA-256 比较并交换、带回滚的
  多文件补丁，以及统一管理且自动过期的文件备份
- 从文件解析秘密并注入环境变量，秘密值不会出现在 MCP 参数中
- 支持 ChatGPT/连接器文件导入导出，并限制传输大小
- 自包含 MCP App：目标/MCP 服务器列表使用紧凑卡片；`exec` 使用非交互式命令
  结果界面并安全渲染 ANSI 样式；文件变更对新增/删除显示完整内容，对修改显示
  编辑器式差异
- 仅允许访问预先配置服务器的下游 Streamable HTTP MCP 网关
- 同时支持 stdio、HTTP、静态 Bearer Token、PKCE OAuth 2.0，以及持久化的
  轮换刷新令牌

## 架构

```text
MCP / ChatGPT 客户端
        │
        ├── stdio ─────────────────────────────┐
        └── HTTP + Bearer 或 OAuth ────────────┤
                                               ▼
                                             MCP 层
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
- `protocol` 把 MCP、HTTP 和 OAuth 请求转换为既有操作；路由、授权页面渲染、
  表单解码与响应生成分别位于独立模块。
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
- 使用 SSH 目标时需要系统中的 `ssh` 与 `scp` 可执行文件；跨 target 文件传输在
  可用时优先使用 `rsync`
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

### 2. 配置

不需要额外安装器或 bootstrap 文件。单二进制首次启动时，会根据内置模板自动创建 `~/.config/mcp-target-ops/config.toml`（或由 `MCP_TARGET_OPS_CONFIG` / `--config` 指定的路径）；Unix 上新文件权限为 `0600`。也可以直接从完整注释示例开始：

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
| `--http ADDR`、`--http-addr ADDR` | 使用 HTTP 而非 stdio（`--http-addr` 为兼容别名，见 `COMPAT-007`） |
| `-V`、`--version` | 输出程序版本 |
| `-h`、`--help` | 输出帮助信息 |

未指定 `--config` 时，若设置了 `MCP_TARGET_OPS_CONFIG` 就使用该路径，否则使用 `~/.config/mcp-target-ops/config.toml`。若选中的配置文件不存在，Target Ops 会先从内置的默认拒绝模板生成配置，再继续启动。

## 配置

完整且带注释的配置示例位于 [`examples/config.toml`](examples/config.toml)。

### 配置文件生命周期

每次启动时，Target Ops 都会先解析已有 TOML，把当前二进制新引入而用户文件尚缺失的字段合并进去，验证合并后的配置，然后回写文档。已有值、注释、自定义目标以及未知的兼容字段均会保留；升级后新增的默认参数会立即出现在配置文件中并可编辑。来自环境变量的秘密值不会在这一过程中被复制到 TOML。

如果希望磁盘文件保持原样，可设置：

```toml
[config]
rewrite_on_start = false
```

此时缺失字段仍会在内存中使用当前版本的内置默认值，只是不回写磁盘。

进程级运行参数集中在 `[runtime]`：

| 配置项 | 默认值 | 用途 |
| --- | ---: | --- |
| `result_cache_max_bytes` | `104857600` | App 会话结果缓存总预算；超限时优先淘汰最久未活动的完整会话 |
| `max_retained_jobs` | `128` | 进程内保留的后台任务条目数 |
| `max_retained_foreground_execs` | `64` | 为 App 附着保留的前台 exec 会话数 |
| `exec_auto_background_after_ms` | `5000` | 自适应 `exec` 在前台等待多久后自动转后台 |
| `stream_default_max_bytes` | `65536` | 增量流/读取的默认单次字节上限 |
| `stream_max_bytes` | `524288` | 增量流/读取允许的最大单次字节上限 |
| `job_wait_default_timeout_ms` | `60000` | `job_wait` 服务端默认等待窗口 |
| `job_wait_max_timeout_ms` | `120000` | `job_wait` 服务端最大等待窗口 |
| `file_transfer_default_max_bytes` | `26214400` | `file_import` / `file_export` 默认传输上限 |
| `file_export_delivery` | `link` | `file_export` 默认交付方式：临时下载链接或兼容 attachment |
| `file_export_link_ttl_secs` | `600` | 下载链接默认有效期；硬上限为 86400 秒 |
| `file_export_link_single_use` | `false` | 首次成功 GET 后是否失效；默认关闭以兼容预取 |
| `file_download_timeout_ms` | `30000` | HTTPS 连接器文件下载默认超时 |
| `file_backup_default_ttl_secs` | `86400` | 托管回滚快照默认保留时间（24 小时） |
| `file_backup_max_ttl_secs` | `2592000` | 单次请求可设置的最大备份保留时间（默认/硬上限 30 天） |
| `file_backup_cleanup_interval_secs` | `60` | 后台清理间隔；正常工具流量也会节流触发清理作为兜底 |
| `file_backup_max_file_bytes` | `104857600` | 单个托管备份最大大小 |
| `file_backup_store_max_bytes` | `1073741824` | 托管备份总磁盘预算；超限优先淘汰最旧快照 |
| `file_backup_max_entries` | `1024` | 最大保留快照数；同时限制空文件/小文件备份产生的元数据条目 |
| `terminal_default_rows` | `30` | PTY 默认行数 |
| `terminal_default_cols` | `120` | PTY 默认列数 |
| `app_success_collapse_ms` | `3000` | App 成功卡片自动折叠延迟 |
| `app_failure_collapse_ms` | `6000` | App 失败/warning 卡片自动折叠延迟 |
| `app_sleep_after_ms` | `30000` | 折叠卡片释放重 DOM、转入可恢复 sleep 的延迟 |
| `app_job_poll_interval_ms` | `1000` | App 收到任务输出响应后，到下一次轮询的等待时间 |

100 MiB 文件传输绝对上限等安全/协议硬限制仍编译在程序中，不作为普通可调配置开放。

### 服务设置

| 配置项 | 默认值 | 用途 |
| --- | --- | --- |
| `name` | `mcp-target-ops` | 向客户端公布的服务名 |
| `version` | 软件包版本 | 向客户端公布的版本 |
| `default_target` | 无 | 没有显式目标和活动目标时使用的回退目标 |
| `terminal_ring_buffer_bytes` | `524288` | 每个终端保留的输出大小 |
| `runtime_dir` | 系统临时目录下的 `mcp-target-ops` | 启动时创建的运行目录 |
| `http_bearer_token` | 无 | 保护 HTTP 路由的静态 Bearer Token |
| `public_base_url` | 无 | 用于 OAuth 元数据、导出链接与 MCP App widget 元数据的外部 HTTPS 源站地址 |
| `oauth_enabled` | `false` | 启用内置 OAuth 授权服务器 |
| `oauth_authorization_password` | 无 | 授权页的可选密码门禁 |
| `oauth_scopes` | `["mcp:tools"]` | OAuth 公布并校验的 scope |
| `oauth_allow_dynamic_client_registration` | `true` | 启用 `/oauth/register` |
| `oauth_authorization_code_ttl_secs` | `600` | 授权码有效期 |
| `oauth_access_token_ttl_secs` | `3600` | 访问令牌有效期 |
| `oauth_refresh_token_ttl_secs` | `2592000` | 刷新令牌有效期 |
| `oauth_state_file` | `~/.config/mcp-target-ops/oauth-state.json` | 持久化 OAuth 客户端和令牌 |

Target Ops 还会自动查找主配置文件同目录下的可选 `AGENTS.md`（例如 `~/.config/mcp-target-ops/AGENTS.md`），但**不会在 MCP initialize 阶段加载它**。当该文件存在且非空时，第一次真正会触达本机/SSH target 的工具调用会先被一个很短的 preflight 提示拦截；模型必须随后调用 `target_instructions`，该工具才会返回 `AGENTS.md` 的完整 UTF-8 内容，并把它标记为本 MCP 会话已加载，然后模型再重试原 target 操作。`target_list` 等纯 inventory 查询以及普通网页对话不会加载该文件。Target Ops 永远不会自动创建或修改此文件。

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

Target Ops 当前发布 41 个工具描述：39 个模型可见工具，以及 2 个仅供 App 使用的 `exec_stream` 与 `result_read`。

| 类别 | 工具 |
| --- | --- |
| 服务 | `server_info` |
| 目标 | `target_list`、`target_current`、`target_instructions`、`target_select`、`target_connect`、`target_disconnect` |
| 下游 MCP | `mcp_server_list`、`mcp_tools_list`、`mcp_tool_call` |
| 命令与任务 | `exec`、`exec_batch`、`exec_start`、`job_poll`、`job_output`、`job_wait`、`job_cancel`；仅 App：`exec_stream`、`result_read` |
| 文件与目录 | `file_read`、`file_backup`、`file_backup_list`、`file_restore`、`file_backup_delete`、`file_list`、`file_find`、`file_edit`、`file_write`、`file_delete`、`file_import`、`file_export`、`file_transfer`、`file_patch`、`file_move`、`file_chmod`、`directory_create` |
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

`exec`、`exec_batch` 与 `exec_start` 都绑定到稳定的 `ui://target-ops/exec-terminal/v1.html` MCP App。同步 `exec` 保持为短命令路径并正常等待最终结果；预计运行超过几秒，或实时输出有价值时，应优先使用 `exec_start`。它会立即返回 job id，因此 App 可以调用 `job_output` 获取 stdout/stderr 增量，不会再被同步父调用阻塞。每次响应返回后，App 等待 `runtime.app_job_poll_interval_ms`（默认 1 秒）再发出下一次请求，不会并发堆积轮询。宿主调度、网络延迟和命令自身的输出缓冲都可能让可见更新慢于这个间隔。ANSI SGR 颜色/样式会被安全渲染。

对于模型侧只需要“等后台命令结束后再继续”的工作流，应优先使用 `job_wait`，而不是反复调用 `job_poll` 或 `job_output`。其服务端默认等待窗口与最大等待窗口分别来自 `runtime.job_wait_default_timeout_ms` 和 `runtime.job_wait_max_timeout_ms`，单次调用可以请求更短的等待时间。

所有 Target Ops App 使用统一且可配置的结果卡生命周期。默认情况下成功卡 3 秒后折叠，失败/warning 卡 6 秒后折叠，30 秒后进入释放重 DOM 的 sleep。重新打开已有对话时，历史 App 结果走单独的冷启动路径：bridge 握手前只 hydrate 紧凑标题行，不再预先构建完整命令输出、代码行或 diff DOM；只有用户主动展开卡片时才恢复重内容。同时会识别并抑制宿主在初始化后对同一历史 `result_id` 的重复 replay，避免它再次触发“新结果自动展开”定时器。App 还会使用 `ResizeObserver` 通过 MCP Apps `ui/notifications/size-changed` 主动上报自身实际尺寸，使支持弹性高度的宿主在 bridge 就绪后立即把历史 iframe 收缩到折叠标题行。静态工具结果缓存在 `runtime_dir/results/` 下，不设置 TTL；`runtime.result_cache_max_bytes` 默认 100 MiB，超限时按会话 LRU 从最久未活动的完整会话开始淘汰，新事件会刷新该会话的活动时间。用户重新展开时由仅 App 可见的 `result_read` 使用不可预测的 `result_id` 恢复。长命令卡也只在用户展开后才从保留的 job output buffer 重建。当前不自动请求 iframe teardown，因此历史卡片仍可重新打开。

`exec` 与 `exec_start` 现在共用同一套 `CommandSession`，统一处理进程生命周期、
stdout/stderr 捕获、超时、取消与增量输出。`exec` 会等待会话结束，并在未显式设置
超时时继续使用目标策略的默认 timeout；`exec_start` 会立即返回任务 ID，未指定时不
设置运行时 timeout。当前 ChatGPT Host 会把 App 发起的工具调用排在正在执行的同步 `exec` 之后，因此实时 UI 输出明确通过 `exec_start` 实现，而不再依赖前台 `exec_stream` 轮询。子进程 stdin 与 MCP 控制流隔离。SSH 命令会话使用独立 OpenSSH
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
- `file_transfer` 用于在两个显式且不同的 target 之间复制一个普通文件。优先使用
  `rsync`，不可用时回退到 `scp`。SSH→SSH 时文件数据只在两台 target 之间直传：
  Target Ops 会尝试源端 push 与目标端 pull，但绝不会在插件 host 上落地中转文件。
  因此至少要有一台远端能够无交互地认证到另一台远端。

### 托管回滚快照

在修改配置、编辑文件、重启服务、迁移数据或执行其他有风险的操作前，如果需要临时
安全副本，应使用 `file_backup`。模型不应再通过 `cp` / `mv` 在原目录创建 `.bak`、
`.old`、时间戳副本等散装备份；`exec` 的工具描述也会明确把此类工作引导到
`file_backup`。

托管快照统一保存在 `server.runtime_dir/file-backups/`，不会污染源文件所在目录。
每份快照记录 target、原始路径、SHA-256、字节数、可获取时的 Unix 权限位、创建/过期
时间以及可选备注。`file_backup` 默认保留 24 小时，也可按调用请求其他 TTL，但不能
超过配置的最大值（默认 30 天）。Unix 下备份目录权限为 0700，payload 与元数据文件
权限为 0600。

即使调用者已经丢失原始工具结果或对话上下文，也可以通过 `file_backup_list` 重新找到
仍有效的快照。默认返回最新 100 条，可通过 `limit` 调整到最多 200 条，并返回
`total_matches` 与 `truncated`。`file_restore` 会先验证备份完整性，再恢复到原路径或显式指定的新路径；
如果目标文件已经存在，必须提供 `expected_current_sha256` 做 CAS 校验，或明确设置
`overwrite = true`。支持的平台会同时恢复原权限位。`file_backup_delete` 可提前删除单个
快照，但不会触碰源文件。

过期快照在备份相关操作中会立即视为不可用，并在服务启动时、后台清理线程中（默认每
60 秒）以及正常工具流量的节流清理中从磁盘删除；同时执行 `file_backup_store_max_bytes` 总容量预算和
`file_backup_max_entries` 条目数预算，任一超限时即使 TTL 尚未到期也会优先淘汰最旧快照。
因此不再需要人工整理分散在各项目目录中的备份文件。

### 终端

`terminal_open` 启动持久 PTY，并永久绑定到当时解析出的目标。`terminal_read` 支持
增量读取，`terminal_send` 写入输入，`terminal_resize` 调整真实 PTY 尺寸。终端
输出使用有界环形缓冲区，所有会话均为进程内状态。

### ChatGPT 文件集成与审阅界面

`file_read` 绑定 `ui://target-ops/file-read/v1.html`，以带行号的代码阅读界面展示文本、范围与哈希元数据，并以可展开的紧凑列表展示批量结果。相比通过 `exec` 调用 `cat`/`sed`，结构化读取更容易审阅。

`file_import` 接受经运行时重写后的 ChatGPT/连接器文件参数，可以是已挂载本地
路径或 HTTPS 文件引用，并通过普通原子/CAS 写入策略落盘；HTTPS 重定向被禁用。

> [!WARNING]
> `file_export` 可能触发前端强制审核/授权。用户不在客户端前时，授权容易超时，
> 从而中断正在进行的模型思考。除非确有必要，模型应避免在主要推理过程中调用
> `file_export`，优先等主体工作完成后再调用；对于昂贵的 Pro 级模型尤其如此。

`file_export` 默认使用 `delivery = "link"`。通过普通目标读取策略检查后，服务会把
文件暂存到 `runtime_dir/downloads/`，生成 256-bit 加密随机不透明 token，并返回
`/downloads/<token>` 下载 URL，而不是 MCP 文件附件。默认有效期为 10 分钟，
`link_ttl_secs` 最长可覆盖到 24 小时。链接默认可重复下载，避免浏览器或宿主预取
提前消耗；设置 `single_use = true` 后，第一次成功 GET 即会消费该 token。Unix 下
暂存目录权限为 0700，文件为 0600；过期 token 会被拒绝，服务每次启动都会清空旧
暂存导出。

link 模式要求配置 `server.public_base_url`，除 localhost 测试外必须使用 HTTPS。
应把该不透明下载 URL 视为短期敏感信息。显式 `delivery = "attachment"` 则保留原有
MCP embedded resource 与 ChatGPT 文件输出兼容路径；只有 attachment 模式会产生
embedded base64 resource 与 `file` 结果字段，link 模式只返回下载元数据。

导入和导出默认上限均由 `runtime.file_transfer_default_max_bytes` 控制（默认 25 MiB），
同时保留编译期 100 MiB 硬上限。

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
| `GET /downloads/<token>` | 由不透明 token 定位的临时 `file_export` 下载 |
| `POST /` 或 `POST /mcp` | MCP JSON-RPC；配置认证后受保护 |
| `DELETE /mcp` | 确认关闭 MCP session；配置认证后受保护 |
| `/.well-known/oauth-protected-resource` | 启用 OAuth 时的资源元数据 |
| `/.well-known/oauth-authorization-server` | 启用 OAuth 时的授权服务器元数据 |
| `GET|POST /oauth/authorize` | 授权页面，以及批准或拒绝请求 |
| `POST /oauth/token` | 授权码与刷新令牌交换 |
| `POST /oauth/register` | 启用时的动态客户端注册 |

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
  file_transfer.rs       target 间直连 rsync/scp 传输
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
