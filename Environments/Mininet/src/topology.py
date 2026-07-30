#!/usr/bin/python

from pathlib import Path
import sys

from mininet.topo import Topo
from mininet.nodelib import NAT

from nodes import LinuxRouter, EdgeNode


REPO_ROOT = Path(__file__).resolve().parents[3]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from topology_data.geant import (  # noqa: E402
    GEANT_CITY_CODES as SHARED_GEANT_CITY_CODES,
    GEANT_LINKS as SHARED_GEANT_LINKS,
    geant_selected_links,
    parse_geant_cities,
)


class NetworkTopo(Topo):
    # Mininet TCLink bw is expressed in Mbit/s.
    # 1 Gbit/s = 1000 Mbit/s.
    MIN_LINK_BW_MBIT = 1_000

    GEANT_CITY_CODES = list(SHARED_GEANT_CITY_CODES)
    GEANT_LINKS = list(SHARED_GEANT_LINKS)

    def addLink(self, node1, node2, *args, **kwargs):
        """Apply the experiment-wide capacity to links without an explicit bandwidth."""
        requested_bw = kwargs.get('bw')
        if requested_bw is None:
            kwargs['bw'] = getattr(self, 'default_link_bw_mbit', self.MIN_LINK_BW_MBIT)

        return super().addLink(node1, node2, *args, **kwargs)

    # Simplified topology with a server, router, and client
    def build(
        self,
        n_nodes=2,
        n_paths=2,
        topology='basic',
        geant_cities=None,
        default_link_bw_mbit=MIN_LINK_BW_MBIT,
        **params,
    ):
        try:
            default_link_bw_mbit = float(default_link_bw_mbit)
        except (TypeError, ValueError) as exc:
            raise ValueError("Parameter 'default_link_bw_mbit' must be numeric.") from exc
        if default_link_bw_mbit < self.MIN_LINK_BW_MBIT:
            default_link_bw_mbit = self.MIN_LINK_BW_MBIT
        self.default_link_bw_mbit = default_link_bw_mbit

        mode = str(topology).lower().strip()
        if mode == 'geant':
            self._build_geant(n_nodes=n_nodes, geant_cities=geant_cities, **params)
            return

        self._build_basic(n_nodes=n_nodes, n_paths=n_paths, **params)

    def _build_basic(self, n_nodes=2, n_paths=2, **params):
        self.topology_mode = 'basic'

        if n_nodes is None:
            raise ValueError("Parameter 'n_nodes' must be specified.")

        n_connections = n_nodes

        # Store the routers in a list for easy access
        routers = []
        switch_count = 0

        # NAT connection for internet access
        nat = self.addHost('nat0', cls=NAT, ip=f'11.0.{n_nodes+1}.2', subnet='11.0/8', inNamespace=False)
        # Router for the NAT ( we only add the link later, so we first define the ip of to the first node)
        nat_router = self.addHost(
            'r1',
            cls=LinuxRouter,
            ip=f'11.0.{n_nodes+1}.1/24',
            defaultRoute=f'via 11.0.{n_nodes+1}.2',
            n_connections=n_connections,
            multicast_enabled=False,
        )
        # Store the NAT router in our list
        routers.append(nat_router)
        # NAT Switch connected to NAT node and NAT router for internet access
        nat_switch = self.addSwitch(f's{switch_count}')
        switch_count += 1
        self.addLink(nat, nat_switch) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
        # Attach r1 to the NAT switch before any edge-node links so Mininet's
        # host-level default IP lands on the NAT-facing interface rather than
        # clobbering the first node-facing control-plane link.
        self.addLink(nat_router, nat_switch, params1={'ip':f'11.0.{n_nodes+1}.1/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link

        # Create one router per path
        for i in range(1, n_paths+1): # We start at 1 because we already have a router for the NAT
            router = self.addHost(
                f'r{i+1}',
                cls=LinuxRouter,
                ip=f'{10 + 1 + i}.0.1.1/24',
                n_connections=n_connections,
                multicast_enabled=(i == 1),
            )
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

        # Now, also add a switch between each router and the NAT router
        for i, router in enumerate(routers):
            if i == 0:
                continue
            switch = self.addSwitch(f's{switch_count}')
            switch_count += 1
            self.addLink(router, switch, params1={'ip':f'11.{10 + 1 + i}.1.2/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link
            self.addLink(switch, nat_router, params2={'ip':f'11.{10 + 1 + i}.1.1/24'}) #, bw=4000, max_queue_size=5000, use_hfsc=True)   # 4000 Mbps link

    def _build_geant(self, n_nodes=31, geant_cities=None, **params):
        self.topology_mode = 'geant'

        max_nodes = len(self.GEANT_CITY_CODES)
        if n_nodes is None:
            n_nodes = max_nodes
        selected_cities = parse_geant_cities(geant_cities, default_count=n_nodes)
        n_nodes = len(selected_cities)
        if n_nodes <= 0:
            raise ValueError("Parameter 'n_nodes' must be greater than zero.")
        if n_nodes > max_nodes:
            raise ValueError(f"GEANT supports up to {max_nodes} nodes.")

        n_connections = n_nodes
        city_to_node_index = {city: idx + 1 for idx, city in enumerate(selected_cities)}

        # Controller/NAT edge and controller router.
        nat = self.addHost('nat0', cls=NAT, ip=f'11.0.{n_nodes+1}.2', subnet='11.0/8', inNamespace=False)
        nat_router = self.addHost(
            'r1',
            cls=LinuxRouter,
            ip=f'11.0.{n_nodes+1}.1/24',
            defaultRoute=f'via 11.0.{n_nodes+1}.2',
            n_connections=n_connections,
            multicast_enabled=False,
        )

        # Dedicated multicast backbone router, separate from the GEANT unicast mesh.
        multicast_router = self.addHost(
            'rmc',
            cls=LinuxRouter,
            ip='11.254.1.2/24',
            n_connections=n_connections,
            multicast_enabled=True,
        )

        # NAT switch between nat0 and r1.
        nat_switch = self.addSwitch('s0')
        self.addLink(nat, nat_switch)
        self.addLink(nat_router, nat_switch, params1={'ip':f'11.0.{n_nodes+1}.1/24'})

        # Management link so rmc can reach controller services via the controller/NAT plane.
        self.addLink(
            multicast_router,
            nat_router,
            params1={'ip':'11.254.1.2/24'},
            params2={'ip':'11.254.1.1/24'},
        )

        # Create one GEANT router and one edge node per selected city.
        geant_routers = {}
        edge_nodes = {}
        for idx, city in enumerate(selected_cities, start=1):
            router_name = f'r{idx+1}'
            router_uplink_subnet_octet = 11 + idx
            geant_routers[city] = self.addHost(
                router_name,
                cls=LinuxRouter,
                ip=f'11.{router_uplink_subnet_octet}.1.2/24',
                n_connections=n_connections,
                multicast_enabled=False,
            )

            edge_node = self.addHost(
                f'n{idx}',
                cls=EdgeNode,
                ip=f'11.0.{idx}.2/24',
                defaultRoute=f'via 11.0.{idx}.1',
                n_nodes=n_nodes,
            )
            edge_nodes[city] = edge_node

            # Controller plane via r1, matching the basic topology's eth0 semantics.
            self.addLink(
                edge_node,
                nat_router,
                params1={'ip':f'11.0.{idx}.2/24'},
                params2={'ip':f'11.0.{idx}.1/24'},
            )

            # Separate multicast plane via the shared multicast router, matching the
            # basic topology's eth1 semantics.
            self.addLink(
                edge_node,
                multicast_router,
                params1={'ip':f'12.0.{idx}.2/24'},
                params2={'ip':f'12.0.{idx}.1/24'},
            )

        # Add the controller-plane uplink first on every GEANT router so it becomes
        # eth0. This keeps the per-router numbering stable for tc configuration.
        for idx, city in enumerate(selected_cities, start=1):
            subnet_octet = 11 + idx
            self.addLink(
                geant_routers[city],
                nat_router,
                params1={'ip':f'11.{subnet_octet}.1.2/24'},
                params2={'ip':f'11.{subnet_octet}.1.1/24'},
            )

        # Attach each node to its local GEANT router after the router uplink so the
        # node-facing link becomes eth1, like the basic star topology's router-node links.
        for idx, city in enumerate(selected_cities, start=1):
            self.addLink(
                edge_nodes[city],
                geant_routers[city],
                params1={'ip':f'13.0.{idx}.2/24'},
                params2={'ip':f'13.0.{idx}.1/24'},
            )

        # Build the GEANT backbone links.
        geant_router_links = []
        link_id = 1
        for city_a, city_b in geant_selected_links(selected_cities):
            self.addLink(
                geant_routers[city_a],
                geant_routers[city_b],
                params1={'ip':f'10.200.{link_id}.1/30'},
                params2={'ip':f'10.200.{link_id}.2/30'},
            )
            geant_router_links.append(
                (
                    f"r{city_to_node_index[city_a] + 1}",
                    f"r{city_to_node_index[city_b] + 1}",
                )
            )
            link_id += 1

        # Metadata used by runtime route provisioning in app.py.
        self.geant_node_count = n_nodes
        self.geant_selected_cities = selected_cities
        self.geant_router_links = geant_router_links
