<div align="center">

<img src="docs/images/icon.png" width="96" alt="GrandHonor 图标">

# GrandHonor

**为荣耀 MagicBook 笔记本打造的轻量、无广告控制中心。**<br>
Fn 键、电池养护、风扇与传感器、屏幕色彩与驱动 —— 无需荣耀电脑管家。

[![最新版本](https://img.shields.io/github/v/release/TheToxicFactory/GrandHonor?style=flat-square&color=0A84FF&label=release)](https://github.com/TheToxicFactory/GrandHonor/releases/latest)
[![下载量](https://img.shields.io/github/downloads/TheToxicFactory/GrandHonor/total?style=flat-square&color=30D158)](https://github.com/TheToxicFactory/GrandHonor/releases)
[![Windows 11](https://img.shields.io/badge/Windows%2011-x64-0078D4?style=flat-square&logo=windows11&logoColor=white)](#系统要求)
[![Rust](https://img.shields.io/badge/made%20with-Rust-B7410E?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![MIT 许可证](https://img.shields.io/github/license/TheToxicFactory/GrandHonor?style=flat-square&color=8E8E93)](LICENSE)

[English](README.md) · [Русский](README.ru.md) · **简体中文**

### [⬇ 下载 GrandHonor](https://github.com/TheToxicFactory/GrandHonor/releases/latest)

<img src="docs/images/overview.png" width="760" alt="GrandHonor 面板：电源、显示、键盘、触控板与系统">

</div>

## 为什么选择 GrandHonor

荣耀电脑管家会显示广告、运行沉重且时常出错，但卸载后 Fn 键、触控板手势和电池充电上限都会失效。GrandHonor 安静地完成同样的工作，并补上原厂软件缺少的功能。

- **轻量。** 小巧的托盘程序，空闲时不占用 GPU；只在您查看时才读取传感器。
- **隐私。** 无广告、无账户、无遥测。仅在检查更新或驱动时联网。
- **完整。** 可以卸载所有荣耀软件：GrandHonor 取代电脑管家和 HONOR OSD。请保留荣耀驱动程序。
- **安全。** 驱动与更新在运行前均经过校验（签名与 SHA-256）；更新失败会自动回退。

## 功能

| | |
|---|---|
| ⚡ **电源** | 实时 CPU 与电池温度、风扇、功率和充电上限 · Windows 电源模式与荣耀高性能模式（`Fn+P`）· 接通电源与电池预设 · **安静夜间** · 游戏与大型应用的**应用配置** |
| 🔋 **电池** | 充电上限（居家 40–70 %、办公 70–90 %、出行）· **智能充电** · **单次充满 100 %** · 电池健康度及其变化 · 到达上限、剩余 20 %、过热和睡眠耗电提示 |
| 🖥 **显示** | 60 / 120 Hz · 色彩配置 **鲜艳、sRGB、P3** · 带色温滑块的**护眼**、电子书与舒适模式 · **护眼休息**提醒 |
| ⌨️ **键盘** | 背光亮度、自动关闭与计划 · 自定义 **Copilot** 键与 **YOYO** 键功能 |
| 🖱 **触控板** | 触觉反馈强度 · 边缘滑动调节亮度与音量 |
| 🧰 **驱动** | 荣耀驱动与 BIOS 更新 · 驱动备份 · **设备检查**：修复异常设备、回退驱动、SSD 健康、重启蓝牙或 Wi-Fi、系统报告 |
| 🎛 **日常** | 所有按键的屏幕提示 · `F7` 麦克风（带指示灯）· `F8` 摄像头 · `F12` 截图 · 托盘右键菜单 · 石墨或玻璃风格 · English、Русский、中文 |

## 截图

<table>
  <tr>
    <td><img src="docs/images/power.png" alt="电源"></td>
    <td><img src="docs/images/display.png" alt="显示"></td>
    <td><img src="docs/images/keyboard.png" alt="键盘"></td>
  </tr>
  <tr>
    <td><img src="docs/images/glass-power.png" alt="玻璃风格"></td>
    <td><img src="docs/images/touchpad.png" alt="触控板"></td>
    <td><img src="docs/images/system.png" alt="系统"></td>
  </tr>
</table>

## 安装

1. 从[最新版本](https://github.com/TheToxicFactory/GrandHonor/releases/latest)下载 **GrandHonor-Setup-x64.exe**。
2. 运行并确认管理员权限提示 —— GrandHonor 需要与笔记本固件通信。
3. 保持勾选**开机启动 GrandHonor** 并完成安装。时钟旁会出现 **H** 图标。

> [!TIP]
> 不想安装？解压 **GrandHonor-win-x64.zip** 并运行 `GrandHonor.exe`，功能相同，但不会开机自启。

> [!NOTE]
> 安装程序尚未进行代码签名，SmartScreen 可能会提示：选择**更多信息 → 仍要运行**。

### 系统要求

- 带有荣耀固件接口的 **荣耀 MagicBook**。已在 MagicBook ZQC-P（Intel Core Ultra X7 358H，BIOS 1.12）上测试。
- **Windows 11** x64。无需其他任何组件或荣耀软件。

## 使用

- **单击托盘图标**打开面板；单击其他位置或按 `Esc` 关闭。
- **右键单击**打开快捷菜单：电源模式、护眼、麦克风、退出。
- Fn 键、手势和充电上限需要 GrandHonor 保持运行。关闭面板不会退出；**系统 → 退出**才会完全关闭。

| 按键 | 功能 |
|---|---|
| `Fn+P` | 节能 → 平衡 → 高性能 |
| `F7` | 麦克风开关（所有麦克风，带按键指示灯） |
| `F8` | 摄像头开关 |
| `F12` | 截图 |
| 刷新率键 | 60 ↔ 120 Hz |
| Copilot、YOYO（`F11`） | 在**键盘 → 按键**中自选 |

**更新：**系统 → 检查更新会从 GitHub 安装新版本；如有失败会恢复原版本。**卸载：**设置 → 应用 → 已安装的应用 → GrandHonor。

## 帮助

📖 [使用指南（英文）](docs/guide.md) · ❓ [常见问题（英文）](docs/faq.md) · 🐞 [报告问题](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose) · 🔒 [安全](SECURITY.md)

报告问题时，请附上**驱动 → 设备检查 → 系统报告**：它会复制型号、BIOS、Windows 版本和事件日志。

## 许可证

GrandHonor 是基于 [MIT 许可证](LICENSE)的免费开源软件。内置 Inter 字体采用 SIL Open Font License。<br>
GrandHonor 是独立项目，与荣耀终端有限公司无关，也未获其认可。HONOR 与 MagicBook 是其所有者的商标。
