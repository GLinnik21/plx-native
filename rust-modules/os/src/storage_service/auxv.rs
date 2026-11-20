//! Rust's executable startup asks getauxval, introduced after our glibc 2.12 baseline.
//! Read Linux's process-owned auxiliary vector using the baseline open/read/close ABI.
//! No allocation, std, or dynamic initialization: this runs before Rust main.
#[no_mangle]
pub unsafe extern "C" fn getauxval(kind: libc::c_ulong) -> libc::c_ulong {
    let fd = libc::open(
        c"/proc/self/auxv".as_ptr(),
        libc::O_RDONLY | libc::O_CLOEXEC,
    );
    if fd < 0 {
        return 0;
    }
    let mut entry = [0 as libc::c_ulong; 2];
    for _ in 0..256 {
        let mut used = 0;
        while used < std::mem::size_of_val(&entry) {
            let count = libc::read(
                fd,
                (entry.as_mut_ptr() as *mut u8).add(used).cast(),
                std::mem::size_of_val(&entry) - used,
            );
            if count < 0 && *libc::__errno_location() == libc::EINTR {
                continue;
            }
            if count <= 0 {
                libc::close(fd);
                *libc::__errno_location() = libc::ENOENT;
                return 0;
            }
            used += count as usize;
        }
        if entry[0] == kind {
            libc::close(fd);
            return entry[1];
        }
        if entry[0] == 0 {
            break;
        }
    }
    libc::close(fd);
    *libc::__errno_location() = libc::ENOENT;
    0
}
