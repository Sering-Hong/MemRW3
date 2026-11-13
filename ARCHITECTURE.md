# MemRW3 架构

## 总览

MemRW3 是 Rust 2024 + eframe/egui 桌面程序。Chart、Table 和 Debug 都实现
`MemRWPlugin`，但插件只管理 UI 状态并产生 `PluginAction`；所有硬件访问统一由
`ProbeWorker` 串行执行。

```text
ChartPlugin ── Stream demand ─┐
TablePlugin ── Latest/SVD ────┼─> MemRW3App ── ProbeCommand ──> ProbeWorker
DebugPlugin ── DebugCommand ──┘                                  │
                                                                  ├─ ProbeSession
ChartPlugin <── FrameData/RingBuffer <────────────────────────────┤
TablePlugin <── LatestValue/RegisterData <────────────────────────┤
DebugPlugin <── DebugSnapshot <───────────────────────────────────┘
```

关键约束：

- 一个物理 Probe 只有一个所有者：`ProbeWorker`。
- FullSpeed/Halt 是 Worker 的调度策略，不是线程所有权迁移。
- Chart 的流数据与 Table 的最新值完全分离。
- 静态变量继续由项目原有 DWARF 模型负责；栈帧和局部变量由
  `probe-rs-debug` 负责。
- DebugPlugin 不重复实现 SVD、静态变量、连接、复位或烧录。

## 模块

```text
src/
├── main.rs
├── app.rs                     # UI shell、插件动作和 Worker 事件编排
├── dwarf/
│   ├── extract.rs             # 静态变量 ELF/DWARF 解析
│   └── types.rs               # 静态变量树和 Extend 类型
├── model/
│   ├── debug.rs               # 可跨线程的调试命令和快照 DTO
│   ├── register_io.rs         # SVD 寄存器请求/结果
│   ├── ring_buffer.rs         # Chart 有界无锁流缓冲
│   ├── state.rs               # AppSession
│   └── variable_pool.rs       # 变量池、读取类别和 LatestValue
├── probe/
│   ├── session.rs             # 唯一 probe-rs Session 的硬件原语
│   └── worker.rs              # 命令循环、双模式调度和 DebugEngine
├── svd/
└── ui/
    ├── plugin.rs              # MemRWPlugin、PluginAction、上下文
    ├── dock.rs
    ├── chart_plugin/
    ├── table_plugin/
    ├── debug_plugin/
    └── variable_tree_panel.rs # 共享 ELF 入口及静态变量树
```

## 插件边界

`MemRW3App` 持有：

```rust
plugins: Vec<Box<dyn MemRWPlugin>>
```

默认顺序是 Chart、Table、Debug。插件每帧接收只读上下文；所有副作用通过
`PluginAction` 返回。硬件类动作包括：

```text
WriteVariable
ReadRegisters / WriteRegister
Debug(DebugCommand)
RebuildSlots
```

App 将硬件动作转换成 `ProbeCommand` 放入 Worker 队列，UI 线程不等待 Probe。
Worker 通过 `ProbeEvent` 返回连接、烧录、寄存器和调试结果。

所有插件都通过 `MemRWPlugin::on_enabled_changed` 接收 Dock 启停变化。普通插件暂停时
只停止更新和交互；DebugPlugin 暂停时还会停止 DebugEngine 状态轮询并卸载硬件断点，
但不改变 MCU 当前运行/暂停状态。DebugPlugin 内的“暂停/继续”才对应 Core Halt/Run。

## 程序映像生命周期

`VariableTreePanel` 是 ELF 的共享加载入口，并维护：

```text
loaded_elf_path
program_generation
```

成功加载 ELF 后 generation 递增。App 将路径和 generation 发给 ProbeWorker；
Worker 在线程内部构建 `probe_rs_debug::DebugInfo`。`DebugInfo` 是 `!Send + !Sync`，
所以永远不会穿过线程边界。

同一 ELF 同时服务两个互不替代的模型：

- 原有 `dwarf::extract`：静态地址变量、结构体和数组，用于 Chart/Table。
- `probe-rs-debug`：源码行、源码断点、源码级步进、CFI unwind、栈帧和 locals。

更换 ELF 时，Worker 先卸载旧地址的硬件断点，再用逻辑断点重新解析和安装。
异步结果携带 `program_generation` 和 `revision`，App 不接受旧代结果。

