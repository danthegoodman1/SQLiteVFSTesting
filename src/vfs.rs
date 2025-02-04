use crate::*;

pub unsafe extern "C" fn x_open<V: VFS + Sync + Sized>(
    arg1: *mut libsqlite3_sys::sqlite3_vfs,
    zName: *const ::std::os::raw::c_char,
    arg2: *mut libsqlite3_sys::sqlite3_file,
    flags: ::std::os::raw::c_int,
    pOutFlags: *mut ::std::os::raw::c_int,
) -> ::std::os::raw::c_int {
    println!(
        "opening with args: {:?}, {:?}, {:?}, {:?}, {:?}",
        arg1, zName, arg2, flags, pOutFlags
    );

    let state = match vfs_state::<V>(arg1) {
        Ok(state) => state,
        Err(_) => return libsqlite3_sys::SQLITE_ERROR,
    };

    let out_file = match (arg2 as *mut FileState<V>).as_mut() {
        Some(f) => f,
        None => {
            return state.set_last_error(
                libsqlite3_sys::SQLITE_CANTOPEN,
                std::io::Error::new(ErrorKind::Other, "invalid file pointer"),
            );
        }
    };
    // out_file.base.pMethods = &state.io_methods;
    out_file.ext.write(state.clone());

    libsqlite3_sys::SQLITE_OK
}
