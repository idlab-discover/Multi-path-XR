# main.py

import json
import os
import random
import subprocess
import sys
import threading
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse
from mininet.clean import cleanup
from mininet.net import Mininet
from mininet.link import TCLink
from mininet.log import setLogLevel, info
from mininet.util import sysctlTestAndSet
from topology import NetworkTopo
import networkx as nx
import matplotlib.pyplot as plt
import matplotlib.colors as mcolors
from io import BytesIO
from PIL import Image

net = None # We store the Mininet network globaly.
lock = threading.Lock()  # Global lock for sequential processing

class SimpleRouter:
    """A simple router to handle path-based requests with HTTP method support."""

    def __init__(self):
        self.routes = {}

    def route(self, path, methods=["GET"]):
        """Decorator to register a route with specified HTTP methods."""
        def decorator(func):
            for method in methods:
                self.routes[(path, method)] = func
            return func
        return decorator

    def get_handler(self, path, method):
        """Retrieve the handler function for a given path and method."""
        return self.routes.get((path, method), None)

    def get_routes_info(self):
        """Returns a summary of all registered routes."""
        route_info = {}
        for (path, method), handler in self.routes.items():
            route_info.setdefault(path, []).append(method)
        return route_info

router = SimpleRouter()

class RequestHandler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1' # Required to support chunked responses

    def do_HEAD(self):
        """Serve a HEAD request."""
        parsed_path = urlparse(self.path)
        handler = router.get_handler(parsed_path.path, "GET")
        if not handler:
            self.send_response(404)
        else:
            self.send_response(200)
            #self.send_header("Content-Type", "application/json")
        self.send_header("Connection", "close") # We don't support persistent connections
        self.end_headers()

    def do_GET(self):
            self._handle_request("GET")

    def do_POST(self):
            self._handle_request("POST")

    def _handle_request(self, method):
        """Handle an HTTP request by finding the appropriate route handler."""
        parsed_path = urlparse(self.path)
        handler = router.get_handler(parsed_path.path, method)

        if handler:
            query_params = parse_qs(parsed_path.query)
            body = self._parse_body()
            try:
                with lock:
                    handler(self, query_params, body)
            except Exception as e:
                self._send_response(500, {"error": str(e)})
        else:
            self._send_response(404, {"error": "Not found"})

    def _parse_body(self):
        """Parse JSON body if present."""
        if 'Content-Length' in self.headers:
            length = int(self.headers['Content-Length'])
            body = self.rfile.read(length)
            try:
                return json.loads(body)
            except json.JSONDecodeError:
                return {}
        return {}

    def _send_response(self, code, message):
        """Send a JSON response."""
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Connection", "close") # We don't support persistent connections
        self.end_headers()
        self.wfile.write(json.dumps(message).encode("utf-8"))

    def _send_chunked_start(self):
        """Start a chunked response."""
        self.send_response(200)
        #self.send_header("Content-Type", "application/json")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close") # We don't support persistent connections
        self.end_headers()

    def _send_chunk(self, chunk):
        """Send a single chunk."""
        if not chunk or len(chunk) == 0:
            return # Skip empty chunks, as that would end the response
        self.wfile.write(f"{len(chunk):X}\r\n".encode("utf-8"))
        self.wfile.write(chunk.encode("utf-8"))
        self.wfile.write(b"\r\n")

    def _send_chunked_end(self):
        """End the chunked response."""
        self.wfile.write(b"0\r\n\r\n")