## ProbeWorker

ProbeWorker 是普通阻塞线程，不依赖 async runtime。它持有：

```text
ProbeSession
DebugInfo
TargetState
逻辑/已安装断点表
StackFrame + VariableCache
Acquisition slots
```

循环调度顺序：

1. 每轮优先处理有限数量的控制命令，避免控制请求或采集任一方饥饿。
2. 全局采集已经“开始”且 DebugPlugin 显式启动后，约每 50 ms 查询 `Core::status()`；
   仅连接 Probe 时不启动 DebugEngine，也不安装硬件断点。全局停止会停止 DebugEngine。
3. Running/Sleeping 时根据用户采集意图执行高速采集。
4. Halted 时停止 Stream，并约每秒执行一次 Table Latest 读取。
5. 空闲时使用有超时的通道等待，不进行忙循环。
6. 暂停/空槽或读取失败时继续做链路健康检查；连续失败后断开并发事件。

连接、断开、Reset、烧录、slot 更新、Table/SVD 请求和 Debug 请求都在这个线程
执行。旧的 `ProbeCell + Sync + acq_thread` 已移除。

`ProbeSession` 在连接期间缓存唯一的 `Core` handle，采集热路径不得每轮调用
`Session::core(0)`。采集、Debug、断点、SVD 和变量写入都复用该 handle；只有烧录等
必须独占 `Session` 的操作、断开/重连以及采集读失败会先使缓存失效。

### 目标状态和采集状态

目标状态独立于采集状态：

```rust
enum TargetState {
    Disconnected,
    Unknown,
    Running,
    Sleeping,
    Halted { reason: String },
    LockedUp,
}
```

`AppSession.acquisition_requested` 表示用户希望采集；`AppSession.running` 表示当前
实际正在产生 Stream 数据。断点命中时 Worker 只将 `running` 置 false，保留
`acquisition_requested`；Continue 后自动恢复。手动停止、断开或烧录会同时取消
采集意图。

## FullSpeed 与 Halt 调度

| 功能 | MCU Running/Sleeping | MCU Halted |
|---|---|---|
| Chart | 高频 Stream | 停止并保留最后画面 |
| Table 左侧 | 同轮读取并更新 Latest | 约 1 Hz Latest |
| Table 写入 | 插入采集轮次之间 | 立即排队执行 |
| SVD | 原频率调度 | 原频率调度 |
| 断点增删 | 短暂 halted access 后恢复 | 直接执行 |
| CPU registers/stack/locals | 不读取 | Halt 快照及按需展开 |
| Flash | Worker 独占 | Worker 独占 |

### Stream 与 Latest

`VariableReadClass`：

```text
Stream  -> Chart
Latest  -> Table
```

`PooledVariable` 分别记录 `stream_readers`、`latest_readers`，同时保留总绑定数和
总活动数用于生命周期管理。

FullSpeed 下，同一物理地址仍只读取一次：Stream 绑定写入 RingBuffer，Latest
绑定写入原子 `LatestValue`。Halt 下只读取含 Latest 绑定的 slot，且绝不向
RingBuffer 写入，因此不会在 Chart、FFT 或 CSV 中制造调试暂停期间的低频数据。

## DebugEngine

底层执行控制直接使用 `probe-rs 0.31`：

- `Core::halt/run/step/status`
- CPU register read
- `available_breakpoint_units`
- `set_hw_breakpoint/clear_hw_breakpoint`
- MemoryInterface 代码和栈读取

高层调试使用 `probe-rs-debug 0.31`：

- `DebugInfo::get_source_location/get_breakpoint_location`
- `DebugRegisters::from_core`
- `DebugInfo::unwind`
- `VariableCache` 的局部变量懒加载

源码步进由 Worker 自己控制，不使用 `probe-rs-debug::SteppingMode` 的运行等待：

- Step Into 在 DWARF 规范化文件路径或行号首次变化时停止；列号变化不算源码级前进。
- Step Over 在源位置变化且 unwind 调用栈深度不大于起始深度时停止，因此会跨过被调
  函数。
- Step Out 在 unwind 调用栈深度小于起始深度时停止。
- Step Over 执行到与目标内存字节一致的直接调用指令时，临时恢复中断状态并使用硬件
  断点运行到返回地址；Step Out 优先运行到 unwind 得到的调用者地址。没有空闲硬件断点
  或无法可靠确定地址时自动回退到 `Core::step()`。
