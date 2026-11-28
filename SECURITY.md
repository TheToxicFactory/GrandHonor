# Security

GrandHonor runs with administrator rights because it changes firmware settings, so its safety matters. This page explains what it does to stay safe and how to report a problem privately.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Use [Report a vulnerability](https://github.com/TheToxicFactory/GrandHonor/security/advisories/new) on the Security tab of this repository instead. Describe what you found, how to reproduce it and the GrandHonor version (System tab). You will get an answer as soon as possible; fixes are released as a new version.

## Supported versions

Only the latest release receives fixes. Check for it in **System → Check for GrandHonor updates**, then choose **Install version**. [Latest release](https://github.com/TheToxicFactory/GrandHonor/releases/latest)

## How GrandHonor protects you

- **No telemetry.** The optional hourly GrandHonor update check requests public release metadata from GitHub. A driver check sends HONOR your laptop's model and the versions of its drivers, BIOS and Windows. The separate internet check sends one HTTPS request to Microsoft.
- **Fixed destinations.** Driver checks talk only to HONOR's servers. Updates come from this repository's GitHub releases. Connections use HTTPS and ignore system proxies. Downloads follow at most five checked redirects. The internet check uses one fixed address and follows no redirects.
- **Verified downloads.** A driver must match the SHA-256 published by HONOR and carry a valid signature; packages without a published hash (BIOS from the support site among them) are accepted only when they contain exactly one installer signed by HONOR; the other files in such a package are not checked one by one. A GrandHonor update must match the SHA-256 that GitHub publishes for the release file.
- **Safe updates.** The previous version is kept before every update; if the new one does not start, it comes back by itself.
- **Protected install folder.** GrandHonor starts with Windows only from `C:\Program Files\GrandHonor`, which only administrators can change, and the background program checks the panel's fingerprint before starting it.
- **Your settings stay yours.** Settings are stored for your Windows account only and are never uploaded.
