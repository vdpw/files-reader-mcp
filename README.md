# files-reader-mcp

Rust 编写的本地只读文本与 Git 阅读 MCP 服务，供支持 MCP 的 LLM 客户端调用。通过 Streamable HTTP 连接，默认监听 `127.0.0.1:3210/mcp`；拒绝非回环监听地址。

授权根目录可以是普通目录，下面包含多个独立仓库。文件操作使用 `root + path`；Git 操作使用 `root + repo` 选择子仓库，再用仓库相对路径定位文件。不会向授权根之外发现仓库。

## 构建与启动

支持 macOS / Linux（Unix 文件描述符边界），需要 Rust 1.88+。

```sh
cargo build --release --locked
cp config.example.toml config.toml
# 编辑 config.toml，选择需要授权的绝对路径。
./target/release/files-reader-mcp --config config.toml
```

`Ctrl-C` 停止前台服务。不提供文件写入工具或命令执行工具。运行时不依赖 Git 可执行程序；集成测试使用系统 Git 创建临时样本。

本地后台管理脚本 `service.zsh` 需要 zsh 和 Python 3，使用独立进程会话及 `nohup`，关闭终端后服务继续运行；不设置开机自启动。脚本及 `.run/` 运行文件已加入 `.gitignore`，不会随仓库克隆分发。脚本从自身目录定位 release 二进制和 `config.toml`，可在任意工作目录调用：

```sh
./service.zsh start     # 后台启动；已运行时不会重复启动
./service.zsh status    # 查看 PID、配置和日志路径
./service.zsh logs      # 查看日志，Ctrl-C 只退出日志查看
./service.zsh stop      # 发送 SIGINT，等待优雅退出
./service.zsh restart   # 重启并重新读取配置
./service.zsh build     # 更新代码后构建，再执行 restart
./service.zsh rebuild   # 构建 release，成功后重启；构建失败保留当前服务
```

PID 和本次启动日志保存在 `.run/service.pid`、`.run/service.log`；每次启动会覆盖上一次日志。启用 `token_env` 时，先在调用脚本的终端导出对应环境变量。`start` 仅在二进制不存在时自动构建，代码更新后可执行 `rebuild`，或依次执行 `build` 和 `restart`。

多个根目录的配置示例：

```toml
listen = "127.0.0.1:3210"
# 可选。启用后，环境变量必须存在且不为空：
# token_env = "FILES_READER_MCP_TOKEN"

[[roots]]
name = "projects"
path = "/absolute/path/to/projects"

[[roots]]
name = "notes"
path = "/absolute/path/to/notes"
```

例如 `projects/service-a/.git` 和 `projects/service-b/.git` 是两个独立仓库，则 `git_log` 分别使用 `{"root":"projects","repo":"service-a"}` 和 `{"root":"projects","repo":"service-b"}`。`projects` 本身不需要 `.git`。根目录本身是仓库时，`repo` 使用空字符串。

## MCP 客户端连接

先启动服务，再在支持 Streamable HTTP 的 LLM 客户端中配置服务地址：

```text
http://127.0.0.1:3210/mcp
```

客户端完成 MCP 初始化后，通过 `tools/list` 获取工具及参数定义，通过 `tools/call` 调用文件和 Git 阅读工具。

服务端启用 `token_env` 时，客户端需要在请求中发送 `Authorization: Bearer <token>`，其中 token 与服务端环境变量的值一致。具体配置方式由客户端决定。

请使用与 `listen` 完全相同的 IP 和端口访问。HTTP `Host` 必须匹配，存在的 `Origin` 必须为同源地址；例如绑定 `127.0.0.1` 时不接受 `localhost` 别名。`listen = "[::1]:3210"` 可使用 IPv6 回环。

## 工具

全部 18 个工具都标注为只读，并拒绝未知参数。无资源写入、提示执行、通用 shell 或任意 Git 参数入口。

