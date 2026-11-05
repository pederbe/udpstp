#![forbid(unsafe_code)]

use crate::Assessment;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sid(pub Vec<u8>);
impl Sid {
    pub fn system() -> Self {
        Self(vec![1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0])
    }
    pub fn admins() -> Self {
        Self(vec![1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 32, 2, 0, 0])
    }
}
#[derive(Debug, Clone)]
pub enum Ace {
    Allow { sid: Sid, mask: u32, flags: u8 },
    Deny { mask: u32, flags: u8 },
    Unsupported,
}
#[derive(Debug, Clone)]
pub enum Dacl {
    Absent,
    Null,
    Entries(Vec<Ace>),
}
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub owner: Sid,
    pub dacl: Dacl,
}

// File generic mappings from WinNT.h. GENERIC_EXECUTE includes FILE_EXECUTE,
// which Windows permits for executable sections; treat it as content access.
pub fn map_generic(mut mask: u32) -> u32 {
    for (generic, specific) in [
        (0x8000_0000, 0x12_0089),
        (0x4000_0000, 0x12_0116),
        (0x2000_0000, 0x12_00a0),
        (0x1000_0000, 0x1f_01ff),
    ] {
        if mask & generic != 0 {
            mask = (mask & !generic) | specific;
        }
    }
    mask
}
// Read/write/append/execute, write EA/attributes, delete, WRITE_DAC/OWNER.
// READ_CONTROL, SYNCHRONIZE and read EA/attributes alone reveal no key bytes.
pub const DANGEROUS: u32 = 0x000d_0137;
const KNOWN: u32 = 0x001f_01ff;
const INHERIT_ONLY: u8 = 0x08;
const ORDINARY_ACE_FLAGS: u8 = 0x1f;

pub fn assess(snapshot: &Snapshot, running: &Sid) -> Assessment {
    let trusted = |sid: &Sid| sid == running || *sid == Sid::system() || *sid == Sid::admins();
    // Unsupported forms always win, so a partial descriptor is never called verified.
    if let Dacl::Entries(entries) = &snapshot.dacl {
        for ace in entries {
            match ace {
                Ace::Unsupported => return Assessment::Unverified("unsupported ACE type".into()),
                Ace::Allow { mask, flags, .. } | Ace::Deny { mask, flags, .. } => {
                    if flags & !ORDINARY_ACE_FLAGS != 0 || map_generic(*mask) & !KNOWN != 0 {
                        return Assessment::Unverified(
                            "unsupported ACE flags or access rights".into(),
                        );
                    }
                }
            }
        }
    }
    if !trusted(&snapshot.owner) {
        return Assessment::Mismatch("untrusted owner");
    }
    match &snapshot.dacl {
        Dacl::Absent | Dacl::Null => Assessment::Mismatch("absent or null DACL"),
        Dacl::Entries(entries) => {
            // Denies cannot cancel an outsider grant under this structural policy.
            // This deliberately avoids group expansion and effective-access claims.
            for ace in entries {
                if let Ace::Allow { sid, mask, flags } = ace
                    && flags & INHERIT_ONLY == 0
                    && !trusted(sid)
                    && map_generic(*mask) & DANGEROUS != 0
                {
                    return Assessment::Mismatch("dangerous grant to an untrusted identity");
                }
            }
            Assessment::Meets
        }
    }
}

#[cfg(test)]
mod tests {
    // Every test below proves ADR 0015's supported private-file policy,
    // spec story 39, rather than an RFC requirement.
    use super::*;
    fn current() -> Sid {
        Sid(vec![1, 1, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0])
    }
    fn outsider() -> Sid {
        Sid(vec![1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0])
    }
    fn with(entries: Vec<Ace>) -> Snapshot {
        Snapshot {
            owner: current(),
            dacl: Dacl::Entries(entries),
        }
    }
    fn allow(sid: Sid, mask: u32, flags: u8) -> Ace {
        Ace::Allow { sid, mask, flags }
    }
    // Spec story 39 and ADR 0015: private-file assessment, not RFC behavior.
    #[test]
    fn trusted_identities_and_empty_dacl() {
        assert_eq!(assess(&with(vec![]), &current()), Assessment::Meets);
        for sid in [current(), Sid::system(), Sid::admins()] {
            let mut s = with(vec![allow(sid.clone(), 0x1000_0000, 0)]);
            s.owner = sid;
            assert_eq!(assess(&s, &current()), Assessment::Meets);
        }
    }
    #[test]
    fn metadata_and_dangerous_rights_are_distinct() {
        for mask in [0x80, 8, 0x20000, 0x100000, 0x120088] {
            assert_eq!(
                assess(&with(vec![allow(outsider(), mask, 0)]), &current()),
                Assessment::Meets
            );
        }
        for mask in [
            1,
            2,
            4,
            0x10,
            0x20,
            0x100,
            0x10000,
            0x40000,
            0x80000,
            0x8000_0000,
            0x4000_0000,
            0x2000_0000,
            0x1000_0000,
        ] {
            assert!(
                matches!(
                    assess(&with(vec![allow(outsider(), mask, 0)]), &current()),
                    Assessment::Mismatch(_)
                ),
                "{mask:x}"
            );
        }
    }
    #[test]
    fn inheritance_and_denies_do_not_hide_grants() {
        assert_eq!(
            assess(&with(vec![allow(outsider(), 1, 0x18)]), &current()),
            Assessment::Meets
        );
        let deny = Ace::Deny {
            mask: 0x1000_0000,
            flags: 0,
        };
        assert!(matches!(
            assess(&with(vec![deny, allow(outsider(), 1, 0x10)]), &current()),
            Assessment::Mismatch(_)
        ));
    }
    #[test]
    fn unknown_cases_are_unverified_even_beside_a_mismatch() {
        for ace in [
            Ace::Unsupported,
            allow(outsider(), 0x0200_0000, 0),
            allow(current(), 1, 0x40),
        ] {
            assert!(matches!(
                assess(&with(vec![allow(outsider(), 1, 0), ace]), &current()),
                Assessment::Unverified(_)
            ));
        }
    }
    #[test]
    fn owners_and_unrestricted_dacls() {
        for dacl in [Dacl::Absent, Dacl::Null] {
            assert!(matches!(
                assess(
                    &Snapshot {
                        owner: current(),
                        dacl
                    },
                    &current()
                ),
                Assessment::Mismatch(_)
            ));
        }
        assert!(matches!(
            assess(
                &Snapshot {
                    owner: outsider(),
                    dacl: Dacl::Entries(vec![])
                },
                &current()
            ),
            Assessment::Mismatch(_)
        ));
    }
    #[test]
    fn generic_mapping_matches_file_mapping() {
        for (mask, expected) in [
            (0x80000000, 0x120089),
            (0x40000000, 0x120116),
            (0x20000000, 0x1200a0),
            (0x10000000, 0x1f01ff),
        ] {
            assert_eq!(map_generic(mask), expected);
        }
    }
}
