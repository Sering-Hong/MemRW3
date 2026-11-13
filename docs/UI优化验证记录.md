# UI 优化验证记录

日期：2026-11-03。此处记录 UI 优化阶段的检查；后续 Codex 只读接入版本的129项测试及新增功能验证见 `Codex只读接入.md`。目标为当前目录的 MemRW3 源码；未连接任何探针或 MCU。最终版本按用户要求移除了二次元装饰和相关开关，保留粉色主题及表格/图表优化。

## 实际结果

| 检查 | 结果与边界 |
| --- | --- |
| Rust 语法与类型 | 首次 `cargo check --locked` 通过 |
| 格式 | 最终 `cargo fmt --all -- --check` 通过 |
| Release 单元测试 | 121 通过、0 失败、4 跳过，耗时 0.10 秒；4 项包含原有 3 项与可选本地配置测试 |
| 真实配置兼容 | 单独显式运行可选测试，1 通过；三份镖架 JSON 各 132 个变量，全部插件加载/保存往返成功，变量名称、地址、类型、大小不变；未写回原配置 |
| 注释编辑 | 无窗口 egui 输入测试：Enter 确认、Escape 取消，均无 pending_writes；旧显示名迁移、表达式分隔符、空注释及列宽往返测试通过 |
| 长曲线显示 | 3 条 × 120000 点，峰值 999 保留；清空、追加、限长缓存失效测试通过 |
| 缓存耗时 | 合成数据生成及首次压缩约 19.176 ms；100 轮三曲线缓存读取共 0.1206 ms |
| 图表 UI 逻辑 | 无窗口 egui 布局 30 帧，共 7.4963 ms；固定手动范围保持，原始每条 120000 点未减少 |
| CSV / FFT | 既有无硬件回归测试包含 CSV 时间戳合并与 FFT 纯逻辑检查，随单元测试通过 |
| 独立程序 | Release EXE 已生成，复制至独立 dist 目录；未覆盖安装版 |
| 本机图形初始化 | 对 dist 中的最终 EXE 运行 `--check-install`，退出码 0，报告“OK: 系统运行库与图形初始化检查通过。”；该入口不创建硬件采集会话 |
| DLL 导入 | objdump 检查最终 EXE 导入项均为 Windows 系统 DLL，无额外 MinGW DLL 依赖 |
| 独立静态审查 | 一次只读审查发现 2 项 P2：图例菜单点击事件、手动视图下自动轴选择；均已修正，随后重跑测试和构建 |

以上耗时是本机一次 Release 合成测试的测量，不含 GPU 出图和用户实际拖动，不代表实机采样性能或可承诺的帧率。

## 人工验收状态

**尚未完成实际窗口人工验收。** 桌面控制工具初始化报错：`failed to write kernel assets: 系统找不到指定的路径。 (os error 3)`，因此未将真实窗口交互标为通过。

建议在不连接探针的情况下打开独立版并加载旧配置：确认名称/注释对应；拖动及双击各列分隔线；窄窗口横向滚动；Pop out 后独立调整列宽并保存/加载；注释点击别处提交；图例右键打开属性。带真实采样的暂停继续、拖拽缩放、CSV/FFT 和长时间刷新体验仍需人工验收。

## 构建环境与兼容处理

- 按本次授权准备 Rust stable 1.99.1 GNU（含 rustfmt），未更改全局 PATH。
- 使用 `Cargo.lock` 的锁定依赖，未升级依赖、添加 crate 或改写 Cargo 构建配置。
- 默认 `cargo test --release --locked` 在最终链接失败：本机 GNU ld 2.30 无法识别新版 Rust 部分 `.drectve` 指令。
- 改用 Rust 自带 ld 2.44 时，旧 GCC 驱动带入旧运行库，发生 `__imp_atan` 重复定义。
- 最终使用 Rust 自带的 GNU 链接驱动，单独补充现有 MinGW 的 `libshlwapi.a` 导入库搜索目录，测试目标与正式 EXE 均成功链接。没有升级或替换原 MinGW。
- 最终命令使用 `cargo rustc --release --locked --tests` 构建测试目标，再显式执行测试 EXE；不把原来的失败 `cargo test` 命令记为通过。
- 配置复测时曾按修改时间误选正式 EXE；已关闭本次启动实例，改按 `test-bin-MemRW3` 构建标记定位并重跑，退出码 0；未连接探针。
- 最终正式构建保留 23 条告警，主要为现有浮点字面量的未来兼容提示和未使用函数；未屏蔽告警。
- 按用户规范跳过探针连接、MCU 写入、暂停/复位和烧录；本轮只执行上位机无硬件测试。

## 复现命令（Windows GNU）

在源码根目录运行，前提是 Cargo、rustfmt 和本机原有 MinGW 的 gcc 已可从当前进程 PATH 找到：

```powershell
$taskRustRoot = (& rustc --print sysroot).Trim()
$taskLinkRoot = Join-Path $taskRustRoot 'lib/rustlib/x86_64-pc-windows-gnu/bin/self-contained'
$taskLinker = Join-Path $taskLinkRoot 'x86_64-w64-mingw32-gcc.exe'
$taskImport = (& gcc -print-file-name=libshlwapi.a).Trim()
New-Item -ItemType Directory -Path target/ui-link-libs -Force | Out-Null
Copy-Item -LiteralPath $taskImport -Destination target/ui-link-libs/libshlwapi.a
cargo fmt --all -- --check
cargo rustc --release --locked --tests -- -C "linker=$taskLinker" -L native=target/ui-link-libs
$taskMarker = Get-ChildItem target/release/.fingerprint/MemRW3-*/test-bin-MemRW3 |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
$taskTest = Join-Path (Get-Location) ('target/release/deps/' + $taskMarker.Directory.Name + '.exe')
$taskTestRun = Start-Process -FilePath $taskTest -ArgumentList '--nocapture' `
    -RedirectStandardOutput target/test.log -RedirectStandardError target/test-stderr.log `
    -WindowStyle Hidden -PassThru -Wait
$taskTestRun.ExitCode
cargo rustc --release --locked --bin MemRW3 -- -C "linker=$taskLinker" -L native=target/ui-link-libs
```

本地三份配置测试需另设 `MEMRW3_CONFIG_DIR` 为其所在目录，显式运行测试 EXE 参数 `local_three_configs_roundtrip_without_changing_variable_identity --ignored --nocapture`。测试只读 JSON，不连接硬件，不要求将配置放进源码仓库。

## 产物追溯

- 程序字节数：62461345
- EXE SHA256：`88d971919e14d00e511610291c2e145d1548d3825888ccff631fffa698b5d368`
- Cargo.lock SHA256：`ce337d24e5955b3bffcbb74599cfc8347c804e5642ad6e05eab93093ee608636`
- 本次源码、测试和使用说明位于同一源码目录；原始构建及测试输出在 `target/ui-enhanced-*.log`；发布文件夹 `verification/` 保留最终输出副本。
