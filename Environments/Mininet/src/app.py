# main.py

import json
import heapq
import math
import os
import signal
import subprocess
import sys
import threading
import time
import traceback
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs, urlparse
from mininet.clean import cleanup
from mininet.net import Mininet
from mininet.link import TCLink
from mininet.log import setLogLevel, info as mn_info, error as mn_error, lg as mn_lg, StreamHandlerNoNewline
import logging
from mininet.util import sysctlTestAndSet
from topology import NetworkTopo
import networkx as nx
import matplotlib.pyplot as plt
import matplotlib.colors as mcolors
from io import BytesIO

# split Mininet logs across stdout/stderr
def _route_mininet_logs():
    class _AboveWarning(logging.Filter):
        def filter(self, record):
            return record.levelno > logging.WARNING

    for h in list(mn_lg.handlers):
        if isinstance(h, StreamHandlerNoNewline) and getattr(h, 'stream', None) is sys.stderr:
            h.addFilter(_AboveWarning())

    # add a new handler that sends DEBUG/INFO/OUTPUT to stdout
    h_out = StreamHandlerNoNewline(stream=sys.stdout)
    h_out.setFormatter(logging.Formatter('%(message)s'))

    class _BelowOrEqualWarning(logging.Filter):
        def filter(self, record):
            # DEBUG(10), INFO(20), OUTPUT(25) < WARNING(30)
            return record.levelno <= logging.WARNING

    h_out.addFilter(_BelowOrEqualWarning())
    h_out.setLevel(logging.DEBUG)  # allow DEBUG/INFO/OUTPUT through
    mn_lg.addHandler(h_out)
    # avoid accidental duplication via root logger
    mn_lg.propagate = False

_route_mininet_logs()

def info(msg: str, *args) -> None:
    """Log info messages with Mininet's info function, adding a newline only when needed."""
    text = msg if msg.endswith("\n") else msg + "\n"
    mn_info(text, *args)

def error(msg: str, *args) -> None:
    """Log error messages with Mininet's error function, adding a newline only when needed."""
    text = msg if msg.endswith("\n") else msg + "\n"
    mn_error(text, *args)

net: Mininet = None # We store the Mininet network globaly.
lock = threading.Lock()  # Global lock for sequential processing
_BG_PROCS = {}  # pid -> subprocess.Popen
_BG_GROUPS = {}  # pgid -> set of pids (the process group leaders we created)


def _safe_add_route(node, route_cmd: str) -> None:
    """Install a route but avoid startup aborts if it already exists."""
    result = node.cmd(route_cmd)
    if result and 'File exists' not in result:
        info(result)


def _build_router_link_graph(net: Mininet, router_names: set[str]) -> dict[str, list[dict[str, str]]]:
    """Build an adjacency list with interface/next-hop metadata for router-to-router links."""
    graph: dict[str, list[dict[str, str]]] = {name: [] for name in router_names}

    for link in net.links:
        if link is None:
            continue

        a = link.intf1.node.name
        b = link.intf2.node.name
        if a not in router_names or b not in router_names:
            continue

        graph[a].append({
            'neighbor': b,
            'dev': link.intf1.name,
            'next_hop': link.intf2.IP(),
        })
        graph[b].append({
            'neighbor': a,
            'dev': link.intf2.name,
            'next_hop': link.intf1.IP(),
        })

    return graph


def _shortest_path_next_hop(graph: dict[str, list[dict[str, str]]], src: str, dst: str):
    """Return (dev, next_hop) to reach dst from src using shortest path in hop count."""
    if src == dst:
        return None

    dist = {src: 0}
    prev = {}
    queue = [(0, src)]

    while queue:
        d, node = heapq.heappop(queue)
        if node == dst:
            break
        if d > dist.get(node, float('inf')):
            continue

        for edge in graph.get(node, []):
            neighbor = edge['neighbor']
            nd = d + 1
            if nd < dist.get(neighbor, float('inf')):
                dist[neighbor] = nd
                prev[neighbor] = (node, edge)
                heapq.heappush(queue, (nd, neighbor))

    if dst not in prev:
        return None

    cursor = dst
    while prev[cursor][0] != src:
        cursor = prev[cursor][0]

    first_edge = prev[cursor][1]
    return (first_edge['dev'], first_edge['next_hop'])


