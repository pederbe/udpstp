#![forbid(unsafe_code)]
use crate::windows_permissions::policy::{Ace, Dacl, Sid, Snapshot};
fn word(b: &[u8], i: usize) -> Result<u16, &'static str> {
    Ok(u16::from_le_bytes(
        b.get(i..i + 2)
            .ok_or("truncated descriptor")?
            .try_into()
            .unwrap(),
    ))
}
fn dword(b: &[u8], i: usize) -> Result<u32, &'static str> {
    Ok(u32::from_le_bytes(
        b.get(i..i + 4)
            .ok_or("truncated descriptor")?
            .try_into()
            .unwrap(),
    ))
}
pub fn sid(b: &[u8]) -> Result<Sid, &'static str> {
    if b.len() < 8 || b[0] != 1 || b[1] > 15 {
        return Err("invalid SID");
    }
    Ok(Sid(b
        .get(..8 + 4 * usize::from(b[1]))
        .ok_or("truncated SID")?
        .to_vec()))
}
pub fn parse(b: &[u8]) -> Result<Snapshot, &'static str> {
    if b.len() < 20 || b[0] != 1 || word(b, 2)? & 0x8000 == 0 {
        return Err("not a self-relative descriptor");
    }
    let owner = dword(b, 4)? as usize;
    if owner < 20 || !owner.is_multiple_of(4) {
        return Err("invalid owner offset");
    }
    let owner = sid(b.get(owner..).ok_or("owner outside descriptor")?)?;
    let offset = dword(b, 16)? as usize;
    let dacl = if word(b, 2)? & 4 == 0 {
        Dacl::Absent
    } else if offset == 0 {
        Dacl::Null
    } else {
        if offset < 20 || !offset.is_multiple_of(4) {
            return Err("invalid DACL offset");
        }
        let acl = b.get(offset..).ok_or("DACL outside descriptor")?;
        if !matches!(acl.first(), Some(2 | 4)) {
            return Err("unsupported ACL revision");
        }
        let size = usize::from(word(acl, 2)?);
        if size < 8 || !size.is_multiple_of(4) {
            return Err("invalid ACL size");
        }
        let acl = acl.get(..size).ok_or("truncated ACL")?;
        let count = usize::from(word(acl, 4)?);
        if count > 1024 {
            return Err("too many ACEs");
        }
        let mut cursor = 8;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let size = usize::from(word(acl, cursor + 2)?);
            if size < 4 || !size.is_multiple_of(4) {
                return Err("invalid ACE size");
            }
            let ace = acl.get(cursor..cursor + size).ok_or("ACE outside ACL")?;
            let kind = ace[0];
            let flags = ace[1];
            entries.push(match kind {
                0 | 1 => {
                    let mask = dword(ace, 4)?;
                    let sid = sid(ace.get(8..).ok_or("missing ACE SID")?)?;
                    if size != 8 + sid.0.len() {
                        return Err("unexpected ordinary ACE payload");
                    }
                    if kind == 0 {
                        Ace::Allow { sid, mask, flags }
                    } else {
                        Ace::Deny { mask, flags }
                    }
                }
                _ => Ace::Unsupported,
            });
            cursor += size;
        }
        Dacl::Entries(entries)
    };
    Ok(Snapshot { owner, dacl })
}
#[cfg(test)]
mod tests {
    // Every test below proves ADR 0015's safe descriptor bounds, rather
    // than an RFC protocol requirement.
    use super::*;
    fn descriptor() -> Vec<u8> {
        let mut b = vec![0; 40];
        b[0] = 1;
        b[2..4].copy_from_slice(&0x8004u16.to_le_bytes());
        b[4..8].copy_from_slice(&20u32.to_le_bytes());
        b[16..20].copy_from_slice(&32u32.to_le_bytes());
        b[20..32].copy_from_slice(&Sid::system().0);
        b[32] = 2;
        b[34] = 8;
        b
    }
    // ADR 0015: offsets and lengths from native data are bounded in safe Rust.
    #[test]
    fn every_truncation_is_rejected() {
        let b = descriptor();
        for n in 0..b.len() {
            assert!(parse(&b[..n]).is_err(), "{n}");
        }
        assert!(parse(&b).is_ok());
    }
    #[test]
    fn bad_offsets_counts_and_ace_lengths_are_rejected() {
        let b = descriptor();
        for (offset, value) in [(4, 0), (4, u32::MAX), (16, 21), (16, u32::MAX)] {
            let mut x = b.clone();
            x[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(parse(&x).is_err());
        }
        let mut x = b;
        x[36] = 1;
        assert!(parse(&x).is_err());
    }
    #[test]
    fn arbitrary_short_bytes_never_panic() {
        for size in 0..512 {
            for value in [0, 1, 2, 4, 0xff] {
                let _ = parse(&vec![value; size]);
            }
        }
    }
}