# Route definitions
@router.route("/start", methods=["GET"])
def start_network(request_handler=None, query_params=None, body=None) -> Mininet:
    """Start the Mininet network."""
    global net
    if net is not None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network already running"})
        return None

    try:
        info("Starting Mininet network")
        cleanup()
        info("Clean up done")

        # Convert query parameters to kwargs for NetworkTopo
        topo_kwargs = {key: int(value[0]) if value[0].isdigit() else value[0] for key, value in query_params.items()}

        topo = NetworkTopo(**topo_kwargs)
        info("Topology created with parameters:", topo_kwargs)
        net = Mininet(topo=topo, link=TCLink)
        net.start()
        info("Network started")

        sysctlTestAndSet( 'net.core.wmem_max', 67108864 )
        sysctlTestAndSet( 'net.core.wmem_default', 67108864 )
        sysctlTestAndSet( 'net.core.rmem_max', 67108864 )
        sysctlTestAndSet( 'net.core.rmem_default', 67108864 )
        sysctlTestAndSet( 'net.ipv4.tcp_rmem', '20480 349520 67108864' )
        sysctlTestAndSet( 'net.ipv4.tcp_wmem', '20480 349520 67108864' )
        sysctlTestAndSet( 'net.core.netdev_max_backlog', 20000 )

        info('*** Routing Table on NAT Router:\n')
        info(net['r1'].cmd('route'))

        # Get the number of nodes that start with 'nDIGIT'
        n_nodes = len([node for node in net.hosts if node.name.startswith('n') and node.name[1:].isdigit()])
        # Get the number of routers that start with 'rDIGIT' (excluding the NAT router [r1])
        n_routers = len([node for node in net.hosts if node.name.startswith('r') and node.name[1:].isdigit() and node.name != 'r1'])
        # Get the number of switches that start with 'sDIGIT'
        n_switches = len([node for node in topo.switches() if node.startswith('s') and node[1:].isdigit()])
        info(f"Number of nodes: {n_nodes}, routers: {n_routers}, switches: {n_switches}")
        
        nat = net['nat0']
        # Search for the interface that is connected to the NAT router
        nat_intf = [intf for intf in nat.intfList() if intf.IP().startswith('11.0.')][0]
        # We need to set the routes for the NAT router, we have to redirect all outside traffic to the nat router
        for n in range(1, n_nodes+1):
            info(nat.cmd(f'ip route add 11.0.{n}.0/24 via 11.0.{n_nodes+1}.1 dev {nat_intf}'))

        for n in range(1, n_routers+1):
            # We need to set the routes for the NAT router, we have to redirect all outside traffic to the nat router
            info(nat.cmd(f'ip route add 11.{10 + n + 1}.1.0/24 via 11.0.{n_nodes+1}.1 dev {nat_intf}'))

            # We need to set the other way around as well, so the routers know how to reach the nat router
            router = net[f'r{n+1}']
            router_to_nat_intf = [intf for intf in router.intfList() if intf.IP().startswith(f'11.{10 + n + 1}.')][0]
            info(router.cmd(f'ip route add 11.0.{n_nodes+1}.0/24 via 11.{10 + n + 1}.1.1 dev {router_to_nat_intf}'))

        # Make all the switches do L2 forwarding
        # We skip the first switch, as that is just to our NAT router
        for n in range(1, n_switches):
            switch = net[f's{n}']
            info(switch.cmd(f'ovs-ofctl add-flow {switch} " cookie=0x0, priority=0 actions=NORMAL" -O OpenFlow13'))
            

        if request_handler:
            request_handler._send_response(200, {"message": "Network started with HTTP server at 192.168.1.101:8080 and internet access via NAT"})
    except Exception as e:
        # Print the full traceback to the console
        traceback.print_exc()
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
    
    return net

@router.route("/stop", methods=["GET"])
def stop_network(request_handler=None, query_params=None, body=None) -> bool:
    """Stop the Mininet network."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network not running"})
        return True
    
    info("Stopping Mininet network")
    try:
        net.stop()
        cleanup()
        net = None
        if request_handler:
            request_handler._send_response(200, {"message": "Network stopped"})
        else:
            info("Network stopped")
        return True
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        else:
            info(f"Error stopping network: {str(e)}")
    
    return False

@router.route("/exec", methods=["GET"])
def execute_command(request_handler=None, query_params=None, body=None) -> bool:
    """Execute a command on a given node and stream output."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network is not running"})
        return False
    
    # Parse node and command from query parameters
    node_name = query_params.get("node", [None])[0]
    command = query_params.get("command", [None])[0]
    background = query_params.get("background", ["false"])[0].lower() == "true"

    info(f"Executing command '{command}' on node '{node_name}'")

    if not node_name or not command:
        if request_handler:
            request_handler._send_response(400, {"message": "Missing node or command parameter"})
        return False

    try:
        node = net.get(node_name)
        if background:
            # Ensure the command ends with '&' for background execution
            if not command.strip().endswith("&"):
                command += " &"

            # Execute the command
            node.cmd(command)

            if request_handler:
                request_handler._send_response(200, {"message": f"Background command executed on node '{node_name}'"})
            return True

        if request_handler:
            request_handler._send_chunked_start()

        # Execute command and stream output in chunks
        proc = node.popen(command, shell=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, universal_newlines=True)
        while True:
            output = proc.stdout.readline()
            if output and request_handler:
                request_handler._send_chunk(output)
            elif proc.poll() is not None:
                break

        # Send any remaining output from stderr
        err_output = proc.stderr.read()
        if err_output and request_handler:
            request_handler._send_chunk(err_output)

        if request_handler:
            request_handler._send_chunked_end()

        return True
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return False

