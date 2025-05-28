use sysinfo::Networks;

/// Get a list of all available network interfaces on the system.
pub fn get_all_interfaces() -> Vec<String> {
    let networks = Networks::new_with_refreshed_list();
    networks.keys().cloned().collect()
}