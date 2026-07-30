use tracing::{debug, info};
use virtual_wall::{Result, VirtualWallManager};

use crate::{
    generator::{
        generate_host_cleanup_scripts, generate_host_runtime_scripts, generate_host_scripts,
    },
    inventory::discover_hosts,
    overlay::OverlaySpec,
    planner::plan_overlay_with_underlay,
    safety::validate_safety,
};

/// Apply overlay by generating scripts and executing them on each host via slices ssh (VirtualWall exec).
pub async fn apply_overlay(
    manager: &VirtualWallManager,
    spec: &OverlaySpec,
    underlay_override: Option<std::collections::HashMap<String, String>>,
    dry_run: bool,
) -> Result<()> {
    // Discover hosts/underlay from experiment if not provided
    let underlay_map = if let Some(map) = underlay_override {
        map
    } else {
        let inv = discover_hosts(manager).await?;
        inv.into_iter().map(|h| (h.name, h.underlay)).collect()
    };

    validate_safety(spec, &underlay_map)?;
    let plan = plan_overlay_with_underlay(spec, &underlay_map);
    let scripts = generate_host_scripts(&plan, spec.vlan_bindings.as_ref());
    let runtime_scripts = generate_host_runtime_scripts(&plan, spec);

    if dry_run {
        info!("Dry run: printing scripts only");
        for (host, script) in &scripts {
            println!("### HOST: {host}\n{script}");
        }
        return Ok(());
    }

    for (host, script) in scripts {
        info!("Applying overlay on host {}", host);
        // Use slices-based ssh via VirtualWall manager exec
        let cmd = format!("sudo bash -s <<'EOS'\n{script}\nEOS");
        let output = manager.exec(&host, &cmd, None, None, None).await?;
        debug!("Host {} output:\n{}", host, output);
    }

    // Apply runtime scripts (namespaces/veths) after overlay plumbing
    for (host, script) in runtime_scripts {
        info!("Setting up virtual nodes on host {}", host);
        let cmd = format!("sudo bash -s <<'EOS'\n{script}\nEOS");
        let output = manager.exec(&host, &cmd, None, None, None).await?;
        debug!("Host {} runtime output:\n{}", host, output);
    }

    Ok(())
}

/// Tear down overlay artifacts based on the plan.
pub async fn clean_overlay(
    manager: &VirtualWallManager,
    spec: &OverlaySpec,
    underlay_override: Option<std::collections::HashMap<String, String>>,
    dry_run: bool,
) -> Result<()> {
    let underlay_map = if let Some(map) = underlay_override {
        map
    } else {
        let inv = discover_hosts(manager).await?;
        inv.into_iter().map(|h| (h.name, h.underlay)).collect()
    };

    validate_safety(spec, &underlay_map)?;
    let plan = plan_overlay_with_underlay(spec, &underlay_map);
    let scripts = generate_host_cleanup_scripts(&plan);

    if dry_run {
        info!("Dry run: printing cleanup scripts only");
        for (host, script) in &scripts {
            println!("### HOST: {host}\n{script}");
        }
        return Ok(());
    }

    for (host, script) in scripts {
        info!("Cleaning overlay on host {}", host);
        let cmd = format!("sudo bash -s <<'EOS'\n{script}\nEOS");
        let output = manager.exec(&host, &cmd, None, None, None).await?;
        debug!("Host {} cleanup output:\n{}", host, output);
    }

    // Clean namespaces/veths best-effort
    for host in plan.host_plans.iter().map(|hp| hp.host.clone()) {
        let cmd = "sudo bash -s <<'EOS'\nfor ns in $(ip netns list | awk '{print $1}'); do ip netns del $ns 2>/dev/null || true; done\nfor v in $(ip -o link show | awk -F: '/veth-/ {print $2}' | tr -d ' '); do ip link del $v 2>/dev/null || true; done\nEOS";
        let _ = manager.exec(&host, cmd, None, None, None).await;
    }

    Ok(())
}
