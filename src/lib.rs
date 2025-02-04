use std::{
    cell::OnceCell,
    ffi::{c_char, CString},
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::null_mut,
    sync::{Arc, Mutex},
};

use tracing::debug;

// #[no_mangle]
// pub extern "C" fn sqlite3_testvfs_init(
//   db: *mut sqlite_ffi::db,
//   pzErrMsg: *mut *mut c_char,
//   pApi: *const sqlite_ffi::sqlite3_api_routines,
// ) -> i32 {
//   debug!("sqlite3_testvfs_init");
//   // Initialize your extension (e.g. register your VFS)
//   // You can use the SQLite extension macros if desired (see sqlite3ext.h in the C world)
//   sqlite_ffi::SQLITE_OK
// }

pub trait VFS {
    fn x_open(&self);
}

struct VFSState<T: VFS + Sized> {
    vfs: Arc<T>,
    last_error: Arc<Mutex<Option<(i32, std::io::Error)>>>, // sqlite error, rust error
}

/// FileState is a wrapper around the sqlite3_file struct that contains the VFS state.
/// Because SQLite allocates this initially, the ext might not exist, so we use a MaybeUninit.
struct FileState<T: VFS + Sized> {
    base: libsqlite3_sys::sqlite3_file,
    ext: MaybeUninit<T>, // TODO: I think this needs to be a "file-specific" pointer, even if just a thin proxy for referencing the VFS again through an Arc
}

impl<T: VFS + Sized> VFSState<T> {
    /// Set the last error for the VFS. Returns the error code for convenience so it can be retured.
    fn set_last_error(&self, code: i32, error: std::io::Error) -> i32 {
        debug!("setting last error: {:?}, {:?}", code, error);
        self.last_error.lock().unwrap().insert((code, error));
        return code;
    }
}

fn null_ptr_error() -> std::io::Error {
    std::io::Error::new(ErrorKind::Other, "received null pointer")
}

unsafe fn vfs_state<'a, V: VFS + Sync + Sized>(
    ptr: *mut libsqlite3_sys::sqlite3_vfs,
) -> Result<&'a mut VFSState<V>, std::io::Error> {
    let vfs: &mut libsqlite3_sys::sqlite3_vfs = ptr.as_mut().ok_or_else(null_ptr_error)?;
    let state = (vfs.pAppData as *mut VFSState<V>)
        .as_mut()
        .ok_or_else(null_ptr_error)?;
    Ok(state)
}

unsafe fn file_state<'a, V: VFS + Sync + Sized>(
    ptr: *mut libsqlite3_sys::sqlite3_file,
) -> Result<&'a mut V, std::io::Error> {
    let f = (ptr as *mut FileState<V>)
        .as_mut()
        .ok_or_else(null_ptr_error)?;
    let ext = f.ext.assume_init_mut();
    Ok(ext)
}

#[derive(Debug)]
pub enum RegisterError {
    Nul(std::ffi::NulError),
    Register(i32),
}

impl std::error::Error for RegisterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nul(err) => Some(err),
            Self::Register(_) => None,
        }
    }
}

impl std::fmt::Display for RegisterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nul(_) => f.write_str("interior nul byte in name found"),
            Self::Register(code) => {
                write!(f, "registering sqlite vfs failed with error code: {}", code)
            }
        }
    }
}

impl From<std::ffi::NulError> for RegisterError {
    fn from(err: std::ffi::NulError) -> Self {
        Self::Nul(err)
    }
}

mod io_methods;
mod vfs;

pub fn register<T: VFS + Sync + Sized>(
    name: &str,
    as_default: bool,
    vfs: T,
) -> Result<(), RegisterError> {
    let io_methods = libsqlite3_sys::sqlite3_io_methods {
        iVersion: 2,
        xClose: Some(io_methods::x_close),
        xRead: None,
        xWrite: None,
        xTruncate: None,
        xSync: None,
        xFileSize: None,
        xLock: None,
        xUnlock: None,
        xCheckReservedLock: None,
        xFileControl: None,
        xSectorSize: None,
        xDeviceCharacteristics: None,
        xShmMap: None,
        xShmLock: None,
        xShmBarrier: None,
        xShmUnmap: None,
        xFetch: None,
        xUnfetch: None,
    };
    let name = CString::new(name)?;
    let name_ptr = name.as_ptr();
    let ptr = Box::into_raw(Box::new(VFSState {
        //   name,
        vfs: Arc::new(vfs),
        last_error: Arc::new(Mutex::new(None)),
        //   #[cfg(any(feature = "syscall", feature = "loadext"))]
        //   parent_vfs: unsafe { ffi::sqlite3_vfs_find(std::ptr::null_mut()) },
        //   io_methods,
        //   last_error: Default::default(),
        //   next_id: 0,
    }));
    let vfs = Box::into_raw(Box::new(libsqlite3_sys::sqlite3_vfs {
        #[cfg(not(feature = "syscall"))]
        iVersion: 2,
        #[cfg(feature = "syscall")]
        iVersion: 3,
        szOsFile: size_of::<VFSState<T>>() as i32,
        mxPathname: 512 as i32, // max path length supported by VFS
        pNext: null_mut(),
        zName: name_ptr,
        pAppData: ptr as _,
        xOpen: Some(vfs::open::<T>),
        xDelete: None,
        xAccess: None,
        xFullPathname: None,
        xDlOpen: None,
        xDlError: None,
        xDlSym: None,
        xDlClose: None,
        xRandomness: None,
        xSleep: None,
        xCurrentTime: None,
        xGetLastError: None,
        xCurrentTimeInt64: None,

        #[cfg(not(feature = "syscall"))]
        xSetSystemCall: None,
        #[cfg(not(feature = "syscall"))]
        xGetSystemCall: None,
        #[cfg(not(feature = "syscall"))]
        xNextSystemCall: None,

        #[cfg(feature = "syscall")]
        xSetSystemCall: Some(vfs::set_system_call::<V>),
        #[cfg(feature = "syscall")]
        xGetSystemCall: Some(vfs::get_system_call::<V>),
        #[cfg(feature = "syscall")]
        xNextSystemCall: Some(vfs::next_system_call::<V>),
    }));

    let result = unsafe { libsqlite3_sys::sqlite3_vfs_register(vfs, as_default as i32) };
    if result != libsqlite3_sys::SQLITE_OK {
        return Err(RegisterError::Register(result));
    }

    // TODO: return object that allows to unregister (and cleanup the memory)?

    Ok(())
}