| 工具 | 用途 |
| --- | --- |
| `roots` | 分页列出命名授权根目录 |
| `list_directory` | 有限深度浏览目录，默认深度 1 |
| `find_files` | 文件名子串、路径子串、glob 查找 |
| `search_text` | 字面量/正则文本搜索、路径过滤、上下文 |
| `read_file` | 全文、头部、尾部、行范围、长行分段续读 |
| `file_info` | 类型、字节大小、纳秒修改时间 |
| `git_status` | 暂存区/工作区状态及未跟踪文件 |
| `git_diff` | `worktree`、`staged`、`revisions` 三类差异 |
| `git_log` | 提交历史、作者、提交说明 |
| `git_show` | 单次提交说明和文本补丁 |
| `git_tree` | 指定版本的文件树、对象 ID 和模式 |
| `git_read_file` | 指定版本的文本文件及续读 |
| `git_search` | 指定版本的文本搜索 |
| `git_blame` | first-parent 行级追溯 |
| `git_search_history` | 按字面量/正则匹配新增、删除行 |
| `git_refs` | 本地已有分支、标签、远程跟踪引用 |
| `git_resolve` | 本地版本解析和 tag 剥离 |
| `git_merge_base` | 两个版本的全部最佳共同祖先 |

全部工具在 MCP `tools/list` 中提供 `inputSchema` 和 `outputSchema`，分别描述参数与成功结果。输出 Schema 包含字段类型、说明、必填项、可空字段及不同结果条目的结构（如搜索命中、未覆盖项和 Git 补丁）。成功调用通过 `structuredContent` 返回对应 JSON，同时在 `content[0].text` 保留相同 JSON 文本以兼容原有客户端；执行失败仍通过 `isError=true` 和文本错误返回，不适用成功结果的 Schema。

主要调用示例：

```json
{"name":"find_files","arguments":{"root":"projects","path":"service-a","name":"order","path_glob":"**/*.go"}}
```

```json
{"name":"search_text","arguments":{"root":"projects","path":"service-a","pattern":"CreateOrder|PENDING","regex":true,"path_glob":"**/*.go","context":2}}
```

```json
{"name":"read_file","arguments":{"root":"projects","path":"service-a/internal/server/http.go","start_line":50,"end_line":130,"limits":{"max_lines":80,"max_bytes":65536}}}
```

头部读取用 `start_line=1` 和 `end_line=N`；尾部用 `tail_lines=N`；省略范围即从头阅读全文，并按预算分页。`tail_lines` 与行范围互斥。

```json
{"name":"git_diff","arguments":{"root":"projects","repo":"service-a","scope":"revisions","from":"main","to":"HEAD","path_glob":"internal/**/*.go"}}
```

## 原文、预算与继续位置

- 支持严格 UTF-8（含 BOM）、带 BOM 的 UTF-16 LE/BE。解码不替换无效字符；BOM 通过 `bom` 字段标记，不混入首行。UTF-16 输出转为 Unicode 文本，保留换行和缩进。
- 文本以 `{"line":12,"text":"  original\r\n"}` 返回，JSON 中的转义由客户端解码后即为原文。不会给 `text` 加行号前缀、归一化 CRLF 或补齐末尾换行。
- 默认 200 行/条目、64 KiB；可用 `limits.max_lines` 和 `limits.max_bytes` 调整，硬上限 2,000 行、256 KiB，字节预算最小 2 KiB。字节数指工具结果中的 JSON 数据，不包括 MCP/HTTP 外层封装。上下文行计入行预算。
- 返回 `truncated=true` 时同时给出原因和 `next_cursor`。保持原查询参数，传入该游标继续。长行不跳过剩余文本：`read_file` 使用 `byte_offset/complete`，补丁/消息分页使用 `text_byte_offset/text_complete`。偏移是解码后 UTF-8 字节偏移。
- 搜索超长行仍报告命中路径和行号，并明确 `context_omitted=true`，随后用读取工具分段获取原文。
- 搜索出现 `not_searched` 时，该文件/目录没有被覆盖；`coverage.remaining_sources` 和继续位置表示后续尚未处理的来源。**只有读完全部页、且没有未覆盖项，才能在指定范围内认定没有命中。**
- 文件读取游标校验内容指纹；目录和工作区搜索游标校验发现结果的路径/大小/修改时间。变化时返回 `STALE_CURSOR`，重新查询。文件读取同时检查读取前后大小/修改时间。对正在并发修改的仓库，不承诺跨文件的原子快照；历史查询可使用完整 commit ID 固定版本。
- 一个文件或 Git 对象最多 8 MiB；更大文件明确拒绝，搜索记录未覆盖。目录深度最多 32、发现最多 20,000 项。目录扫描到预算边界时返回 gap，需缩小到该子目录继续发现。
- 文本搜索每次读取预算约 64 MiB，失败读取按 8 MiB 保守计费，预算耗尽后可续页。Git 查询限制对象读取/解压总量、pack index 大小、历史深度和结果条目数；到达限制报错或标注未检查，不能视为完整结果。

