# Retire unused features

**Status: Accepted** (2026-11-11). Supersedes ADR 0013's optional Key Table format marker.

The TOML Key Table's `format = 1` marker adds no meaning. Unknown fields already make the whole file fail to load, so a newer restriction cannot be silently ignored. Deprecate the marker and keep accepting it with a warning naming the safe replacement: delete the line. Under ADR 0013's input deprecation rule, it remains accepted throughout the minor release that first publishes the warning and that release's patches. Removal is eligible only in the following minor release, after the deprecation has shipped. Existing Key Table columns keep their meaning. Unknown fields and unsupported marker values remain errors. Rejecting valid marked files immediately would break the released input contract.