@router.route("/endpoints", methods=["GET"])
def list_endpoints(request_handler=None, query_params=None, body=None) -> list:
    """List all registered endpoints with their methods."""
    routes_info = router.get_routes_info()
    formatted_routes = [{"path": path, "methods": methods} for path, methods in routes_info.items()]
    if request_handler:
        request_handler._send_response(200, formatted_routes)
    
    return formatted_routes

@router.route("/nodes", methods=["GET"])
def list_nodes(request_handler=None, query_params=None, body=None) -> list:
    """Lists the nodes in the network."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network not running"})
        return
    
    info("Getting all the nodes in the Mininet network")
    try:
        nodes = [{"name": node.name, "type": type(node).__name__} for node in net.values()]
        if request_handler:
            request_handler._send_response(200, nodes)
        return nodes
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return []

@router.route("/links", methods=["GET"])
def list_links(request_handler=None, query_params=None, body=None) -> list:
    """Lists the links in the network."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network not running"})
        return []
    
    info("Getting all the links in the Mininet network")
    try:
        links = []
        for link in net.links:
            # Retrieve IP addresses of both interfaces, if they exist
            ip1 = link.intf1.IP() if link.intf1.IP() is not None else "N/A"
            ip2 = link.intf2.IP() if link.intf2.IP() is not None else "N/A"
            links.append({
                "node1": link.intf1.node.name,
                "intf1": link.intf1.name,
                "ip1": ip1,
                "node2": link.intf2.node.name,
                "intf2": link.intf2.name,
                "ip2": ip2,
                "status": "up" if link.status() == "(OK OK)" else "down"
            })
        if request_handler:
            request_handler._send_response(200, links)
        return links
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return []