def _is_truthy(value) -> bool:
    if isinstance(value, bool):
        return value
    if value is None:
        return False
    return str(value).strip().lower() in {'1', 'true', 'yes', 'on'}


def _parse_geant_hop_weights(raw: str | None) -> dict[int, int]:
    """Parse hop->weight map from '1:16,2:8,3:4' style config."""
    default_weights = {1: 16, 2: 8, 3: 4, 4: 2, 5: 1}
    if raw is None:
        return default_weights

    parsed: dict[int, int] = {}
    for token in str(raw).split(','):
        token = token.strip()
        if not token or ':' not in token:
            continue
        hop_part, weight_part = token.split(':', 1)
        try:
            hop = int(hop_part.strip())
            weight = int(weight_part.strip())
        except ValueError:
            continue
        if hop <= 0 or weight <= 0:
            continue
        parsed[hop] = weight

    return parsed if parsed else default_weights


def _shortest_distances_to_dst(graph: dict[str, list[dict[str, str]]], dst: str) -> dict[str, int]:
    """Compute hop count from every router to dst in an unweighted graph."""
    dist: dict[str, int] = {dst: 0}
    queue: list[str] = [dst]
    idx = 0

    while idx < len(queue):
        node = queue[idx]
        idx += 1
        base = dist[node]
        for edge in graph.get(node, []):
            neighbor = edge['neighbor']
            if neighbor in dist:
                continue
            dist[neighbor] = base + 1
            queue.append(neighbor)

    return dist


def _build_weighted_nexthops(
    graph: dict[str, list[dict[str, str]]],
    src: str,
    dst: str,
    hop_weights: dict[int, int],
    max_next_hops: int,
) -> list[dict[str, int | str]]:
    """Build weighted next-hop candidates from src to dst using static hop-based weights."""
    distances = _shortest_distances_to_dst(graph, dst)
    candidates = []
    for edge in graph.get(src, []):
        neighbor = edge['neighbor']
        if neighbor not in distances:
            continue

        total_hops = 1 + distances[neighbor]
        weight = hop_weights.get(total_hops, 1)
        if weight <= 0:
            continue

        candidates.append({
            'dev': edge['dev'],
            'next_hop': edge['next_hop'],
            'weight': weight,
            'hops': total_hops,
        })

    candidates.sort(key=lambda c: (c['hops'], -c['weight'], str(c['dev']), str(c['next_hop'])))
    if max_next_hops > 0:
        candidates = candidates[:max_next_hops]

    return candidates


def _configure_basic_nat_routes(net: Mininet, nat, nat_intf: str, n_nodes: int, n_routers: int) -> None:
    for n in range(1, n_nodes + 1):
        _safe_add_route(nat, f'ip route add 11.0.{n}.0/24 via 11.0.{n_nodes+1}.1 dev {nat_intf}')

    for n in range(1, n_routers + 1):
        _safe_add_route(nat, f'ip route add 11.{10 + n + 1}.1.0/24 via 11.0.{n_nodes+1}.1 dev {nat_intf}')

        router = net[f'r{n+1}']
        router_to_nat_intf = [intf for intf in router.intfList() if intf.IP().startswith(f'11.{10 + n + 1}.')][0]
        _safe_add_route(router, f'ip route add 11.0.{n_nodes+1}.0/24 via 11.{10 + n + 1}.1.1 dev {router_to_nat_intf}')


