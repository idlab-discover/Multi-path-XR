import re
from mininet.node import Node


def _ip_octets(ip: str) -> list[int]:
    return [int(part) for part in ip.split('.')]


def _ip_first_octet(ip: str) -> int:
    return _ip_octets(ip)[0]


def _is_legacy_geant_multicast_ip(ip: str) -> bool:
    return ip.startswith('172.31.')


def _is_node_facing_edge_plane_ip(ip: str) -> bool:
    octets = _ip_octets(ip)
    return len(octets) == 4 and octets[1] == 0


def _is_reserved_multicast_plane_ip(ip: str) -> bool:
    return _is_node_facing_edge_plane_ip(ip) and _ip_first_octet(ip) == 12


def _subnet_from_ip(ip: str) -> str:
    return '.'.join(ip.split('.')[:3] + ['0'])


def _router_ip_for_plane(ip: str, node_number: int) -> str:
    if _is_legacy_geant_multicast_ip(ip):
        return f'172.31.{node_number}.1'
    return f'{_ip_first_octet(ip)}.0.{node_number}.1'


def _sorted_interface_names_by_ip(node: Node) -> list[str]:
    return sorted(node.intfNames(), key=lambda name: tuple(_ip_octets(node.intf(name).IP())))


def _join_multicast_groups(node: Node, intf_name: str, router_ip: str, group_indices) -> None:
    daemon_name = f'smcroute-{node.name}'
    for group_idx in group_indices:
        node.cmd(f'smcroutectl -I {daemon_name} join {intf_name} 239.0.{group_idx}.1')
        node.cmd(f'ip route replace 239.0.{group_idx}.0/24 via {router_ip} dev {intf_name}')

class LinuxRouter(Node):

    # A Node with IP forwarding and multicast enabled
    def config(self, n_connections=None, multicast_enabled=True, **params):
        super(LinuxRouter, self).config(**params)

        print(f'Configuring LinuxRouter {self.name}')
        daemon_name = f'smcroute-{self.name}'

        if n_connections is None:
            raise ValueError("Parameter 'n_connections' must be specified for LinuxRouter.")

        # Enable IP forwarding
        self.cmd('sysctl -w net.ipv4.ip_forward=1')
        self.cmd('sysctl -w net.ipv6.conf.all.forwarding=1')

        self.multicast_enabled = multicast_enabled

        # Do not ignore ICMP echo requests that are broadcasted
        self.cmd('sysctl -w net.ipv4.icmp_echo_ignore_broadcasts=0')

        interface_names = list(self.intfNames())

        if multicast_enabled:
            # Enable IGMPv2 and disable Reverse Path Filtering for all connected interfaces.
            for intf in interface_names:
                self.cmd(f'sysctl -w net.ipv4.conf.{intf}.force_igmp_version=2')
                self.cmd(f'sysctl -w net.ipv4.conf.{intf}.rp_filter=0')

            # Start smcrouted daemon and add multicast routes for each connection.
            self.cmd(f'smcrouted -l debug -I {daemon_name}')
            self.cmd('sleep 1') # Wait for smcrouted to start

        for intf in interface_names:
            # Get the ip address of the interface
            ip = self.intf(intf).IP()
            print(intf, ip)

            if multicast_enabled and (
                _is_legacy_geant_multicast_ip(ip) or _is_node_facing_edge_plane_ip(ip)
            ):
                group_idx = _ip_octets(ip)[2]
                other_interfaces = [name for name in interface_names if name != intf]
                self.cmd(
                    f'smcroutectl -I {daemon_name} add {intf} 239.0.{group_idx}.1 {" ".join(other_interfaces)}'
                )

            # All traffic for X.X.X.m should be routed through this interface
            # eg the ip is 11.0.1.0, then all traffic for 11.0.1.m should be routed through this interface
            # We can do this by adding a route for the /24 subnet
            subnet = _subnet_from_ip(ip)
            self.cmd('route add %s/24 dev %s' % (subnet, intf))

        # Accept everything
        self.cmd('iptables -A INPUT -j ACCEPT')
        self.cmd('iptables -A FORWARD -j ACCEPT')
        self.cmd('iptables -A OUTPUT -j ACCEPT')

    def terminate(self):
        # Disable IP forwarding
        self.cmd('sysctl -w net.ipv4.ip_forward=0')
        self.cmd('sysctl -w net.ipv6.conf.all.forwarding=0')

        # Undo the ICMP, GMP and RPF changes
        self.cmd('sysctl -w net.ipv4.icmp_echo_ignore_broadcasts=1')
        daemon_name = f'smcroute-{self.name}'
        for intf in self.intfNames():
            if getattr(self, 'multicast_enabled', True):
                self.cmd(f'sysctl -w net.ipv4.conf.{intf}.force_igmp_version=0')
                self.cmd(f'sysctl -w net.ipv4.conf.{intf}.rp_filter=1')

        # Stop smcrouted daemon for this router when multicast is enabled.
        if getattr(self, 'multicast_enabled', True):
            self.cmd(f'smcroutectl -I {daemon_name} flush')
            self.cmd(f'smcroutectl -I {daemon_name} kill')
        super(LinuxRouter, self).terminate()

