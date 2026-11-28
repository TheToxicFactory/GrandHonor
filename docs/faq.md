# FAQ and troubleshooting

[README](../README.md) · [Guide](guide.md) · **FAQ** · [Русский](faq.ru.md)

### Which laptops are supported?
HONOR MagicBook models with HONOR's firmware interface. GrandHonor is tested on the MagicBook ZQC-P (Intel Core Ultra X7 358H, BIOS 1.12) with Windows 11. On another model the panel shows only what the firmware answers; please [report](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose) what works.

### Can I uninstall HONOR PC Manager and the other HONOR apps?
Yes: PC Manager, HONOR OSD and Magicore can all go. Keep the HONOR **drivers** in Device Manager — the camera driver, for example, provides the `F8` privacy switch. HONOR Share and multi-screen (part of Magicore) have no GrandHonor equivalent. While HONOR OSD is still installed, GrandHonor keeps it switched off so notices never appear twice; uninstalling GrandHonor turns it back on.

### Why does it need administrator rights? SmartScreen warns about the setup.
The charge limits, keyboard backlight and fan readings are firmware settings that Windows only allows for administrators. The setup is not code-signed yet, so SmartScreen does not know it: choose **More info → Run anyway**, or build it from the source code. With **Smart App Control** turned on, Windows blocks unsigned programs without that button: GrandHonor can then only be built from source, or Smart App Control turned off in Windows Security → App & browser control (it cannot be turned back on without resetting Windows).

### How much does it use?
In the background only `GrandHonor.exe` runs, in Windows' efficiency mode, without the GPU, waiting for events. The open panel uses about 80–150 MB of memory and about 0.2 % of the CPU; once closed, it uses nothing.

### Why is HONOR Performance unavailable in Fast?
Fast requests the firmware's performance mode when Windows accepts Performance, the charger is connected, the battery has at least 20 % and HUNTER is off. Open **Power → Applied settings** to see which changes were applied.

### Where is the CPU power limit?
It was removed because it could not hold a watt limit reliably. If an earlier version lowered the processor setting, GrandHonor restores it automatically. Choose **Quiet** or use **Quiet nights** to favor lower power use.

### A sensor shows a dash.
GrandHonor has no fresh reading. The load row needs two samples for CPU, GPU and NPU, so give it a second after opening **Power**. If an accelerator still shows a dash, Windows or the driver may not expose its usage.

### Why did the colour filter pause?
The colour filters pause while HDR is enabled. The built-in panel's sRGB and P3 adjustments also pause when an external display is active.

### Why doesn't the four-finger swipe seek in this player?
Turn on **Touchpad → Media scrubbing** and choose **Nothing** for four-finger swipes left and right in Windows Advanced gestures. Start a video and keep its window active. Lift your fingers to apply the position you selected. GrandHonor uses the player's Windows media session or an accessible seek slider. Some players, live streams and browser sites expose neither.

### The bottom corner also opens a menu or the edge gesture scrolls the page.
Windows still receives the normal click or scroll. If it gets in the way, set that gesture to **Off** in the Touchpad tab.

### Why is a reading missing from the tray?
Turn on the reading in **System → Tray readings** and check the Windows taskbar's hidden icons. Readings pause when the display turns off.

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

### How do I check the hardware?
Open **Drivers → Device check → Run check**. The page reports Windows device errors and the outcome of each test. **Could not check** means Windows or the device did not return enough information. **Needs your input** asks you to try a test and confirm the result.

### Why does F11 run a GrandHonor action?
On laptops where YOYO sends `F11`, its assignment also applies to regular `F11`, including on an external keyboard. Choose **Keyboard → Keys → YOYO key (F11) → Windows default** to restore the usual action.

### How do I report a problem?
Open **Drivers → Device check → System report** — it copies the model, BIOS, Windows version and the event log — and paste it into a [new issue](https://github.com/TheToxicFactory/GrandHonor/issues/new/choose). The log file itself is `%LOCALAPPDATA%\GrandHonor\GrandHonor.log`.