@router.route("/status", methods=["GET"])
def network_status(request_handler=None, query_params=None, body=None) -> dict:
    """Returns the status of the network."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(200, {"status": "stopped"})
        return {"status": "stopped"}
    
    try:
        # Basic network info
        nodes = list_nodes(None, query_params, body)
        links = list_links(None, query_params, body)

        status = {
            "status": "running",
            "nodes": nodes,
            "links": links,
            "node_count": len(nodes),
            "link_count": len(links),
        }

        if request_handler:
            request_handler._send_response(200, status)
        return status
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return {"status": "error"}

@router.route("/visualize", methods=["GET"])
def visualize_network(request_handler=None, query_params=None, body=None):
    """Generate a network visualization as an image."""
    # Retrieve the current network status
    status = network_status(None, query_params, body)
    
    # Ensure the network is running before attempting to visualize
    if status["status"] == "stopped":
        if request_handler:
            request_handler._send_response(400, {"message": "Network is not running"})
        return None
    
    try:
        # Create a NetworkX graph from nodes and links
        G = nx.Graph()
        
        # Add nodes with labels
        for node in status["nodes"]:
            G.add_node(node["name"], label=node["name"], type=node["type"])

        # Add edges between nodes
        for link in status["links"]:
            G.add_edge(
                link["node1"],
                link["node2"],
                intf1=link["intf1"],
                intf2=link["intf2"],
                status=link["status"]
            )

        node_size = 3000
        line_width = 2.5
        font_size = 10
        font_color = "white"
        font_weight = "bold"
        fig_width, fig_height = 24, 8

    
        # Define colors based on node type
        color_map = {
            "LinuxRouter": "orange",
            "EdgeNode": "skyblue",
            "NAT": "green",
            "OVSSwitch": "gray",
            "Controller": "purple",
        }
        
        # Assign colors to nodes based on their type
        node_colors = [color_map.get(G.nodes[node].get("type"), "black") for node in G.nodes()]

        # Find all routers (note that some switches may be called a router (for ease of use when detecting them in our project))
        router_nodes = [node["name"] for node in status["nodes"] if node.get("type") == "LinuxRouter" or node["name"].startswith("r")]
        # Force add controller to the router nodes
        router_nodes.insert(0, "c0")
        edge_nodes = [node["name"] for node in status["nodes"] if node.get("type") == "EdgeNode" and node["name"] not in router_nodes]
        # Add the NAT to the edge nodes
        edge_nodes.insert(0, "nat0")
        other_nodes = [node["name"] for node in status["nodes"] if node["name"] not in router_nodes + edge_nodes]


        # First, create a mapping of routers to switches they are connected to
        switch_groups = {router: [] for router in router_nodes}

        # Populate the switch_groups dictionary by iterating over links
        for link in status["links"]:
            if link["node1"] in router_nodes and link["node2"] in other_nodes:
                switch_groups[link["node1"]].append(link["node2"])
            elif link["node2"] in router_nodes and link["node1"] in other_nodes:
                switch_groups[link["node2"]].append(link["node1"])

        # Flatten the switch_groups in order of router_nodes to get sorted switches
        sorted_switches = []
        for router in router_nodes:
            sorted_switches.extend(switch_groups.get(router, []))

        # Update other_nodes with the sorted order
        other_nodes = sorted_switches + [node for node in other_nodes if node not in sorted_switches]

        # Generate distinct colors for each router’s links
        router_colors = list(mcolors.TABLEAU_COLORS.values())
        # random.shuffle(router_colors)

        # Map each router to a unique color
        router_link_colors = {router: router_colors[i % len(router_colors)] for i, router in enumerate(router_nodes)}

        # Initialize edge color map
        edge_colors = ["black"] * len(G.edges())

        sorted_edges = [tuple(sorted(edge)) for edge in G.edges()]

        # Identify branches and apply the router's color
        for router, color in router_link_colors.items():
            visited = set()

            # Perform BFS to traverse each router's branches
            queue = [(router, None)]
            while queue:
                current, prev_edge = queue.pop(0)
                neighbors = G.neighbors(current)

                for neighbor in neighbors:
                    if neighbor not in visited:
                        visited.add(neighbor)
                        edge = tuple(sorted((current, neighbor))) 
                        reverse_edge = (neighbor, current)

                        # Safely find the edge index
                        try:
                            edge_index = sorted_edges.index(edge)
                            # Apply color to the router's branch
                            edge_colors[edge_index] = color
                        except ValueError:
                            # Skip coloring if the edge is not found
                            info(f"Edge {edge} not found")
                            continue

                        # Apply color to the router's branch
                        edge_colors[edge_index] = color

                        # Continue traversal if it's not reaching an EdgeNode
                        if G.nodes[neighbor]["type"] == "OVSSwitch":
                            queue.append((neighbor, edge))

        edge_styles = [
            "solid" if G.edges[edge]["status"] == "up" else "dashed"
            for edge in G.edges
        ]

        # Create the visualization
        plt.figure(figsize=(fig_width, fig_height))

        # Assign positions
        pos = {}

        def evenly_spaced_positions(nodes, y_pos):
            """Generate x positions for nodes evenly spaced between 0 and 1."""
            count = len(nodes)
            if count == 1:
                return {nodes[0]: (0.5, y_pos)}
            return {node: ((i + 1) / (count + 1), y_pos) for i, node in enumerate(nodes)}

        # Position routers in the center row (y=0)
        pos.update(evenly_spaced_positions(router_nodes, y_pos=0))
        # Position EdgeNodes in the lower row (y=-2)
        pos.update(evenly_spaced_positions(edge_nodes, y_pos=-2))
        # Position other nodes (e.g., switches) in the row between routers and EdgeNodes (y=-1)
        pos.update(evenly_spaced_positions(other_nodes, y_pos=-1))

        # Create edge labels with IP information
        edge_labels = {}
        for link in status["links"]:
            node1 = link.get("node1", "N/A")
            ip1 = link.get("ip1", "N/A")
            node2 = link.get("node2", "N/A")
            ip2 = link.get("ip2", "N/A")
            edge_key = tuple(sorted((link["node1"], link["node2"])))
            if ip1 != "N/A" and ip2 != "N/A":
                edge_labels[edge_key] = f"{node1}:{ip1} <-> {node2}:{ip2}"
            elif ip1 != "N/A":
                edge_labels[edge_key] = f"{node1}:{ip1}"
            elif ip2 != "N/A":
                edge_labels[edge_key] = f"{node2}:{ip2}"
            else:
                edge_labels[edge_key] = ""

        # Draw nodes and edges with color mapping
        nx.draw(G, pos, with_labels=True, node_size=700, font_size=10, font_color="white", font_weight="bold", node_color=node_colors, edge_color=edge_colors, width=line_width, style=edge_styles)
        nx.draw_networkx_edge_labels(G, pos, edge_labels=edge_labels, font_size=10, font_color="gray")
        
        # Save the visualization to a PNG image in memory
        buffer = BytesIO()
        plt.savefig(buffer, format="png")
        buffer.seek(0)
        
        # Send the image as a response
        if request_handler:
            request_handler.send_response(200)
            request_handler.send_header("Content-Type", "image/png")
            request_handler.send_header("Connection", "close") # We don't support persistent connections
            request_handler.end_headers()
            request_handler.wfile.write(buffer.getvalue())

        # Close the plot to free up memory
        plt.close()
    except Exception as e:
        # Print the full traceback to the console
        traceback.print_exc()
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})

@router.route("/start_xterm", methods=["GET"])
def start_xterm(request_handler=None, query_params=None, body=None):
    """Start an X terminal (xterm) for a given node."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network is not running"})
        return False

    # Get the node name from query parameters
    node_name = query_params.get("node", [None])[0]
    if not node_name:
        if request_handler:
            request_handler._send_response(400, {"message": "Missing 'node' parameter"})
        return False

    try:
        # Retrieve the node from the Mininet network
        node = net.get(node_name)
        # Start an xterm terminal for the node
        node.cmd("xterm -ls -xrm 'XTerm*selectToClipboard: true' &")

        if request_handler:
            request_handler._send_response(200, {"message": f"X terminal started for node '{node_name}'"})
        return True
    except KeyError:
        if request_handler:
            request_handler._send_response(404, {"error": f"Node '{node_name}' not found"})
        return False
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return False

