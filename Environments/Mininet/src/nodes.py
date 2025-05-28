import re
from mininet.node import Node

class LinuxRouter(Node):

    # A Node with IP forwarding and multicast enabled
    def config(self, n_connections=None, **params):
        super(LinuxRouter, self).config(**params)

        print(f'Configuring LinuxRouter {self.name}')

        if n_connections is None:
            raise ValueError("Parameter 'n_connections' must be specified for LinuxRouter.")

        # Enable IP forwarding
        self.cmd('sysctl -w net.ipv4.ip_forward=1')
        self.cmd('sysctl -w net.ipv6.conf.all.forwarding=1')

        # Do not ignore ICMP echo requests that are broadcasted
        self.cmd('sysctl -w net.ipv4.icmp_echo_ignore_broadcasts=0')
        
        # Enable IGMPv2 and disable Reverse Path Filtering for all connected interfaces
        for intf in self.intfNames():
            self.cmd(f'sysctl -w net.ipv4.conf.{intf}.force_igmp_version=2')
            self.cmd(f'sysctl -w net.ipv4.conf.{intf}.rp_filter=0')

        # Start smcrouted daemon and add multicast routes for each connection
        self.cmd(f'smcrouted -l debug -I smcroute-{self.name}')
        self.cmd('sleep 1') # Wait for smcrouted to start
        for intf in self.intfNames():
            # Create a list of interfaces, excluding the current one
            l = [i for i in self.intfNames() if i != intf]
            # Get the digit of the interface (using regex)
            i = int(re.search(r'\d+', intf).group())
            # Join the multicast group
            self.cmd(f'smcroutectl -I smcroute-{self.name} add {intf} 239.0.{i}.1 {" ".join(l)}')
            # Get the ip address of the interface
            ip = self.intf(intf).IP()
            print(intf, ip)
            # All traffic for X.X.X.m should be routed through this interface
            # eg the ip is 11.0.1.0, then all traffic for 11.0.1.m should be routed through this interface
            # We can do this by adding a route for the /24 subnet
            subnet = '.'.join(ip.split('.')[:3] + ['0'])
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
        for intf in self.intfNames():
            self.cmd(f'sysctl -w net.ipv4.conf.{intf}.force_igmp_version=0')
            self.cmd(f'sysctl -w net.ipv4.conf.{intf}.rp_filter=1')


        # Stop smcrouted daemon for this route
        self.cmd(f'smcroutectl -I smcroute-{self.name} flush')
        self.cmd(f'smcroutectl -I smcroute-{self.name} kill')
        super(LinuxRouter, self).terminate()

class EdgeNode(Node):
    # A Node that supports multicast.
    def config(self, n_nodes=None, **params):
        super(EdgeNode, self).config(**params)
        print(f'Configuring EdgeNode {self.name}')

        if n_nodes is None:
            raise ValueError("Parameter 'n_nodes' must be specified for LinuxRouter.")

        # Get the node number
        nodeNumber = int(re.search(r'\d+', self.name).group())

        # Enable ICMP echo requests that are broadcasted
        self.cmd('sysctl net.ipv4.icmp_echo_ignore_broadcasts=0')
        # Add multicast route for the interface
        for intfName in self.intfNames():
            # Get the IP address of the interface
            ip = self.intf(intfName).IP()
            ip_first_part = ip.split('.')[0]
            router_ip = '.'.join([f'{ip_first_part}', '0', f'{nodeNumber}', '1'])
            # Get the router number
            router_number = int(ip_first_part) - 10
            # Add a route for the multicast group
            self.cmd(f'route add -net 239.0.{router_number}.0 netmask 255.255.255.0 dev {intfName}')
            # TODO: Check if the above route is necessary
        # Start smcrouted daemon and join the multicast group
        self.cmd(f'smcrouted -l debug -I smcroute-{self.name}')
        self.cmd('sleep 1') # Wait for smcrouted to start
        # Join the multicast group for each interface
        for intfName in self.intfNames():
            # Get the IP address of the interface
            ip = self.intf(intfName).IP()
            ip_first_part = ip.split('.')[0]
            router_ip = '.'.join([f'{ip_first_part}', '0', f'{nodeNumber}', '1'])
            # Get the router number
            router_number = int(ip_first_part) - 10
            # Join the multicast group
            self.cmd(f'smcroutectl -I smcroute-{self.name} join {intfName} 239.0.{router_number}.1')
            # Route all traffic for the multicast group through the interface
            self.cmd(f'ip route add 239.0.{router_number}.0/24 via {router_ip} dev {intfName}')
            # Generate routes for other subnets based on `n_nodes`
            for n in range(0, n_nodes + 1):
                if n == nodeNumber:
                    continue
                subnet = '.'.join([f'{ip_first_part}', '0', f'{n}', '0'])
                # Add route for each subnet
                self.cmd(f'ip route add {subnet}/24 via {router_ip} dev {intfName}')


        self.cmd(f'ip route add default via 11.0.{nodeNumber}.1')
            


    def terminate(self):
        # Stop smcrouted daemon route for this node
        self.cmd(f'smcroutectl -I smcroute-{self.name} flush')
        self.cmd(f'smcroutectl -I smcroute-{self.name} kill')

        # Undo the ICMP changes
        self.cmd('sysctl net.ipv4.icmp_echo_ignore_broadcasts=1')

        super(EdgeNode, self).terminate()