class EdgeNode(Node):
    # A Node that supports multicast.
    def config(self, n_nodes=None, **params):
        super(EdgeNode, self).config(**params)
        print(f'Configuring EdgeNode {self.name}')
        daemon_name = f'smcroute-{self.name}'

        if n_nodes is None:
            raise ValueError("Parameter 'n_nodes' must be specified for EdgeNode.")

        # Get the node number
        node_number = int(re.search(r'\d+', self.name).group())

        # Enable ICMP echo requests that are broadcasted
        self.cmd('sysctl net.ipv4.icmp_echo_ignore_broadcasts=0')
        interface_names = _sorted_interface_names_by_ip(self)
        interface_ips = {name: self.intf(name).IP() for name in interface_names}
        legacy_multicast_intfs = [
            name for name in interface_names if _is_legacy_geant_multicast_ip(interface_ips[name])
        ]
        routed_intfs = [name for name in interface_names if name not in legacy_multicast_intfs]

        # Configure unicast routes:
        # - single-plane basic: default and node routes stay on 11.*
        # - current GEANT/basic multipath: keep one routing table entry per plane prefix
        uses_plane_specific_links = len(routed_intfs) > 1 and not legacy_multicast_intfs
        if routed_intfs:
            if uses_plane_specific_links:
                for routed_intf in routed_intfs:
                    ip = interface_ips[routed_intf]
                    ip_first_part = ip.split('.')[0]
                    router_ip = _router_ip_for_plane(ip, node_number)

                    for n in range(1, n_nodes + 1):
                        if n == node_number:
                            continue
                        self.cmd(f'ip route add {ip_first_part}.0.{n}.0/24 via {router_ip} dev {routed_intf}')

                # GEANT proxy-to-origin connections can arrive from router backbone
                # addresses (10.200.x.y); return those via the local GEANT router.
                for routed_intf in routed_intfs:
                    ip = interface_ips[routed_intf]
                    if ip.startswith('13.0.'):
                        router_ip = _router_ip_for_plane(ip, node_number)
                        self.cmd(f'ip route replace 10.200.0.0/16 via {router_ip} dev {routed_intf}')
                        break

                # Keep default route over the primary path (11.*), which is first in basic topology.
                primary_ip = interface_ips[routed_intfs[0]]
                primary_first = primary_ip.split('.')[0]
                self.cmd(f'ip route replace default via {primary_first}.0.{node_number}.1 dev {routed_intfs[0]}')
            else:
                routed_intf = routed_intfs[0]
                router_ip = f'11.0.{node_number}.1'
                for n in range(1, n_nodes + 1):
                    if n == node_number:
                        continue
                    self.cmd(f'ip route add 11.0.{n}.0/24 via {router_ip} dev {routed_intf}')

                self.cmd(f'ip route replace default via {router_ip} dev {routed_intf}')

        # Start smcrouted daemon and join the multicast group
        self.cmd(f'smcrouted -l debug -I {daemon_name}')
        self.cmd('sleep 1') # Wait for smcrouted to start

        reserved_multicast_intfs = [
            name for name in routed_intfs if _is_reserved_multicast_plane_ip(interface_ips[name])
        ]

        # Explicit multicast links take precedence. If the current topology exposes
        # multiple numbered planes but no dedicated marker, preserve the old fallback
        # of reserving the first non-control plane for multicast.
        if legacy_multicast_intfs:
            mcast_targets = legacy_multicast_intfs
            join_all_groups = True
        elif reserved_multicast_intfs:
            mcast_targets = reserved_multicast_intfs
            join_all_groups = True
        elif uses_plane_specific_links:
            mcast_targets = routed_intfs[1:2]
            join_all_groups = True
        else:
            mcast_targets = interface_names
            join_all_groups = False

        for intf_name in mcast_targets:
            ip = interface_ips[intf_name]
            router_ip = _router_ip_for_plane(ip, node_number)
            if join_all_groups:
                group_indices = range(1, n_nodes + 1)
            else:
                group_indices = [_ip_first_octet(ip) - 10]

            _join_multicast_groups(self, intf_name, router_ip, group_indices)


    def terminate(self):
        # Stop smcrouted daemon route for this node
        daemon_name = f'smcroute-{self.name}'
        self.cmd(f'smcroutectl -I {daemon_name} flush')
        self.cmd(f'smcroutectl -I {daemon_name} kill')

        # Undo the ICMP changes
        self.cmd('sysctl net.ipv4.icmp_echo_ignore_broadcasts=1')

        super(EdgeNode, self).terminate()
