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
    ext: MaybeUninit<Arc<VFSState<T>>>, // TODO: I think this needs to be a "file-specific" pointer, even if just a thin proxy for referencing the VFS again through an Arc
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
) -> Result<&'a mut Arc<VFSState<V>>, std::io::Error> {
    let vfs: &mut libsqlite3_sys::sqlite3_vfs = ptr.as_mut().ok_or_else(null_ptr_error)?;
    let state = (vfs.pAppData as *mut Arc<VFSState<V>>)
        .as_mut()
        .ok_or_else(null_ptr_error)?;
    Ok(state)
}

unsafe fn file_state<'a, V: VFS + Sync + Sized>(
    ptr: *mut libsqlite3_sys::sqlite3_file,
) -> Result<&'a mut Arc<VFSState<V>>, std::io::Error> {
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

    // Leak the VFS name so its memory remains valid.
    let name_ptr = CString::new(name)?.into_raw();

    // Create and box an Arc<VFSState<T>>
    let state = Arc::new(VFSState {
        vfs: Arc::new(vfs),
        last_error: Arc::new(Mutex::new(None)),
    });
    let ptr = Box::into_raw(Box::new(state));

    let vfs = Box::into_raw(Box::new(libsqlite3_sys::sqlite3_vfs {
        #[cfg(not(feature = "syscall"))]
        iVersion: 2,
        #[cfg(feature = "syscall")]
        iVersion: 3,
        szOsFile: std::mem::size_of::<FileState<T>>() as i32,
        mxPathname: 512 as i32,
        pNext: null_mut(),
        zName: name_ptr,
        pAppData: ptr as _,
        xOpen: Some(vfs::x_open::<T>),
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::{register, VFS};
    use rusqlite::{Connection, OpenFlags};
    use std::{ffi::CString, fs};

    // A simple dummy VFS implementation just for testing.
    struct DummyVFS;

    impl VFS for DummyVFS {
        fn x_open(&self) {
            // This is just for demonstration.
            println!("DummyVFS::x_open was called");
        }
    }

    #[test]
    fn test_vfs_x_open_logging() {
        // Choose a unique name for your custom VFS. This must match when opening the connection.
        let vfs_name = "dummyvfs";

        // Register your dummy VFS.
        // (Any error here means the registration failed. In a real test you might want to tear down the file afterwards.)
        register(vfs_name, true, DummyVFS).expect("failed to register dummy VFS");

        // Check that the VFS is registered
        let found_vfs = unsafe {
            let c_vfs_name = CString::new(vfs_name).unwrap();
            libsqlite3_sys::sqlite3_vfs_find(c_vfs_name.as_ptr())
        };
        if found_vfs.is_null() {
            println!("VFS {} is not registered!", vfs_name);
        } else {
            println!("VFS {} registered: {:?}", vfs_name, found_vfs);
        }

        // Open a SQLite connection using your custom VFS by passing its name.
        // Note: Instead of using ":memory:" you might want to use a file so that SQLite calls x_open.
        let db_path = "dummy.db";
        // Remove any previous file.
        let _ = fs::remove_file(db_path);
        let conn = Connection::open_with_flags_and_vfs(
            db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            vfs_name,
        )
        .expect("failed to open connection with dummy VFS");

        // Use the connection so that SQLite will perform file I/O and trigger x_open.
        conn.execute("CREATE TABLE test (id INTEGER)", [])
            .expect("failed to create table");

        // When running tests with `cargo test -- --nocapture` you should see the
        // output from the println! inside x_open (and from DummyVFS::x_open if called).
        drop(conn);
        let _ = fs::remove_file(db_path);
    }
}
