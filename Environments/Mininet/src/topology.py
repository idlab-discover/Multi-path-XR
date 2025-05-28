#!/usr/bin/python

from mininet.topo import Topo
from mininet.node import Node
from mininet.nodelib import NAT

from nodes import LinuxRouter, EdgeNode

class NetworkTopo(Topo):
    # Simplified topology with a server, router, and client
    def build(self, n_nodes=2, n_paths=2, **params):

        if n_nodes is None:
            raise ValueError("Parameter 'n_nodes' must be specified.")

        n_connections = n_nodes

        # Store the routers in a list for easy access
        routers = []
        switch_count = 0

        # NAT connection for internet access
        nat = self.addHost('nat0', cls=NAT, ip=f'11.0.{n_nodes+1}.2', subnet='11.0/8', inNamespace=False)
        # Router for the NAT ( we only add the link later, so we first define the ip of to the first node)
        nat_router = self.addHost('r1', cls=LinuxRouter, ip=f'11.0.1.1/24', defaultRoute=f'via 11.0.{n_nodes+1}.2', n_connections=n_connections)
        # Store the NAT router in our list
        routers.append(nat_router)
        # NAT Switch connected to NAT node and NAT router for internet access
        nat_switch = self.addSwitch(f's{switch_count}')
        switch_count += 1
        self.addLink(nat, nat_switch) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link

        # Create one router per path
        for i in range(1, n_paths+1): # We start at 1 because we already have a router for the NAT
            router = self.addHost(f'r{i+1}', cls=LinuxRouter, ip=f'{10 + 1 + i}.0.1.1/24', n_connections=n_connections)
            routers.append(router)

        # Create EdgeNodes, each connected to their own switch
        for i in range(1, n_nodes+1):
            edge_node = self.addHost(f'n{i}', cls=EdgeNode, ip=f'11.0.{i}.2/24', defaultRoute='via 11.0.{i}.1', n_nodes=n_nodes)

            # Iterate over the routers and connect the edge node to each of them
            for j, router in enumerate(routers):
                # Create a switch to connect the edge node to the router
                switch = self.addSwitch(f's{switch_count}')
                switch_count += 1
                edge_ip = f'{10 + 1 + j}.0.{i}.2'
                router_ip = f'{10 + 1 + j}.0.{i}.1'
                self.addLink(edge_node, switch, params1={'ip':f'{edge_ip}/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
                self.addLink(switch, router, params2={'ip':f'{router_ip}/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
                #self.addLink(switch, router)

        # Finally, link the nat switch to the nat router
        self.addLink(nat_router, nat_switch, params1={'ip':f'11.0.{n_nodes+1}.1/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link

        # Now, also add a switch between each router and the NAT router
        for i, router in enumerate(routers):
            if i == 0:
                continue
            switch = self.addSwitch(f's{switch_count}')
            switch_count += 1
            self.addLink(router, switch, params1={'ip':f'11.{10 + 1 + i}.1.2/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
            self.addLink(switch, nat_router, params2={'ip':f'11.{10 + 1 + i}.1.1/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
            