- Fault、数据 watchpoint、软件断点、外部暂停或非预期硬件断点会立即结束源码步进，
  不会继续执行下一条指令。
- 源码步进期间暂时卸载用户硬件断点，结束后恢复，避免目标指令同时命中用户 comparator
  干扰单步停止原因。

`ProbeWorkerHandle` 另持有不经过命令队列的原子步进中断标志。Debug 手动打断、停止、
断开、Reset 和退出按优先级写入请求；`single_step_source` 在每条硬件指令前后及加速运行
期间检查标志。Worker 恢复 PRIMASK、临时断点和用户断点后，再处理队列中的命令，
因此唯一 Session owner 约束不变，也不会从 UI 线程直接访问 Probe。

汇编由 Capstone 0.14 完成。支持 Thumb2、A32、A64、RV32 和 RV32C；Xtensa
当前返回不支持而不会导致 Worker panic。Cortex-M 的 Thumb 地址在比较 PC 和
断点时会清除 bit 0。

ELF 加载时同时缓存可执行代码段并生成离线反汇编，因此目标运行或尚未连接时也能
浏览汇编。Halt 快照优先使用匹配 PC 的 ELF 内容，只有 PC 不属于 ELF 代码段时才按
目标内存区域边界读取实时代码，避免不必要的 ARM 总线访问。

`probe-rs-debug 0.31` 会把 C++ 标记为未知语言，但仍能解析变量作用域、类型和
DWARF 位置。Worker 对 `DW_LANG_C_plus_plus*` 变量做第二阶段求值，补充基础整数、
浮点、布尔、字符、枚举、指针和位域值；结构体/数组继续使用其变量树懒加载。

### Halt 快照

Worker 仅在 `Running -> Halted`、Step 完成、切换栈帧或用户显式刷新时构建快照：

```text
DebugSnapshot
├── target_state / halt reason / PC
├── CPU registers
├── PC 附近汇编 + 源码定位
├── call frames
├── selected-frame locals
├── SP 附近原始栈内存
├── logical/resolved breakpoints
└── revision / program_generation / stop_id
```

局部变量不会进入 `VariablePool`。它们可能位于寄存器、CFA 相对内存、常量或
碎片位置，仅对当前 `stop_id` 有效。复合变量首次只展开有限深度，用户展开节点
时再发送 `ExpandVariable`。

可写局部变量只限 `VariableLocation::Address` 的标量。UI 发送的新值同时携带
`stop_id`、frame index 和 variable reference；Worker 拒绝过期请求，并通过
`Variable::update_value` 编码写入后刷新变量缓存。probe-rs-debug 将 C++ 标记为未知语言，
因此 Worker 写入 C++ 基础类型时临时按 C ABI 解析，完成后恢复原 DWARF 语言和类型元数据。

Debug 工作区底部拆分为局部变量和 CPU 寄存器两个可调整面板；右侧检查器只保留调用栈
和栈内存。中央编辑区支持源码、汇编以及二者并排模式。源码行内汇编通过现有
`Disassemble(Source)` 命令按需加载，UI 按源码路径+行号缓存结果，不扩大全局快照。

源码/汇编双向导航分别维护 source line 和 instruction address 滚动目标。新的 `stop_id`
将真实 PC 设置为汇编滚动目标；反汇编异步返回时再将源码请求解析到具体指令。滚动目标
只有在对应行实际渲染并居中后才清除，因此隐藏视图和 Split 模式不会丢失定位请求。

源码文件缓存为共享不可变行数组。源码行及已展开的行内汇编映射为固定高度虚拟行，只
渲染可见范围；DWARF 可执行行在 program generation 更新时构建路径／行号索引。
`DebugSnapshot.last_step_method` 记录最近一次实际采用的 `SingleStep` 或硬件断点加速，
并显示在 DebugBar 状态行；开始、继续、复位或更换程序时清空。

### 断点

配置保存 Debug 启动模式和逻辑断点：源码路径+行/列，或指令地址。连接和 ELF 更新时
只解析逻辑断点；DebugPlugin 启动后，以及已启动状态下的 Reset/烧录后才重新安装，
不保存 comparator 编号。

第一版只提供可移植的硬件指令断点。界面显示硬件容量、verified/unverified 和
具体错误。硬件槽用尽时不会伪装成安装成功。

