use std::net::Ipv4Addr;

#[cfg(unix)]
pub fn ifindex_by_name(name: &str) -> Option<u32> {
    // SAFETY: libc if_nametoindex returns 0 on error
    let c = std::ffi::CString::new(name).ok()?;
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    (idx != 0).then_some(idx as u32)
}

#[cfg(windows)]
pub fn ifindex_by_name(_name: &str) -> Option<u32> {
    // TODO: implement via GetAdaptersAddresses
    None
}

#[cfg(unix)]
pub fn ipv4_of(name: &str) -> Option<Ipv4Addr> {
    // TODO: Minimal impl: use std::process::Command to query or add a proper getifaddrs impl later.
    // Keep it simple for now; return None by default.
    let _ = name;
    None
}

#[cfg(windows)]
pub fn ipv4_of(_name: &str) -> Option<Ipv4Addr> {
    // TODO: implement via GetAdaptersAddresses
    None
}
