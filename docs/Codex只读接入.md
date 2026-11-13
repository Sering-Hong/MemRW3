# Codex 读取 MemRW3

当前使用2.2.0默认读写版，无需下文历史版本的开关/授权步骤；见 [默认AI读写说明](默认AI读写说明.md)。

本页记录前一只读阶段；当前受控读写版的交付路径、授权方式与验证见 [Codex参数写入](Codex参数写入.md)。

## 已交付

新增五个只读 MCP 工具：`list_sessions`、`get_status`、`list_variables`、`read_variables`、`get_history`。本机已通过 `codex mcp add memrw3` 注册，使用现有 Python 3.14 标准库，不安装任何 Python 包或新 Rust 依赖。

独立程序仍在 `dist/MemRW3-ui-enhanced/MemRW3.exe`。顶部新增 **AI只读** 开关，默认关闭，不写入配置。也可运行 `MemRW3.exe --ai-readonly` 或双击发布目录的 `启动AI只读版.cmd`，仅启用本地查询服务，不自动连接探针或开始采集。

原安装版没有此接口，需要使用本次生成的新版。本轮保留参数注释分列、列宽与图表优化，没有二次元装饰。

## 使用步骤

1. 启动新版，在软件内加载原调参 JSON；确认顶部 **AI只读** 已开启。
2. 重启 Codex，使新增的 `memrw3` MCP 服务加载。当前已打开的聊天不保证动态获得新工具。
3. 对 Codex 说：“使用 memrw3 列出包含上膛或 speed 的变量，报告连接与采集状态。”
4. 需要真实运行值时，由人工按原流程连接目标、开启对应变量并开始采集。然后说：“读取这些变量的最新值，先检查是否过期，再分析目标速度与实际速度。”
5. 对已有 Chart 曲线可说：“读取该变量最近五秒历史，分析是否振荡。”

未连接探针也能列出已加载变量、类型、地址和中文注释；没有采样时返回 `has_sample=false`、`value=null`，不会编造零值。接口只读已有数据，不会自动添加变量、开启采集、连接目标或触发新的 MCU 访问。

多实例时先调用 `list_sessions`，再给其他工具指定 `session_id`，不自动猜测目标实例。关闭“AI只读”或正常退出软件会停止服务并移除本会话记录；崩溃可能留下失效记录，该记录的查询会报连接错误，需要区分有效会话。

## 当前聊天的直接读取方式

即使当前聊天尚未加载原生 MCP 工具，Codex 仍可通过终端执行发布目录中的桥接脚本，无需截屏识别：

```powershell
python tools/memrw3_mcp.py --call get_status
python tools/memrw3_mcp.py --call list_variables --arguments '{"query":"speed","limit":20}'
python tools/memrw3_mcp.py --call read_variables --arguments '{"ids":[0,1]}'
python tools/memrw3_mcp.py --call get_history --arguments '{"variable_id":0,"seconds":5,"limit":1000}'
```

在发布目录执行上述命令；变量 ID 必须先从 `list_variables` 取得，示例 ID 不表示特定电机或参数。多个实例时在 JSON 参数中附加 `session_id`。

本机注册命令使用 Python 和发布脚本的绝对路径；软件移动目录后需重新注册：

```powershell
codex mcp add memrw3 -- C:\Python314\python.exe -u "<新版目录>\tools\memrw3_mcp.py"
codex mcp get memrw3 --json
```

不需要再向 AI 提供变量地址或让另一个进程连接 J-Link。Codex 的 stdio 服务配置方式参考 [OpenAI 官方说明](https://learn.chatgpt.com/docs/extend/mcp)。本接口不调用 OpenAI API，不需要额外 API Key；数据交给 AI 后的处理范围取决于使用的 Codex 环境。

## 数据含义和限制

- `value` 按原类型解码；64 位整数同时返回十进制 `value_text` 和小端 `raw_hex`，避免用图表浮点显示代替精确值。非有限浮点返回空值，可结合 `raw_hex` 判断。
- `observed_at_ms` 是主机 UI 观察时间，不是精确 MCU 采样时间。第一次遇到既有 Table 缓存时无法确认采样年龄，时间为空、`stale=true`；后续看到新发布序列才更新时间。
- 断开、暂停采集、烧录中、观察年龄未知或超过 1 秒时标为 `stale=true`。这表示不应当作实时值，并不删除最后已知数据。
- `sample_time_s` / 历史 `time_s` 是采集计时器相对秒数，不是 UTC。历史读取来自 Chart 原始历史，不使用屏幕压缩曲线；单次最多10000点，超出时保留最近部分并返回 `truncated=true`。未加入 Chart 的变量没有曲线历史。
- 历史响应附带连接、采集及 Chart 暂停状态；历史点存在不代表目标仍在运行。软件和模型均不提供硬实时保证。
- 只读接口没有参数写入、任意内存读取、探针连接、MCU暂停/复位或烧录工具。原 GUI 的手动功能保持不变。

## 实现与验证

Rust `src/ai_readonly.rs` 负责当前用户目录会话发现、127.0.0.1 随机端口、会话认证、限长请求和有界 GUI 查询队列；Python `tools/memrw3_mcp.py` 负责 MCP stdio 和直接命令行读取。请求在 GUI 主线程访问已有数据；后台网络线程不持有探针。会话凭据保存在本机用户应用数据目录，不写进工程、MCP配置或日志。

`MemRWPlugin` 新增只读注释/历史查询方法；Table 导出中文注释，Chart 导出限量历史；`app.rs` 在已有数据更新后处理查询并标记状态；`main.rs` 处理启用参数，控制栏提供开关。未修改固件、硬件读取策略或采集频率。

实际检查：

- Rust Release 单元测试：129 通过、0 失败、4 跳过，包含原 UI/CSV/FFT 回归及新增只读测试。
- 三份实际镖架配置：各132变量的配置往返与 AI 变量导出通过，中文注释非空、未采样值不伪造，未改写原 JSON。
- Python 标准库测试：7 通过，覆盖 MCP 子进程握手、工具清单、未知写入拒绝、通知、参数、认证传输和多实例。
- 静态独立审查发现并修复：旧 LatestValue 遮盖新 stream 数据、慢速请求的读取截止及退出处理；补充对应回归测试。
- 格式检查、锁定依赖 Release 构建通过；沿用 `docs/UI优化验证记录.md` 中本机 GNU 链接方式，未升级依赖。正式构建仍有23条既有兼容性/未使用代码告警，未屏蔽。
- Codex CLI 0.160.0 已注册并确认启用 `memrw3`。真实新版 GUI → 认证本地接口 → MCP stdio 握手/工具查询通过，返回未连接、未采集、0变量；未连接探针。
- 原生 MCP 工具未热加载进当前聊天，已确认当前 Codex 可通过脚本读到真实 GUI。重启 Codex 后的原生工具调用及带硬件采样的读取仍需人工验收。

最终输出证据位于发布目录 `verification/ai-readonly-*`。本轮没有 MCU 写入、暂停、复位或烧录；只执行上位机无硬件检查。构建与测试使用本机现有工具链，不增加外部 SDK。

发布 EXE SHA256：`88d971919e14d00e511610291c2e145d1548d3825888ccff631fffa698b5d368`；字节数 62461345。