“运行到光标”复用同一源码/地址解析器。若目标地址没有用户硬件断点，Worker
临时安装一个断点并运行；目标在该地址或其他原因暂停时立即清除临时断点，不将其
写入配置。

源码断点先按 DWARF 完整路径解析；若路径来自另一台构建机，则按规范化最长后缀
在工程源码清单中匹配。请求行不是可暂停行时，使用预建的 DWARF `is_stmt` 行索引
向后查找最多 32 行，并在 UI 中同时显示请求位置、实际解析位置和硬件地址。

## 静态变量与 SVD

原有职责保持不变：

- `dwarf::extract` 只接受可化简为绝对地址的静态变量位置。
- `VariablePool` 存储稳定地址和最多 8 字节的采样表示。
- Chart 负责流式历史、FFT 和 CSV。
- Table 负责静态变量树、写入和 CMSIS-SVD 外设寄存器。
- DebugPlugin 只显示 CPU core registers 和 frame locals，不再提供 globals/SVD。

## 配置和失效规则

配置文件包含 Probe 设置、ELF 路径、VariablePool 和每个插件 payload。Debug
payload 保存逻辑断点。加载配置前必须停止采集并断开目标。

以下事件会令 Halt 快照失效：Continue、停止 DebugPlugin、断开、Reset、烧录、更换
ELF。重新连接不会自动启动调试；用户选择 Attach 或 Reset 并启动后，Worker 才查询
硬件断点容量并安装逻辑断点。异步 UI 使用 request/revision/generation 防止过期结果
覆盖新状态。

## 当前限制

- 当前使用 core 0；模型保留了向多核扩展的空间，但尚未提供核心选择器。
- 硬件断点数量取决于目标 MCU；没有通用软件断点和数据 watchpoint。
- 优化构建中的变量可能被优化掉，源码级步进可能跨行。
- Cortex-M 源码级 Step Into/Over/Out 会在命令期间临时设置并回读验证 PRIMASK，防止
  普通可屏蔽中断把步进带入 ISR；NMI、Fault 和非 Cortex-M 架构仍可能影响落点。
- DebugPlugin 从 DWARF line program 构建工程源码树；构建机路径不可用时，可将
  DWARF 公共根目录映射到本机源码根目录。
- 没有物理 Probe 时只能执行纯逻辑和 UI 测试，硬件功能需在目标板上验证。

## Codex 只读数据出口

`ai_readonly::ReadService` 仅监听本机随机端口，认证后将四类只读查询送至有界队列并唤醒 egui。主线程在插件更新后处理查询，从 VariablePool 已发布值和插件只读视图生成响应。读取请求不产生 PluginAction 或 ProbeCommand，不新增硬件采样；受控写入分支见下节。

`MemRWPlugin::readonly_annotations` 提供 Table 名称/注释，`readonly_history` 提供 Chart 限量历史，默认均为空，Debug 不额外导出。ReadState 记录主机观察时间与发布序列；首见旧 LatestValue 不证明新鲜度；禁用 Table 后的新 stream 值不会被旧 LatestValue 覆盖。

Python 标准库桥接程序把 MCP stdio 工具调用转换为上述认证查询；发现信息位于当前用户应用数据目录，软件默认关闭出口。详细协议、上限和状态含义见 `docs/Codex只读接入.md`。

## Codex 受控写入

`WriteController` 管理临时人工白名单、上下限、可撤销 gate 与256条去重操作记录。GUI 请求写入仅提交 `ProbeCommand::GuardedAiWrite`，worker 使用 `ProbeSession::guarded_ai_write` 和纯逻辑 `guarded_io` 做写前比较、写入和读回；通过 `AiWriteFinished` 关联异步最终结果。

本地请求的单调时钟截止时间传入 worker；过期或权限撤销时不再发起写入。App Drop 在 worker shutdown 前撤销许可，Debug Reset 和程序/配置切换也撤销。原有手动 WriteValue/寄存器/调试命令保持原通路。

详情、MCP参数、结果状态与实机验收边界见 `docs/Codex参数写入.md`。

## 通用 PID 会话

`tools/pid_online.py` 在既有MCP读写接口上实现主机状态机和持久化基线，显式绑定六个变量，不直接访问探针。checkpoint/intent先落盘，再提交一项增益；后续轮询校验完整原生请求、授权代次和写入结果。rollback仅恢复本轮单项增益，拒绝外部改变或不确定结果。