def _configure_geant_routes(
    net: Mininet,
    topo,
    nat,
    nat_intf: str,
    n_nodes: int,
    weighted_nexthops: bool = False,
    hop_weights_raw: str | None = None,
) -> None:
    nat_router_ip = f'11.0.{n_nodes+1}.1'
    nat_router = net.get('r1')

    # NAT must reach all node LANs and all nat-router uplink subnets.
    for n in range(1, n_nodes + 1):
        _safe_add_route(nat, f'ip route add 11.0.{n}.0/24 via {nat_router_ip} dev {nat_intf}')

    geant_router_names = {f'r{i+1}' for i in range(1, n_nodes + 1)}
    for idx in range(1, n_nodes + 1):
        subnet_octet = 11 + idx
        _safe_add_route(nat, f'ip route add 11.{subnet_octet}.1.0/24 via {nat_router_ip} dev {nat_intf}')

        router = net[f'r{idx+1}']
        router_uplink_intf = None
        nat_router_uplink_intf = None
        for link in net.links:
            a = link.intf1.node.name
            b = link.intf2.node.name
            if {a, b} != {router.name, 'r1'}:
                continue

            if a == router.name:
                router_uplink_intf = link.intf1
                nat_router_uplink_intf = link.intf2
            else:
                router_uplink_intf = link.intf2
                nat_router_uplink_intf = link.intf1
            break

        if router_uplink_intf is None or nat_router_uplink_intf is None:
            raise RuntimeError(f'GEANT: uplink between {router.name} and r1 not found')

        router_uplink_dev = str(router_uplink_intf)
        nat_router_uplink_dev = str(nat_router_uplink_intf)
        expected_router_uplink_ip = f'11.{subnet_octet}.1.2/24'
        expected_nat_router_uplink_ip = f'11.{subnet_octet}.1.1/24'

        if not router_uplink_intf.IP() or not router_uplink_intf.IP().startswith(f'11.{subnet_octet}.1.'):
            router.cmd(f'ip addr replace {expected_router_uplink_ip} dev {router_uplink_dev}')
        if not nat_router_uplink_intf.IP() or not nat_router_uplink_intf.IP().startswith(f'11.{subnet_octet}.1.'):
            nat_router.cmd(f'ip addr replace {expected_nat_router_uplink_ip} dev {nat_router_uplink_dev}')

        _safe_add_route(
            router,
            f'ip route add 11.0.{n_nodes+1}.0/24 via 11.{subnet_octet}.1.1 dev {router_uplink_dev}',
        )

    # NAT must be able to return traffic to rmc's management subnet.
    _safe_add_route(nat, f'ip route add 11.254.1.0/24 via {nat_router_ip} dev {nat_intf}')

    # Multicast backbone router needs explicit reachability to controller/NAT networks.
    try:
        rmc = net.get('rmc')
    except Exception:
        rmc = None

    if rmc is not None:
        r1 = net.get('r1')
        rmc_mgmt_intf = None
        r1_mgmt_intf = None

        # Find the dedicated management link by topology relation (rmc <-> r1),
        # which is more reliable than searching interfaces by preconfigured IP.
        for link in net.links:
            a = link.intf1.node.name
            b = link.intf2.node.name
            if {a, b} == {'rmc', 'r1'}:
                if a == 'rmc':
                    rmc_mgmt_intf = link.intf1
                    r1_mgmt_intf = link.intf2
                else:
                    rmc_mgmt_intf = link.intf2
                    r1_mgmt_intf = link.intf1
                break

        if rmc_mgmt_intf is not None and r1_mgmt_intf is not None:
            mgmt_dev = str(rmc_mgmt_intf)

            # Ensure management IPs exist even if Mininet interface params were not applied.
            if not rmc_mgmt_intf.IP() or not rmc_mgmt_intf.IP().startswith('11.254.'):
                rmc.cmd(f'ip addr replace 11.254.1.2/24 dev {mgmt_dev}')
            r1_mgmt_dev = str(r1_mgmt_intf)
            if not r1_mgmt_intf.IP() or not r1_mgmt_intf.IP().startswith('11.254.'):
                r1.cmd(f'ip addr replace 11.254.1.1/24 dev {r1_mgmt_dev}')

            # Broad control-plane route for controller/NAT side addresses.
            _safe_add_route(rmc, f'ip route add 11.0.0.0/16 via 11.254.1.1 dev {mgmt_dev}')
            # Keep explicit NAT subnet route for clarity and compatibility with older setups.
            _safe_add_route(rmc, f'ip route add 11.0.{n_nodes+1}.0/24 via 11.254.1.1 dev {mgmt_dev}')
            info(f'Configured rmc control-plane routes via {mgmt_dev}')
        else:
            error('GEANT: rmc<->r1 management link not found; controller reachability may fail.')

    # Auto-provision unicast routes across the GEANT backbone.
    graph = _build_router_link_graph(net, geant_router_names)
    hop_weights = _parse_geant_hop_weights(hop_weights_raw)
    max_next_hops = max(1, int(os.getenv('GEANT_MAX_NEXTHOPS', '4')))
    if weighted_nexthops:
        info(f"GEANT weighted nexthops enabled (hop weights: {hop_weights}, max_nexthops={max_next_hops})")

    for src_idx in range(1, n_nodes + 1):
        src_name = f'r{src_idx+1}'
        src_router = net[src_name]
        for dst_idx in range(1, n_nodes + 1):
            if src_idx == dst_idx:
                continue

            dst_name = f'r{dst_idx+1}'
            dest_subnet = f'13.0.{dst_idx}.0/24'

            if weighted_nexthops:
                nexthops = _build_weighted_nexthops(
                    graph=graph,
                    src=src_name,
                    dst=dst_name,
                    hop_weights=hop_weights,
                    max_next_hops=max_next_hops,
                )

                if nexthops:
                    if len(nexthops) == 1:
                        nh = nexthops[0]
                        _safe_add_route(src_router, f"ip route replace {dest_subnet} via {nh['next_hop']} dev {nh['dev']}")
                    else:
                        nexthop_args = ' '.join(
                            f"nexthop via {nh['next_hop']} dev {nh['dev']} weight {nh['weight']}"
                            for nh in nexthops
                        )
                        route_cmd = f'ip route replace {dest_subnet} {nexthop_args}'
                        result = src_router.cmd(route_cmd)
                        if result and 'File exists' not in result:
                            info(result)
                    continue

            hop = _shortest_path_next_hop(graph, src_name, dst_name)
            if hop is None:
                error(f'No GEANT path between {src_name} and {dst_name}')
                continue

            out_dev, next_hop_ip = hop
            _safe_add_route(src_router, f'ip route add {dest_subnet} via {next_hop_ip} dev {out_dev}')

