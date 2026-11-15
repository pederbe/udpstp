# Retire unused features

**Status: Accepted** (2026-11-11). Supersedes ADR 0013's optional Key Table format marker and ADR 0008's experimental wake-up grouping.

The TOML Key Table's `format = 1` marker adds no meaning. Unknown fields already make the whole file fail to load, so a newer restriction cannot be silently ignored. The marker was deprecated with a warning naming the safe replacement, deleting the line. Under ADR 0013's input deprecation rule, it remained accepted throughout the minor release that first published the warning and that release's patches. Removal became eligible in a following minor release, after the deprecation shipped. Existing Key Table columns keep their meaning and unknown fields remain errors.

The warning shipped in 0.2.0 and its notice period is complete. The top-level `format` marker is now refused with exit status 51 and a message to delete its line, without displaying its value or any Shared Key. The deprecation warning and its startup-log reason are removed. Unmarked Key Tables keep their meaning.

Remove experimental wake-up grouping now. Its precision cost was never qualified, and the default already waits for the earliest deadline. Remove its environment setting, scheduling branch and `grouping_us` startup-log field. ADR 0013 permits experimental settings and the Server's 0.x JSON log to change without a deprecation period. Saved benchmark matrices, logs and reports remain readable with their historical windows. New benchmark Rounds refuse grouping requests before starting work, and maintained presets no longer request it.