`WriteController.permission_epoch` 在授权改变与撤销时更新；PID提交携带预期epoch，由原生端校验。会话同时绑定软件session_id和变量身份；不把主机顺序读取称为MCU同步帧。文件锁与原子替换防会话并发，数据存放于当前用户应用数据目录。

六项PID工具不运行后台控制任务。MCU周期边界原子更新、无扰切换及失联保护仍需专门固件接口，本轮未实现。完整说明见 `docs/PID在线调试方案.md`。

## 默认数值访问（2.2.0）

按最新需求删除人工白名单、范围授权、AI开关和写入设置窗口。main默认启动本机接口，WriteController默认开放所有类型/大小匹配的已加载数值变量；get_write_permissions返回类型界限而非机械安全界限。

上下文变化调用invalidate：关闭旧gate、增加代次、建立默认可用的新gate；旧任务不复活，后续任务无需再授权。App Drop调用shutdown并停止接口，确保退出时不生成新可用gate。MCP工具名及PID会话接口保持兼容。

完整当前行为及验证见 `docs/默认AI读写说明.md`；前述人工授权描述属于历史实现。

## 工作区与调参联动（2026-11-04）

`ui/workspace.rs` 保存两种布局的分隔比例、区域顺序与背景设置；新增顶层 JSON `workspace` 有默认值，旧配置无需迁移。背景 PNG 内置，只首次渲染时解码并缓存纹理。Dock 同时渲染三个插件，独立窗口与最大化仅影响布局，不改变 MCU 状态。

变量表通过 `PluginAction::AddToChart` 提交变量 ID、短名、中文注释，应用转交图表插件；整个批次预校验后复用已有变量池，新增 Stream 绑定，Watch 的 Latest 绑定保留。重复添加仅恢复可见并定位，移除曲线仅释放 Stream。注释通过只读 annotations 同步，不改用户曲线名称。

`ui/debug_tools.rs` 提供格式化、区间统计和写入记录。A/B 统计读取原始历史，打开时最多每 250 ms 计算一次；CSV 从导出瞬间的历史快照后台写文件，不插值、不修改采集缓存。手工/AI 写入日志由应用提交路径及 Worker 完成事件更新，明确区分成功与回读验证；AI 真实预读/回读原始位在可用时追加。记录仅覆盖变量表和 AI 变量写入，不覆盖寄存器、调试器局部变量或外部程序操作。

## 外观自动记忆（2026-11-04）

`AppearanceStore` 单独持久化背景显示强度和面板不透明度，Windows 路径为 `%LOCALAPPDATA%/MemRW3/appearance.json`。启动先读取并限制到合法范围；已有本机偏好优先于后来加载的调参 JSON，布局等项目字段不受影响。首次没有本机偏好时可从调参配置沿用外观。每次调整后 400 ms 防抖写入，静止界面会请求一次必要重绘；正常 Drop 补存。写入使用同目录临时文件、sync_all 与 rename 替换，失败保留旧文件并显示原因，不依赖新包或 eframe persistence 特性。

## 界面偏好、布局与调试助手（2026-11-12）

`ui/prefs.rs` 是进程内唯一的本机界面偏好存储（`%LOCALAPPDATA%/MemRW3/ui-prefs.json`）：布局、变量表隐藏列与换行、侧栏开关和宽度；修改后 400 ms 防抖并原子替换写入，退出补存，单元测试中不落盘。调参 JSON 仍可携带同样的设置，JSON 中存在的项优先，缺失的项沿用本机偏好。

`WorkspaceSettings` 新增 `hidden` 与 `arrangement`（均有默认值）；`Dock` 只为可见且未弹出的面板分配区域，面板标题拖动只交换 `order` 中的位置，不改变插件状态或 MCU 状态。

`ui/side_panel.rs` 由 App 直接持有，不是插件：只读取变量池、插件注释、`DebugSnapshot` 和 `RegisterData`，副作用只有 `AddToChart`、`OpenVariableTree` 和 `ReadRegisters` 三种既有 `PluginAction`。内存查看用带标记的请求 ID 复用寄存器读取通路，App 每帧清理过期代次的结果。`ui/expr.rs` 是表达式的纯逻辑解析与求值。
