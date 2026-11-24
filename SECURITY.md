# Security

GrandHonor runs with administrator rights because it changes firmware settings, so its safety matters. This page explains what it does to stay safe and how to report a problem privately.

## Reporting a vulnerability

Please **do not open a public issue** for a security problem. Use [Report a vulnerability](https://github.com/TheToxicFactory/GrandHonor/security/advisories/new) on the Security tab of this repository instead. Describe what you found, how to reproduce it and the GrandHonor version (System tab). You will get an answer as soon as possible; fixes are released as a new version.

## Supported versions

Only the [latest release](https://github.com/TheToxicFactory/GrandHonor/releases/latest) receives fixes. **System → Check for updates** installs it.

## How GrandHonor protects you

- **No telemetry.** Nothing about you or your laptop is sent anywhere. The program goes online only when you check for updates or drivers.
- **Fixed destinations.** Driver checks talk only to HONOR's own servers; updates come only from this repository's GitHub releases. Connections use HTTPS, ignore system proxies and follow at most five redirects, each checked against that list.
- **Verified downloads.** A driver must match the SHA-256 published by HONOR and carry a valid signature; packages without a published hash are accepted only when they contain exactly one installer signed by HONOR. A GrandHonor update must match the SHA-256 that GitHub publishes for the release file.
- **Safe updates.** The previous version is kept before every update; if the new one does not start, it comes back by itself.
- **Protected install folder.** GrandHonor starts with Windows only from `C:\Program Files\GrandHonor`, which only administrators can change, and the background program checks the panel's fingerprint before starting it.
- **Your settings stay yours.** Settings are stored for your Windows account only and are never uploaded.
