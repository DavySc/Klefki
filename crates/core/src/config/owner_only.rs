//! Owner-only access on Windows, as mode 0600/0700 gives on Unix: a protected access list with one entry, the
//! current user's, so nothing is inherited from the folder above and no other account (not even administrators)
//! is listed.
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
    TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetTokenInformation, NO_INHERITANCE,
    PROTECTED_DACL_SECURITY_INFORMATION, SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY,
    TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Leaves `path` reachable by the current user alone. A folder's entry is inherited by what is made in it later.
pub(crate) fn restrict(path: &Path, folder: bool) -> io::Result<()> {
    let user = current_user()?;
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if folder {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.sid().cast(),
            ..Default::default()
        },
    };
    let mut acl: *mut ACL = std::ptr::null_mut();
    // SAFETY: one entry, read during the call; its SID lives in `user`, which outlives it. No old list to merge.
    // A list returned is ours to free with `LocalFree`.
    let error = unsafe { SetEntriesInAclW(1, &access, std::ptr::null(), &mut acl) };
    if error != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is a NUL-terminated path and `acl` the list just built; owner, group and audit list are left
    // as they are (null).
    let error = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    // SAFETY: allocated by `SetEntriesInAclW` above and not used after this.
    unsafe { LocalFree(acl.cast()) };
    if error != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(())
}

/// The current user's token information, which holds their SID.
struct User(Vec<u64>);

impl User {
    fn sid(&self) -> *mut core::ffi::c_void {
        // The buffer starts with a TOKEN_USER (u64 elements keep it aligned for it).
        let user = self.0.as_ptr().cast::<TOKEN_USER>();
        // SAFETY: filled by `GetTokenInformation(TokenUser)`, whose SID pointer points into this same buffer.
        unsafe { (*user).User.Sid }
    }
}

fn current_user() -> io::Result<User> {
    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the pseudo handle of this process needs no closing; a token returned is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0u32;
    // SAFETY: asks only for the size; the call fails with "insufficient buffer" and sets `len`.
    unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len) };
    let mut buffer = vec![0u64; (len as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` is at least `len` bytes, aligned for TOKEN_USER, and the token is open.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * 8) as u32,
            &mut len,
        )
    };
    let error = io::Error::last_os_error();
    // SAFETY: opened above and not used after this.
    unsafe { CloseHandle(token) };
    if ok == 0 {
        return Err(error);
    }
    Ok(User(buffer))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL_SIZE_INFORMATION, AclSizeInformation, EqualSid, GetAce,
        GetAclInformation, GetSecurityDescriptorControl, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
    };

    /// Whether `path`'s access list is protected and has the current user's entry alone.
    pub(crate) fn is_owner_only(path: &Path) -> bool {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: a NUL-terminated path; the descriptor returned is freed below and `dacl` points into it.
        let error = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        assert_eq!(error, ERROR_SUCCESS, "{}", path.display());
        let user = current_user().unwrap();
        let (mut control, mut revision) = (0u16, 0u32);
        let mut size = ACL_SIZE_INFORMATION::default();
        let mut ace: *mut core::ffi::c_void = std::ptr::null_mut();
        // SAFETY: `sd` and `dacl` are valid until freed; each out-parameter is the type the call takes.
        let only = unsafe {
            GetSecurityDescriptorControl(sd, &mut control, &mut revision) != 0
                && control & SE_DACL_PROTECTED != 0
                && !dacl.is_null()
                && GetAclInformation(
                    dacl,
                    (&mut size as *mut ACL_SIZE_INFORMATION).cast(),
                    size_of::<ACL_SIZE_INFORMATION>() as u32,
                    AclSizeInformation,
                ) != 0
                && size.AceCount == 1
                && GetAce(dacl, 0, &mut ace) != 0
                && EqualSid(
                    (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast(),
                    user.sid(),
                ) != 0
        };
        // SAFETY: returned by `GetNamedSecurityInfoW` above and not used after this.
        unsafe { LocalFree(sd) };
        only
    }
}
