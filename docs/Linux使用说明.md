# MemRW3 Linux 版（Ubuntu 22.04 及更新）

在 GitHub Actions 的 Ubuntu 22.04 上编译（glibc 2.35），x86_64。功能与 Windows 版相同，说明见 `开始使用.md` 和 `docs/`。

## 1. 运行

```bash
tar -xzf MemRW3-linux-x86_64.tar.gz
cd MemRW3-linux-x86_64
./MemRW3
```

桌面版 Ubuntu 一般已带运行库。缺库时安装：

```bash
sudo apt install libudev1 libusb-1.0-0 libxkbcommon0 libgl1 fonts-noto-cjk
```

`fonts-noto-cjk` 用于中文显示；界面出现方框时安装它。

## 2. 探针权限（必做一次）

Linux 默认不允许普通用户访问 USB 探针，直接运行会提示找不到探针或权限不足。按 probe-rs 官方说明安装 udev 规则：<https://probe.rs/docs/getting-started/probe-setup/>

1. 下载其中的 `69-probe-rs.rules`，复制到 `/etc/udev/rules.d/`。
2. 执行 `sudo udevadm control --reload && sudo udevadm trigger`。
3. 重新插拔探针。

如果规则按 `plugdev` 组授权，还要把自己加入该组（`sudo usermod -aG plugdev $USER`），然后注销再登录。J-Link 无需安装 SEGGER 驱动，probe-rs 直接通过 libusb 访问。

## 3. Claude Code / Codex 接入（MCP）

MemRW3 启动后会把会话文件写到 `$XDG_RUNTIME_DIR/MemRW3/ai-readonly/`（没有该变量时用 `~/.local/state/MemRW3/ai-readonly/`），MCP 脚本按同样的规则查找。

```bash
claude mcp add --scope user memrw3 -- python3 -u "$PWD/tools/memrw3_mcp.py"
```

工具列表和安全约束见 `docs/界面定制与调试助手.md` 第 5 节。

## 4. 本机设置位置

布局、列和侧栏偏好保存在 `$XDG_CONFIG_HOME/MemRW3/`（默认 `~/.config/MemRW3/`）。

## 5. 验证状态

CI 在 Ubuntu 22.04 上完成编译和全部单元测试。图形界面、探针连接和 MCP 未在真实 Ubuntu 桌面上验证，首次使用请按上面的步骤自行确认。
