use crate::*;

pub unsafe extern "C" fn x_close(arg1: *mut libsqlite3_sys::sqlite3_file) -> ::std::os::raw::c_int {
    println!("closing with arg: {:?}", arg1);
    libsqlite3_sys::SQLITE_OK
}