@router.route("/ping_all", methods=["GET"])
def ping_all_interfaces(request_handler=None, query_params=None, body=None):
    """Ping between all possible interfaces on all hosts and return the results."""
    global net
    if net is None:
        if request_handler:
            request_handler._send_response(400, {"message": "Network is not running"})
        return False

    try:
        # Collect all hosts in the network
        hosts = [host for host in net.hosts]
        ping_results = {}

        # Ping between all pairs of interfaces on different hosts
        for i, src_host in enumerate(hosts):
            src_interfaces = [intf for intf in src_host.intfList() if intf.IP() is not None]
            for j, dst_host in enumerate(hosts):
                if src_host == dst_host:
                    continue

                dst_interfaces = [intf for intf in dst_host.intfList() if intf.IP() is not None]

                for src_intf in src_interfaces:
                    for dst_intf in dst_interfaces:
                        src_ip = src_intf.IP()
                        dst_ip = dst_intf.IP()

                        # If source and ip do not start with the same number, skip
                        # Excpet if one of them is the NAT or starts with 192
                        src_ip_start = src_ip.split(".")[0]
                        dst_ip_start = dst_ip.split(".")[0]
                        if src_ip_start != dst_ip_start and src_ip_start != "192" and dst_ip_start != "192" and src_host.name != "nat0" and dst_host.name != "nat0":
                            continue

                        # Run ping command and capture output
                        result = src_host.cmd(f"ping -R -c 1 {dst_ip}")

                        info(f"Pinged from {src_host.name}({src_ip}) to {dst_host.name}({dst_ip})")
                        print(result)
                        
                        # Parse result to determine success or failure
                        success = "1 packets transmitted, 1 received" in result
                        result_key = f"{src_host.name}({src_ip}) -> {dst_host.name}({dst_ip})"

                        ping_results[result_key] = {
                            "ping": "Success" if success else "Failure",
                        }

                        
                            
                        # Store the traceroute path in the results
                        #ping_results[result_key]["traceroute"] = traceroute_hops


        # Send results as JSON response
        if request_handler:
            request_handler._send_response(200, ping_results)
        return True
    except Exception as e:
        if request_handler:
            request_handler._send_response(500, {"error": str(e)})
        return False

def run_server(server_class=HTTPServer, handler_class=RequestHandler, port=5000):
    server_address = ('', port)
    httpd = server_class(server_address, handler_class)
    info(f'Starting HTTP server on port {port}...')

    try:
        httpd.serve_forever()
    finally:
        info("Shutting down HTTP server and stopping network...")
        with lock:
            stop_network()  # Ensure the network is stopped on shutdown

def check_smcroute():
    try:
        # Check if smcrouted is available on the system by calling `which`
        subprocess.run(["which", "smcroutectl"], check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return True
    except subprocess.CalledProcessError:
        return False

if __name__ == "__main__":
    if os.geteuid() != 0:
        exit("This script needs to run with root privileges!")

    # Check if smcroute is installed
    if not check_smcroute():
        exit("This script requires the smcroute package to be installed. Please install it using your package manager, e.g., `sudo apt-get install smcroute` on Debian-based systems.")

    setLogLevel('info')
    run_server()