def _pump_stream(stream, log_fn, prefix):
    """Read a stream line-by-line and forward to Mininet logger."""
    def run():
        try:
            for line in iter(stream.readline, ''):
                if not line:
                    break
                # Ensure lines end with \n so your Rust line-reader flushes promptly.
                if not line.endswith('\n'):
                    line += '\n'
                # Don't print empty lines
                if line.strip():
                    log_fn(f"{prefix} {line}")
        finally:
            try:
                stream.close()
            except Exception:
                pass
    t = threading.Thread(target=run, daemon=True)
    t.start()

def _track_bg(proc, node_name, command):
    """Remember the bg process and its group and log when it exits."""
    global _BG_PROCS, _BG_GROUPS
    _BG_PROCS[proc.pid] = proc
    try:
        pgid = os.getpgid(proc.pid)
        _BG_GROUPS[proc.pid] = pgid
    except Exception:
        pass

    def waiter():
        rc = proc.wait()
        # do NOT eagerly drop the PG record; keep it until /stop cleans it
        _BG_PROCS.pop(proc.pid, None)
        info(f"[{node_name}] process {proc.pid} exited with code {rc}")
    threading.Thread(target=waiter, daemon=True).start()

def _shutdown_bg_processes(timeout_sec: float = 3.0):
    """Try to gracefully stop all tracked background processes."""
    global _BG_PROCS, _BG_GROUPS, net

    my_pgid = os.getpgrp()
    my_pid = os.getpid()

    items = list(_BG_PROCS.items())
    groups = list(_BG_GROUPS.items())

    if not items and not groups:
        info("No background processes to stop.")
        return

    info(f"Stopping {len(items)} tracked process(es), {len(groups)} group(s)...")

    # TERM by process handle (best effort)
    for pid, proc in items:
        if proc.poll() is None:
            try:
                # Do not try to kill our own process
                if pid == my_pid:
                    continue

                pgid = os.getpgid(proc.pid)
                # Don't try to kill our own process group
                if pgid == my_pgid:
                    # Instead, just terminate the process directly
                    info(f"SIGTERM pid {pid}")
                    os.kill(pid, signal.SIGTERM)
                    continue

                # The process belongs to a separate process group; kill the whole group
                info(f"SIGTERM pgid {pgid} (pid {pid})")
                os.killpg(pgid, signal.SIGTERM)
            except Exception as e:
                error(f"Failed SIGTERM via handle {pid}: {e}")

    # TERM by remembered groups (covers early-exiting wrappers)
    for pid, pgid in groups:
        try:
            if pgid == my_pgid:
                continue
            info(f"SIGTERM remembered pgid {pgid} (from pid {pid})")
            os.killpg(pgid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        except Exception as e:
            error(f"Failed SIGTERM pgid {pgid}: {e}")

    # Give them a moment
    deadline = time.time() + timeout_sec
    while time.time() < deadline:
        time.sleep(0.05)

    # KILL leftovers
    for pid, pgid in groups:
        try:
            if pgid == my_pgid:
                info(f"Skip SIGKILL pgid {pgid} (own PG)")
                continue
            info(f"SIGKILL remembered pgid {pgid} (from pid {pid})")
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except Exception as e:
            error(f"Failed SIGKILL pgid {pgid}: {e}")

    _BG_PROCS.clear()
    _BG_GROUPS.clear()

    info("Background process cleanup complete.")

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
            except (BrokenPipeError, ConnectionResetError):
                info(f"Client disconnected before '{parsed_path.path}' response was sent")
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
        try:
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Connection", "close") # We don't support persistent connections
            self.end_headers()
            self.wfile.write(json.dumps(message).encode("utf-8"))

            return True
        except (BrokenPipeError, ConnectionResetError):
            info(f"Client disconnected while sending HTTP {code} response")

        return False

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

    def log_message(self, format, *args):
        # Send access logs to stdout (Mininet info), with newline so your reader flushes.
        mn_info(f"{self.client_address[0]} [{self.log_date_time_string()}] {format % args}\n")

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

        topology_mode = getattr(topo, 'topology_mode', 'basic')
        if topology_mode == 'geant':
            geant_nodes = getattr(topo, 'geant_node_count', n_nodes)
            geant_weighted_nexthops = _is_truthy(query_params.get('geant_weighted_nexthops', ['false'])[0])
            geant_hop_weights = query_params.get('geant_hop_weights', [None])[0]
            _configure_geant_routes(
                net,
                topo,
                nat,
                nat_intf,
                geant_nodes,
                weighted_nexthops=geant_weighted_nexthops,
                hop_weights_raw=geant_hop_weights,
            )
        else:
            _configure_basic_nat_routes(net, nat, nat_intf, n_nodes, n_routers)

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
        # 1) Stop/kill any background processes we spawned explicitly
        _shutdown_bg_processes(timeout_sec=2.0)

        # 2) Then stop the Mininet network and cleanup
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

    info(f"Executing command '{command}' on node '{node_name}' (background: {background})")

    if not node_name or not command:
        if request_handler:
            request_handler._send_response(400, {"message": "Missing node or command parameter"})
        return False

    try:
        node = net.get(node_name)
        if background:
            # --- sanitize the command ---
            safe_cmd = command.strip()
            if safe_cmd.startswith("sudo "):
                # This program is already running as root, so sudo is redundant and may cause issues
                safe_cmd = safe_cmd[5:].strip()

            proc = node.popen(
                safe_cmd,
                shell=True,                # important: avoid /bin/sh -c wrapper
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                universal_newlines=True,    # text mode
                bufsize=1,                  # line-buffered if the child flushes lines
                start_new_session=False      # creates its own process group for clean kills
            )

            prefix = f"[{node_name}]"
            _pump_stream(proc.stdout, info, prefix)   # stdout -> info
            _pump_stream(proc.stderr, error, prefix)  # stderr -> error
            _track_bg(proc, node_name, command)

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
                # Sleep to allow other threads to run
                time.sleep(0.001)
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
    
    #info("Getting all the nodes in the Mininet network")
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
    
    #info("Getting all the links in the Mininet network")
    try:
        links: list[dict] = []
        for link in net.links:
            if link is None:
                continue
            # Retrieve IP addresses of both interfaces, if they exist
            ip1 = link.intf1.IP() if link.intf1 is not None and link.intf1.IP() is not None else "N/A"
            ip2 = link.intf2.IP() if link.intf2 is not None and link.intf2.IP() is not None else "N/A"
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

        line_width = 2.5
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

        # Find all routers (note that some switches may be called a router for ease of detection).
        router_nodes = [node["name"] for node in status["nodes"] if node.get("type") == "LinuxRouter" or node["name"].startswith("r")]
        edge_nodes = [node["name"] for node in status["nodes"] if node.get("type") == "EdgeNode" and node["name"] not in router_nodes]
        # Add NAT to edge nodes when present in the graph.
        if "nat0" in G and "nat0" not in edge_nodes:
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

        # Create the visualization
        plt.figure(figsize=(fig_width, fig_height))

        # Assign positions
        pos = {}

        def numeric_suffix(name):
            digits = ''.join(ch for ch in name if ch.isdigit())
            return int(digits) if digits else 10_000

        is_geant_like = "rmc" in G and any(node.startswith("n") and node[1:].isdigit() for node in G.nodes())

        if is_geant_like:
            geant_routers = sorted(
                [node for node in router_nodes if node != "rmc" and node in G],
                key=numeric_suffix,
            )
            geant_edges = sorted(
                [node for node in edge_nodes if node.startswith("n") and node[1:].isdigit()],
                key=numeric_suffix,
            )

            inner_radius = max(3.4, 0.55 * max(1, len(geant_routers)))
            outer_radius = inner_radius + 2.8
            router_angles = {}

            # Inner ring: all routers except rmc.
            for i, router in enumerate(geant_routers):
                angle = (2 * math.pi * i / max(1, len(geant_routers))) + math.pi / 2
                router_angles[router] = angle
                pos[router] = (inner_radius * math.cos(angle), inner_radius * math.sin(angle))

            # Center: dedicated multicast router.
            if "rmc" in G:
                pos["rmc"] = (0.0, 0.0)

            # Outer ring: nX aligned with their corresponding router r(X+1) when available.
            for node in geant_edges:
                idx = numeric_suffix(node)
                mapped_router = f"r{idx + 1}"
                angle = router_angles.get(mapped_router, (2 * math.pi * idx / max(1, len(geant_edges))) + math.pi / 2)
                pos[node] = (outer_radius * math.cos(angle), outer_radius * math.sin(angle))

            # NAT on outer ring near r1 if available.
            if "nat0" in G:
                nat_angle = router_angles.get("r1", -math.pi / 2)
                pos["nat0"] = ((outer_radius + 0.9) * math.cos(nat_angle), (outer_radius + 0.9) * math.sin(nat_angle))

            # Place remaining nodes (e.g., switches) near neighbor centroids.
            for node in other_nodes:
                if node not in G:
                    continue
                neighbors = list(G.neighbors(node))
                neighbor_positions = [pos[n] for n in neighbors if n in pos]
                if neighbor_positions:
                    cx = sum(x for x, _ in neighbor_positions) / len(neighbor_positions)
                    cy = sum(y for _, y in neighbor_positions) / len(neighbor_positions)
                    pos[node] = (cx * 0.9, cy * 0.9)
                else:
                    pos[node] = (0.0, -outer_radius - 1.0)
        else:
            def evenly_spaced_positions(nodes, y_pos):
                """Generate x positions for nodes evenly spaced between 0 and 1."""
                count = len(nodes)
                if count == 1:
                    return {nodes[0]: (0.5, y_pos)}
                return {node: ((i + 1) / (count + 1), y_pos) for i, node in enumerate(nodes)}

            # Default non-GEANT layout: rows.
            pos.update(evenly_spaced_positions(router_nodes, y_pos=0))
            pos.update(evenly_spaced_positions(edge_nodes, y_pos=-2))
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
        up_edges = [edge for edge in G.edges if G.edges[edge]["status"] == "up"]
        down_edges = [edge for edge in G.edges if G.edges[edge]["status"] != "up"]
        edge_color_by_edge = {tuple(sorted(edge)): edge_colors[i] for i, edge in enumerate(G.edges())}

        nx.draw_networkx_nodes(G, pos, node_size=900 if is_geant_like else 700, node_color=node_colors)
        nx.draw_networkx_labels(G, pos, font_size=10, font_color="white", font_weight="bold")

        nx.draw_networkx_edges(
            G,
            pos,
            edgelist=up_edges,
            edge_color=[edge_color_by_edge[tuple(sorted(edge))] for edge in up_edges],
            width=line_width,
            alpha=0.9,
            connectionstyle="arc3,rad=0.06" if is_geant_like else "arc3,rad=0.0",
        )
        nx.draw_networkx_edges(
            G,
            pos,
            edgelist=down_edges,
            edge_color=[edge_color_by_edge[tuple(sorted(edge))] for edge in down_edges],
            width=line_width,
            alpha=0.7,
            style="dashed",
            connectionstyle="arc3,rad=0.06" if is_geant_like else "arc3,rad=0.0",
        )

        # Keep interface/IP labels visible; use a compact style for GEANT to reduce clutter.
        nx.draw_networkx_edge_labels(
            G,
            pos,
            edge_labels=edge_labels,
            font_size=7 if is_geant_like else 10,
            font_color="dimgray" if is_geant_like else "gray",
            bbox={"alpha": 0.55, "facecolor": "white", "edgecolor": "none", "pad": 0.15} if is_geant_like else None,
            rotate=False if is_geant_like else True,
        )
        
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
