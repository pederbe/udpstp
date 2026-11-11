# Latest-only distribution

**Status: Accepted** (2026-11-11). Supersedes the container-tag policy in [ADR 0013](0013-stable-interfaces-from-0-1-0.md#the-container-image).

Installation channels offer the latest release. Current apt and dnf indexes list only that release, and new container publications update only `ghcr.io/pederbe/udpstp:latest`. This keeps installation and upgrade instructions focused on the supported release instead of maintaining version-selection paths.

Keep already published package files, signed metadata snapshots and versioned container tags available. They preserve downloads already in progress and existing references. Older versions remain internal upgrade fixtures, installed directly before upgrading through the latest index. Pre-release container qualification and release-artifact verification continue. Manual package publication selects the latest release, and both publication workflows check that their release is still current before advancing the channel.
