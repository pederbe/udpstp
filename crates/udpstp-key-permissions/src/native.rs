//! ADR 0015's sole native inspection adapter. No pointer escapes.
use crate::{
    descriptor,
    policy::{Sid, Snapshot},
};
use std::{
    fs::File,
    io,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle},
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::*,
    Storage::FileSystem::*,
    System::{SystemServices::FILE_PERSISTENT_ACLS, Threading::*},
};
const LIMIT: usize = 128 * 1024;

fn descriptor_bytes(handle: BorrowedHandle<'_>) -> io::Result<Vec<u8>> {
    let mut needed = 0;
    // SAFETY: borrowed handle remains live; output DWORD is valid; null buffer
    // and length zero request the required allocation size without a write.
    let result = unsafe {
        GetKernelObjectSecurity(
            handle.as_raw_handle(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if result != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(io::Error::last_os_error());
    }
    let size = needed as usize;
    if !(20..=LIMIT).contains(&size) {
        return Err(io::Error::other("descriptor exceeds supported bounds"));
    }
    // Pointer-sized elements align storage for the native API. Initialization
    // means even a short native write cannot expose uninitialized Rust bytes.
    let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
    // SAFETY: the allocation covers size bytes, output pointers remain live.
    // A size change or any other error is not parsed and is not retried as pass.
    if unsafe {
        GetKernelObjectSecurity(
            handle.as_raw_handle(),
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            buffer.as_mut_ptr().cast(),
            size as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if needed as usize != size {
        return Err(io::Error::other("inconsistent descriptor length"));
    }
    // SAFETY: every byte in this aligned allocation was initialized; length is
    // checked above. Copy owned bytes before buffer is freed by Rust.
    Ok(
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), needed as usize) }
            .to_vec(),
    )
}

pub fn running_sid() -> io::Result<Sid> {
    let mut raw = ptr::null_mut();
    // SAFETY: pseudo process handle is valid; raw is a valid output pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: success transfers exactly one owned token handle, closed on every
    // path by OwnedHandle, never by the borrowed file adapter.
    let token = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) };
    let mut needed = 0;
    // SAFETY: token is live, null/zero performs sizing, output is valid.
    let result = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if result != 0
        || io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32)
    {
        return Err(io::Error::last_os_error());
    }
    let size = needed as usize;
    if !(size_of::<TOKEN_USER>()..=LIMIT).contains(&size) {
        return Err(io::Error::other("token data exceeds supported bounds"));
    }
    let mut buffer = vec![0usize; size.div_ceil(size_of::<usize>())];
    // SAFETY: aligned buffer covers the supplied size; token stays live.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if needed as usize != size {
        return Err(io::Error::other("inconsistent token length"));
    }
    // SAFETY: the buffer is aligned and large enough for TOKEN_USER. The
    // internal SID pointer is converted to an offset and bounded before use.
    let user = unsafe { ptr::read(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let offset = (user.User.Sid as usize)
        .checked_sub(buffer.as_ptr() as usize)
        .ok_or_else(|| io::Error::other("SID before token buffer"))?;
    if offset >= needed as usize {
        return Err(io::Error::other("SID outside token buffer"));
    }
    // SAFETY: initialized buffer bytes, checked length, no raw pointer returned.
    let bytes =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), needed as usize) };
    descriptor::sid(&bytes[offset..]).map_err(io::Error::other)
}

pub fn inspect(file: &File) -> io::Result<Snapshot> {
    let handle = file.as_handle();
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut fs = [0u16; 32];
    let mut flags = 0;
    // SAFETY: handle is borrowed, the fixed output buffers match their lengths;
    // every omitted optional output is null. The API owns no returned memory.
    if unsafe {
        GetVolumeInformationByHandleW(
            handle.as_raw_handle(),
            ptr::null_mut(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut flags,
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = fs
        .iter()
        .position(|x| *x == 0)
        .ok_or_else(|| io::Error::other("unterminated filesystem name"))?;
    if String::from_utf16_lossy(&fs[..end]) != "NTFS" || flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(io::Error::other("only local NTFS is supported"));
    }
    let mut path = vec![0u16; 32768];
    // SAFETY: live handle and initialized writable buffer of the passed length.
    // GUID volume names identify local volumes; network shares have none.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            handle.as_raw_handle(),
            path.as_mut_ptr(),
            path.len() as u32,
            VOLUME_NAME_GUID | FILE_NAME_OPENED,
        )
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= path.len()
        || !String::from_utf16_lossy(&path[..length as usize]).starts_with("\\\\?\\Volume{")
    {
        return Err(io::Error::other("unsupported storage"));
    }
    descriptor::parse(&descriptor_bytes(handle)?).map_err(io::Error::other)
}
