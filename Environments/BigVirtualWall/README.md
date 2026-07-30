# Big Virtual Wall

BigVirtualWall layers on top of the VirtualWall crate to let you run “bigger than possible” overlays: one netns/Mininet per bare-metal host, stitched with VXLAN/VLAN so you can emulate many virtual links and nodes while only using a limited set of physical interfaces. The intent is to feel like VirtualWall (same lifecycle and ssh/execution flows) while spoofing a larger virtual topology on the existing experiment.

## Current capabilities
- **Dynamic specs**: `OverlaySpec` supports explicit hosts/nodes or pools (`host_pool`, `virtual_pool`, `vlan_pool`) to auto-generate hosts, virtual nodes, and VLAN ranges from counts/prefixes.
- **Overlay planner**: assigns VLANs for cross-host links, groups per host, and supports optional tunnels/VXLAN/VNI overrides.
- **Script generator**: emits per-host setup (OVS bridge, VXLAN devices, VLAN subinterfaces, tc impairments) and cleanup scripts.
- **Safety**: validates underlay presence per host, keeps VLANs off physical NICs (only on VXLAN devices), guards mgmt-like underlay IPs unless `BIGVW_ALLOW_MGMT_UNDERLAY=1`, and enforces 802.1Q VLAN bounds.
- **CLI**: `overlay plan/apply/clean --spec ... [--experiment ...] [--underlay-map ...] [--dry-run]` with async runtime and auto discovery of underlay from a live experiment.
- **Examples**: pooled specs for ring/full-mesh/tree (`specs/*.yaml`) using 10 hosts and `host: auto` pools.
- **Controller integration**: BigVirtualWall handler provisions the base Virtual Wall, applies overlays, caches the spec/experiment, and surfaces overlay topology in status. VirtualWall handler can visualize the base experiment to a base64 PNG.

## Usage
1) Provision & overlay in one shot:
   ```
   overlay apply --spec Environments/BigVirtualWall/specs/ring.yaml --experiment multipathxr
   ```
   or via controller query: `spec=...&experiment=...&dry_run=1` (provisions base, discovers underlay, applies overlay).

2) Plan only:
   ```
   overlay plan --spec Environments/BigVirtualWall/specs/ring.yaml --experiment multipathxr --dry_run
   ```
   Use `--out-dir` to write per-host scripts.

3) Clean overlay:
   ```
   overlay clean --spec ... --experiment multipathxr
   ```
   (Controller `stop` also attempts best-effort clean using cached spec.)

## What’s still lacking / next work
- **Overlay observability**: enrich controller `status/links` with per-host VXLAN/VLAN info (partially done); expose planned links explicitly.
- **Explicit clean entrypoint in controller**: allow overlay teardown without stopping the base (currently best-effort on stop).
- **Optional underlay override**: allow passing an underlay map via controller params (default remains auto-discovery).
- **Integration tests**: smoke-test pool specs end-to-end (dry-run) to ensure planner/generator outputs and underlay resolution.
- **Docs/quickstart**: controller usage examples, lifecycle ordering (base provision → overlay apply; clean → optional stop), multicast/broadcast expectations (VXLAN carries BUM).
- **Safety hardening**: explicit mgmt NIC detection/ban, configurable VLAN/VNI pools to avoid collisions in shared environments.
- **Virtual node networking**: auto-assign IP/MTU defaults per VLAN link (/30s), but still missing per-namespace routes/gateways for multi-hop; and veth host ends should be attached to the correct VLAN subinterface for cross-host isolation.
- **Cleanup precision**: netns/veth cleanup is global/best-effort; make it targeted to the current overlay plan.

## Plan to support large shared-VLAN virtual nodes (e.g., 10 bare metals × 10 guests × 3 VLANs)
Goal: emulate 100 virtual nodes (10 per bare-metal Virtual Wall host), each with access to 3 shared VLANs (mgmt + 2 experiment VLANs), where nodes can reach each other by IP within each VLAN. Must scale to arbitrary VLAN counts and virtual nodes per host. Each VLAN is mapped to a distinct host interface (e.g., phys-if1→VLAN A, phys-if2→VLAN B, phys-if3→mgmt).

Simplified steps (keep it robust, reduce complexity):
1) **Explicit VLAN→interface mapping**  
   - Let the spec map VLAN IDs to host physical interfaces (e.g., `vlan_bindings: { 100: "eth1", 200: "eth2", 10: "eth0" }`).  
   - Create only the necessary VLAN subinterfaces on the mapped parent (no extra per-VLAN bridge unless needed). Attach host veths directly to the VLAN subinterface or a single bridge per parent to avoid bridge sprawl.

2) **Shared addressing per VLAN**  
   - Define per-VLAN subnets in the spec; auto-allocate IPs per virtual node per VLAN deterministically (`node-<host>-<idx>` gets one IP in each VLAN).  
   - Keep mgmt subnet separate and forbid experiment traffic over it.

3) **Routing per namespace**  
   - Set default routes per namespace to the per-VLAN gateway so multi-hop works out of the box.  
   - Ensure only experiment VLANs are used for data; mgmt VLAN is for control/ssh.

4) **Scaling knobs**  
   - Counts for VLANs and virtual nodes per host remain dynamic via pools. IP allocator and interface binding scale accordingly.

5) **Cleanup precision**  
   - Track created netns/veths/VLAN subinterfaces per plan; remove only those on cleanup.

6) **Observability**  
   - Expose virtual-node→VLAN/IP mapping and host VLAN subinterfaces in controller status; optional graph with VLAN coloring.

This keeps the design closer to VirtualWall semantics while spoofing a larger multi-VLAN topology, without unnecessary bridge layers.

## Goal alignment
BigVirtualWall is meant to behave like VirtualWall from the user’s perspective—start/stop, exec, nodes/links/status, visualize—while spoofing larger virtual topologies on top of the real experiment. It does **not** reconfigure physical switches; instead it adds VXLAN/VLAN overlays and per-interface impairments above the host mgmt network, keeping host connectivity intact. Use the pools to scale virtual nodes; use safety guards to avoid touching mgmt paths.
