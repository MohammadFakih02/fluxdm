#[cfg(windows)]
pub fn mark_file_sparse(f: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::{
        Win32::Foundation::HANDLE, Win32::System::IO::DeviceIoControl,
        Win32::System::Ioctl::FSCTL_SET_SPARSE,
    };

    let handle = HANDLE(f.as_raw_handle());

    unsafe { DeviceIoControl(handle, FSCTL_SET_SPARSE, None, 0, None, 0, None, None).is_ok() }
}
// FluxDM patch: dot-prefixed folders also need the hidden attribute on Windows.
#[cfg(windows)]
pub fn hide_directory(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
    use windows::{
        Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_FLAGS_AND_ATTRIBUTES, SetFileAttributesW,
        },
        core::PCWSTR,
    };
    let attrs = std::fs::metadata(path)?.file_attributes();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        SetFileAttributesW(
            PCWSTR(path.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attrs) | FILE_ATTRIBUTE_HIDDEN,
        )?;
    }
    Ok(())
}

// FluxDM patch: publishing a filename must never replace a destination created concurrently.
pub fn rename_new(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)] {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
        let a: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
        let b: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVE_FILE_FLAGS(0)) }.map_err(std::io::Error::other)
    }
    #[cfg(not(windows))] {
        std::fs::hard_link(from, to)?;
        std::fs::remove_file(from)
    }
}
