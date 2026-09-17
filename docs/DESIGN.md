# 第一版接口与边界

- Rust / Unix（macOS、Linux），Streamable HTTP，默认 `127.0.0.1:3210/mcp`；只允许回环 IP。
- TOML 多个命名根目录；所有参数路径都是相对某个根的 UTF-8 路径；拒绝绝对路径、`..`、反斜杠与 `.git` 直接访问。
- 用启动时打开的根目录句柄和逐级 `openat(O_NOFOLLOW)` 打开文件；目录和普通文件之外的类型不读取。符号链接一律拒绝，包括目录内部链接。
- 不遵循 `.gitignore`，支持隐藏文本文件。`.git` 元数据仅由受限 Git 后端读取。
- UTF-8（含 BOM）、带 BOM 的 UTF-16 LE/BE，解码严格；保留文本本来的 CRLF/LF 和缩进，BOM 以元数据注明。每行用独立字段携带原始行号及原文，不向原文插入前缀。
- 默认每页 200 行或条目 / 64 KiB 结果 JSON（不计 MCP/HTTP 外层封装），硬上限 2,000 行 / 256 KiB 结果 JSON。单文件原始内容上限 8 MiB；超限报告不支持读取该文件，搜索记录为未覆盖。单行超过响应预算时返回同一行的 UTF-8 字节偏移，可继续读，绝不跳过尾部。
- 目录遍历深度最大 32；每次最多检查 20,000 个目录项。截断、未搜索文件、忽略原因显式返回。继续位置用于未变化的数据，携带内容或结果指纹；变化后明确要求重新开始。
- 固定 MCP 工具：roots、list_directory、find_files、search_text、read_file、file_info、git_status、git_diff、git_log、git_show、git_tree、git_read_file、git_search、git_blame、git_refs、git_resolve、git_merge_base、git_search_history。
- Git 直接读取普通 SHA-1 仓库的对象、pack、refs 和 index，不执行 Git 或其他进程，不解析配置、不加载 attributes/filter，不写任何文件，不访问网络。拒绝 `.git` 文件（linked worktree、submodule、separate git dir）、commondir、alternates、浅克隆、非 SHA-1 对象格式、split/sparse index；主仓库有 linked worktree 时 Git 查询也明确不支持。
- Git 工作区读取复用同一文件边界；历史树只读取普通 blob，拒绝符号链接和 gitlink；先通过文本检查再生成差异，二进制/SVG/PDF/Office 绝不通过补丁输出。
- Git 版本参数接受 HEAD、本地 refs、完整或唯一缩写对象 ID，以及 `~N`、`^N`；不接受任意命令、reflog 表达式或远程查询。refs/remotes 为已存在的本地元数据。
- blame 和变更历史搜索采用明确的 first-parent 语义，不跨重命名；diff 不检测重命名，按删除和新增报告。默认 merge 提交补丁相对第一父提交。
- HTTP 校验 Host 和 Origin；拒绝浏览器跨源请求；可选 bearer token。请求体 64 KiB，最多 4 个并发工作任务。授权根目录下的内容被视作不可信数据，而非模型指令。

增强项批量/多区间读取本版暂不提供；客户端可调用多次 read_file。语义搜索、语言解析和函数跳转不在范围内。