## Git 语义与首版限制

Git 后端直接只读解析 SHA-1 的 loose objects、pack v2/v3、pack index v2、松散/打包 refs、index v2/v3/v4。对对象 ID 和 index 校验和进行校验；读取用固定目录句柄逐级打开，拒绝符号链接。参照 [Git pack 格式](https://git-scm.com/docs/gitformat-pack) 和 [Git index 格式](https://git-scm.com/docs/gitformat-index)。

不读取 Git config、attributes、hooks、分页器配置，不创建 Git 子进程、不写 index 或临时文件、不加载 external diff/textconv/filter、不联网。缺失对象直接报 `MISSING_OBJECT`，不自动获取。

明确限制：

- 不支持 worktree，包括 `.git` 文件指向的 linked worktree；存在 `worktrees` 管理目录的主仓库也拒绝 Git 查询。submodule/separate git dir 同样不解引用。
- 不支持 bare 仓库、SHA-256 仓库、reftable、shallow、alternates、split/sparse index、未合并索引或 intent-to-add。上述结构不会退化成空结果；可检测到时明确报错。
- 版本表达式支持本地引用、完整/唯一缩写 SHA-1，以及 `~N`、`^N`。不支持 reflog、`rev:path`、任意参数或远端命令。
- `git_log` 按广度优先祖先顺序返回，最多遍历 10,000 个提交；blame 和变更历史搜索最多 1,000 个 first-parent 提交。不追踪重命名和拷贝。`git_search_history` 匹配变更行，不是 `git log -S` 的出现次数语义。
- `git_show` 的合并提交补丁相对第一父提交。补丁以结构化文件/hunk/行条目返回；每行有 `old_line`、`new_line`、`sign`、`text`，保留原文。
- 工作区比较使用原始字节，不处理 `.gitignore`、attributes、换行过滤；未跟踪列表包含被 Git 忽略的文件。仅模式变化也会报告；重命名表现为删除/新增。工作区 diff 与 Git 一样不包含未跟踪文件的补丁。
- Git 历史树中的符号链接、gitlink 只列模式，不读取内容。二进制、多媒体、SVG、PDF、Office 内容先被拒绝，再生成补丁；不会提供 Base64 回退。
- 不支持 GBK、无 BOM 的 UTF-16、非 UTF-8 路径和非 UTF-8 提交说明。不提供 PDF/Office 提取、语义搜索、语言解析、函数跳转或批量/多区间读取。

## 安全边界与验证

根目录在启动时规范化并固定为目录文件描述符。后续路径逐级使用 `openat(O_NOFOLLOW)`；拒绝 `..`、绝对路径、反斜杠、符号链接、设备、FIFO、socket。隐藏文件可见，`.git` 只能由 Git 工具读取。全部目录内容均视作不可信数据。

根目录由启动配置授权；本地其他进程在未配置 token 时也可访问服务。`token_env` 可启用 bearer 验证。请求体上限 64 KiB，工作任务最多并发 4 个；没有空闲工作槽时返回 `BUSY`。

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
python3 tests/smoke_http.py --binary target/release/files-reader-mcp
```

测试使用临时文件和临时 Git 仓库，包括普通目录下多个仓库、pack/delta、索引、符号链接/路径越界、worktree、恶意 Git 配置、二进制补丁、分段还原、HTTP 初始化与全部工具调用。实际校验结果见 `docs/VALIDATION.md`。
