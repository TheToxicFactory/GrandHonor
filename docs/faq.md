# FAQ and troubleshooting

[README](../README.md) · [Guide](guide.md) · **FAQ** · [Русский](faq.ru.md)

### Which laptops are supported?
HONOR MagicBook models with HONOR's firmware interface. GrandHonor is tested on the MagicBook ZQC-P (Intel Core Ultra X7 358H, BIOS 1.12) with Windows 11. On another model the panel shows only what the firmware answers; please [report](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose) what works.

### Can I uninstall HONOR PC Manager and the other HONOR apps?
Yes: PC Manager, HONOR OSD and Magicore can all go. Keep the HONOR **drivers** in Device Manager — the camera driver, for example, provides the `F8` privacy switch. HONOR Share and multi-screen (part of Magicore) have no GrandHonor equivalent. While HONOR OSD is still installed, GrandHonor keeps it switched off so notices never appear twice; uninstalling GrandHonor turns it back on.

### Why does it need administrator rights? SmartScreen warns about the setup.
The charge limits, keyboard backlight and fan readings are firmware settings that Windows only allows for administrators. The setup is not code-signed yet, so SmartScreen does not know it: choose **More info → Run anyway**, or build it from the source code.

### How much does it use?
In the background only `GrandHonor.exe` runs, in Windows' efficiency mode, without the GPU, waiting for events. The open panel uses about 80–150 MB of memory and about 0.2 % of the CPU; once closed, it uses nothing.

### HONOR Performance or the Windows power mode?
The **Windows power mode** (Efficiency / Balanced / Performance) is what really changes speed and fan noise. **HONOR Performance** switches the firmware profile; on the tested ZQC-P it made no measurable difference under load, because the processor is held by its own thermal limits. Both are kept and restored after sleep and restart.

### Where is the CPU power limit?
It was removed: it could not hold a watt limit reliably. If an earlier version had lowered the processor setting, it is put back automatically. For a cooler, quieter laptop choose the **Quiet** processor profile, Efficiency mode or **Quiet nights**.

### A sensor shows a dash.
The firmware did not answer, or the reading is older than 3 seconds. GrandHonor never shows a guessed or outdated number.

### Why does HWiNFO show a different "system power"?
On the charger GrandHonor shows the power going **into the battery** (up to about 70 W while fast-charging), not what the whole laptop uses. On battery, **Draw** is the whole laptop and matches HWiNFO. The charger's USB-C power (for example 100 W = 20 V × 5 A) is shown under it.

### "GitHub has paused checks from your address"
GitHub allows 60 update checks an hour per internet address without an account; a VPN or a shared address can use them up. Wait the minutes shown, or download the setup from the [releases page](https://github.com/TheToxicFactory/GrandHonor/releases/latest).

### The setup says it could not close applications.
GrandHonor 2.5.2 and earlier could hang while quitting. Click **Cancel**, end **GrandHonor** in Task Manager (Details tab), run the setup again. Newer versions close by themselves.

### Are my settings kept when I update?
Yes. They live in `%APPDATA%\GrandHonor\settings.json`, which setup never touches, and a copy is saved at every start and before each update. **System → Export settings** saves them to a file of your choice. A failed update rolls back by itself; **System → Roll back** does it by hand.

### The icon vanished or the Fn keys stopped working.
Closing the panel keeps GrandHonor running — Windows may have moved the icon into the hidden area (**^**): drag it onto the taskbar. If it is not there, start GrandHonor from the Start menu and check **System → Start with Windows**.

### The colours look washed out / too saturated.
Choose a **Colour profile** in Display: **Vivid** is the panel's full colour range, **sRGB** matches most web pages and photos, **P3** is in between. Eye comfort adds warmth on top of it.

### How do I report a problem?
Open **Drivers → Device check → System report** — it copies the model, BIOS, Windows version and the event log — and paste it into a [new issue](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose). The log file itself is `%LOCALAPPDATA%\GrandHonor\GrandHonor.log`.
