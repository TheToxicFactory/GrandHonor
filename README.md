<div align="center">

<img src="docs/images/icon.png" width="96" alt="GrandHonor icon">

# GrandHonor

**A light, ad-free control center for HONOR MagicBook laptops.**<br>
Fn keys, battery care, fans and sensors, display colour and drivers — without HONOR PC Manager.

[![Latest release](https://img.shields.io/github/v/release/TheToxicFactory/GrandHonor?style=flat-square&color=0A84FF&label=release)](https://github.com/TheToxicFactory/GrandHonor/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/TheToxicFactory/GrandHonor/total?style=flat-square&color=30D158)](https://github.com/TheToxicFactory/GrandHonor/releases)
[![Windows 11](https://img.shields.io/badge/Windows%2011-x64-0078D4?style=flat-square&logo=windows11&logoColor=white)](#requirements)
[![Made with Rust](https://img.shields.io/badge/made%20with-Rust-B7410E?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![MIT License](https://img.shields.io/github/license/TheToxicFactory/GrandHonor?style=flat-square&color=8E8E93)](LICENSE)

**English** · [Русский](README.ru.md) · [简体中文](README.zh.md)

### [⬇ Download GrandHonor](https://github.com/TheToxicFactory/GrandHonor/releases/latest)

<img src="docs/images/overview.png" width="760" alt="GrandHonor panel: Power, Display, Keyboard, Touchpad and System">

</div>

## Why GrandHonor

HONOR PC Manager shows ads, feels heavy and often misbehaves — yet without it the Fn keys, touchpad gestures and battery charge limits stop working. GrandHonor does the same job quietly and adds what the stock app is missing.

- **Light.** A small tray app with no GPU use while idle. Sensors are read only while you look at them.
- **Private.** No ads, no accounts, no telemetry. It goes online only when you check for updates or drivers.
- **Complete.** Every HONOR app can be uninstalled: GrandHonor replaces PC Manager and HONOR OSD. Keep the HONOR drivers.
- **Safe.** Drivers and updates are verified (signature and SHA-256) before anything runs; a failed update rolls back by itself.

## Features

| | |
|---|---|
| ⚡ **Power** | Live CPU and battery temperature, fans, watts and charge limit · Windows power modes and HONOR Performance (`Fn+P`) · presets for charger and battery · **Quiet nights** · **App profiles** for games and heavy apps |
| 🔋 **Battery** | Charge limits (Home 40–70 %, Office 70–90 %, Travel) · **Smart charging** · **Charge to 100 % once** · battery health and its change over time · notices at the limit, at 20 %, when hot and after sleep |
| 🖥 **Display** | 60 / 120 Hz · colour profiles **Vivid, sRGB, P3** · **Eye comfort** with a warmth slider, E-book and Soft modes · **Eye-break** reminders |
| ⌨️ **Keyboard** | Backlight level, auto-off and schedule · your own action for the **Copilot** and **YOYO** keys |
| 🖱 **Touchpad** | Haptic strength · edge swipes for brightness and volume |
| 🧰 **Drivers** | HONOR driver and BIOS updates · driver backup · **Device check**: fix devices with errors, roll back a driver, SSD health, restart Bluetooth or Wi-Fi, system report |
| 🎛 **Everyday** | On-screen notices for every key · `F7` microphone with LED · `F8` camera · `F12` screen snip · right-click tray menu · English, Русский, 中文 |

## Screenshots

<table>
  <tr>
    <td><img src="docs/images/power.png" alt="Power tab"></td>
    <td><img src="docs/images/display.png" alt="Display tab"></td>
    <td><img src="docs/images/keyboard.png" alt="Keyboard tab"></td>
  </tr>
  <tr>
    <td align="center"><sub>Power and sensors</sub></td>
    <td align="center"><sub>Colour and eye comfort</sub></td>
    <td align="center"><sub>Backlight and keys</sub></td>
  </tr>
  <tr>
    <td><img src="docs/images/drivers.png" alt="Drivers tab"></td>
    <td><img src="docs/images/touchpad.png" alt="Touchpad tab"></td>
    <td><img src="docs/images/system.png" alt="System tab"></td>
  </tr>
  <tr>
    <td align="center"><sub>Drivers and device check</sub></td>
    <td align="center"><sub>Touchpad</sub></td>
    <td align="center"><sub>System</sub></td>
  </tr>
</table>

## Install

1. Download **GrandHonor-Setup-x64.exe** from the [latest release](https://github.com/TheToxicFactory/GrandHonor/releases/latest).
2. Run it and confirm the administrator prompt — GrandHonor talks to the laptop's firmware.
3. Keep **Start GrandHonor with Windows** ticked and finish. The **H** icon appears next to the clock.

> [!TIP]
> No installer wanted? Unzip **GrandHonor-win-x64.zip** and run `GrandHonor.exe`. It works the same, but does not start with Windows.

> [!NOTE]
> The setup is not code-signed yet, so Windows SmartScreen may warn: choose **More info → Run anyway**.

### Requirements

- An **HONOR MagicBook** with HONOR's firmware interface. Tested on MagicBook ZQC-P (Intel Core Ultra X7 358H, BIOS 1.12).
- **Windows 11** x64. Nothing else: no runtimes, no HONOR apps.

## Use

- **Click the tray icon** to open the panel; click anywhere else or press `Esc` to close it.
- **Right-click** for a quick menu: power mode, Eye comfort, microphone, Open, Quit.
- GrandHonor must keep running for the Fn keys, gestures and charge limits. Closing the panel keeps it running; **System → Quit** stops it completely.

| Key | Action |
|---|---|
| `Fn+P` | Efficiency → Balanced → Performance |
| `F7` | Microphone on / off (all microphones, with the key's LED) |
| `F8` | Camera on / off |
| `F12` | Screen snip |
| Refresh-rate key | 60 ↔ 120 Hz |
| Copilot, YOYO (`F11`) | Your choice in **Keyboard → Keys** |

**Updates:** System → Check for updates installs a new version from GitHub; if anything fails, the previous one comes back. **Uninstall:** Settings → Apps → Installed apps → GrandHonor.

## Help

📖 [User guide](docs/guide.md) · ❓ [FAQ and troubleshooting](docs/faq.md) · 🐞 [Report a problem](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose) · 🔒 [Security](SECURITY.md)

When reporting a problem, attach **Drivers → Device check → System report**: it copies the model, BIOS, Windows version and the event log.

## License

GrandHonor is free and open source under the [MIT License](LICENSE). The embedded Inter font is under the SIL Open Font License.<br>
GrandHonor is an independent project and is not affiliated with or endorsed by HONOR Device Co., Ltd. HONOR and MagicBook are trademarks of their owner.
