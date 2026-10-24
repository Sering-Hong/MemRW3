# MemRW3 · Enhanced Pink

嵌入式 MCU 变量监控与调试工具，支持 J-Link、CMSIS-DAP、ST-Link 等探针，可同时查看实时曲线和类似 Keil Watch 的变量表。

本仓库基于 [SuperLiaohy/MemRW3](https://github.com/SuperLiaohy/MemRW3) 的 v2.2.1（`c47c29403af1e1295403b415115a93de41f84f57`）进行增强。当前版本恢复到 **2026-10-20 Enhanced Pink**：保留 Watch 变量表、完整采样历史和粉色图标。原有 Debug 调试器继续可用；源码修改、Keil 后台编译以及后来增加的编译/烧录/开始快捷键已撤回。

<p align="center"><img src="assets/MemRW3-pink.png" width="140" alt="MemRW3 粉色芯片波形图标"></p>

## 功能

| 功能 | 当前行为 |
|---|---|
| 实时曲线 | 多变量叠加、坐标缩放、游标、FFT；默认保留本次采集的全部历史 |
| Watch 变量表 | 名称、类型、地址、当前值、刷新频率、写入；结构体/数组展开，名称/地址搜索 |
| 同时查看 | 将 Table 通过 Pop out 弹出，主窗口继续显示 Chart |
| 变量读写 | 从 AXF/ELF 的 DWARF 信息解析变量；标量写入带类型与范围校验 |
| SVD 寄存器 | 可选寄存器面板、访问权限显示、读写与独立刷新率 |
| 固件烧录 | 连接目标后手动选择 ELF/AXF、HEX、BIN、UF2，校验后复位 |
| 原有 Debug | 源码/汇编、硬件断点、步进、调用栈、局部变量和 CPU 寄存器 |
| 数据与配置 | CSV 日志、JSON 配置保存/加载、断开后保留最后显示值 |
| 外观 | 粉色程序/窗口/桌面图标，浅色、深色和跟随系统主题 |

## 第一次使用

1. Windows 便携版解压后运行 `MemRW3.exe`。本地部署目录为 `D:\MemRW3`。
2. 在设置中选择正确 MCU、探针协议与速度，连接已上电的目标板。
3. 点击“打开变量树”，加载与板上固件对应且含 DWARF 调试信息的 `.axf` 或 `.elf`。
4. 将需要的标量添加到 Chart，将标量、结构体或数组添加到 Table。
5. 点击控制栏“开始”。要同时看表格和曲线，将 Table **Pop out** 后切回 Chart。
6. 点击“暂停”结束这段采集，用“查看全部”回看全过程。持久保存前先设置 CSV 文件。

开始与继续采集会覆盖所选的同一个 CSV 文件，请为每段需要保存的记录选择不同路径。曲线完整历史保存在内存中，关闭程序后不会自动恢复；保存配置也不保存采样点。

## 文档

- [详细使用说明](docs/使用说明.md)：连接、变量树、曲线、Watch、CSV、烧录、原有调试器与故障排查。
- [项目技术文档](docs/项目技术文档.md)：模块职责、采样数据流、完整历史策略、配置、约束和验证。
- [构建与打包](docs/构建与打包.md)：从源码构建、测试、Windows 图标和便携包制作。
- [版本记录](CHANGELOG.md)：当前版本范围及回退记录。
- [架构细节](ARCHITECTURE.md)与[上游使用说明](docs/上游使用说明.md)。

## 从源码构建

使用当前稳定版 Rust，保留 `Cargo.lock`。项目采用 Rust 2024 edition；`Cargo.toml` 中声明的 1.85 是项目最低声明值，锁定的依赖可能需要更高版本，不能据此保证 Rust 1.85 可编译。当前 Windows 实际验证工具链为 Rust 1.99.1 / GNU。

```bash
git clone https://github.com/RP27-EC/MemRW3.git
cd MemRW3
cargo build --release --locked
cargo test --release --locked
cargo run --release --locked
```

Windows 输出 `target/release/MemRW3.exe`；Linux/macOS 输出 `target/release/MemRW3`。系统依赖和 Windows 安装包制作见[构建文档](docs/构建与打包.md)。

## 验证与限制

完整历史的回归测试连续写入 120000 个点，检查起始与结束点都存在；绘制压缩测试覆盖首尾点、尖峰和时间顺序。Watch 包含搜索与无硬件 UI 测试。恢复后的测试结果见[版本记录](CHANGELOG.md)。

软件测试不能代替目标板验证。硬件变量读写、探针连接、烧录及长时实测需要在相应 MCU 上确认；实际采样速度受探针、总线、变量数量和目标状态影响。完整历史会持续占用内存，高采样率和多曲线场景应监控内存并保存 CSV。

## 来源与许可

保留原项目提交历史及作者信息。上游 README 标注为 MIT，参见[保留的上游说明](docs/上游使用说明.md#许可证)。本仓库的增强重点是变量表、完整采样历史与图标；原有探针、固件烧录及 Debug 核心来自上游。图标生成提示词保存在 `assets/icon-prompt.txt`。
