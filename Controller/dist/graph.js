// graph.js
// This module builds a connected graph from a JSON data structure and provides graph traversal functionality,
// including finding the shortest (unweighted) path between nodes.

class Node {
    constructor(name, nodeType) {
        this.name = name;
        this.type = nodeType;
        // Map each interface (port) to an array of IP addresses.
        this.interfaces = {};
        // Array of neighbor objects in the form { node, link }
        this.neighbors = [];
    }

    addInterface(interfaceName, ip) {
        if (!this.interfaces[interfaceName]) {
            this.interfaces[interfaceName] = [];
        }
        // Only add valid IP addresses (ignore "N/A").
        if (ip && ip.toUpperCase() !== "N/A") {
            this.interfaces[interfaceName].push(ip);
        }
    }

    toString() {
        return `Node(name=${this.name}, type=${this.type}, interfaces=${JSON.stringify(this.interfaces)})`;
    }
}

class Link {
    constructor(linkData) {
        this.intf1 = linkData.intf1;
        this.intf2 = linkData.intf2;
        this.ip1 = linkData.ip1;
        this.ip2 = linkData.ip2;
        this.node1 = linkData.node1;
        this.node2 = linkData.node2;
        this.status = linkData.status;
    }

    toString() {
        return `Link(${this.node1}:${this.intf1} (${this.ip1}) <-> ${this.node2}:${this.intf2} (${this.ip2}), status=${this.status})`;
    }
}

class Graph {
    constructor() {
        // Dictionary of nodes keyed by node name.
        this.nodes = {};
        // Array to store all links.
        this.links = [];
    }

    addNode(name, nodeType = "Unknown") {
        if (!this.nodes[name]) {
            this.nodes[name] = new Node(name, nodeType);
        }
    }

    addLink(linkData) {
        const link = new Link(linkData);
        this.links.push(link);

        // Ensure both nodes exist in the graph.
        if (!this.nodes[link.node1]) {
            this.addNode(link.node1);
        }
        if (!this.nodes[link.node2]) {
            this.addNode(link.node2);
        }

        // Update each node with its interface/IP information.
        this.nodes[link.node1].addInterface(link.intf1, link.ip1);
        this.nodes[link.node2].addInterface(link.intf2, link.ip2);

        // Add bidirectional neighbor connections.
        this.nodes[link.node1].neighbors.push({ node: this.nodes[link.node2], link });
        this.nodes[link.node2].neighbors.push({ node: this.nodes[link.node1], link });
    }

    toString() {
        const nodesStr = Object.values(this.nodes)
        .map(node => node.toString())
        .join("\n");
        const linksStr = this.links.map(link => link.toString()).join("\n");
        return `Graph:\nNodes:\n${nodesStr}\n\nLinks:\n${linksStr}`;
    }

    /**
   * Find the shortest path between two nodes (by name) using BFS. In addition
   * to returning the ordered list of node names, it returns an array of segments,
   * where each segment details the interface information used to traverse that edge.
   *
   * @param {string} startName - Name of the starting node.
   * @param {string} endName - Name of the destination node.
   * @returns {Object|null} An object containing:
   *                        - nodes: an array of node names representing the path,
   *                        - segments: an array of objects, each representing a hop with:
   *                           { from, to, fromInterface, toInterface, fromIp, toIp, status }
   *                        Returns null if no path exists.
   */
  shortestPath(startName, endName) {
    if (!this.nodes[startName] || !this.nodes[endName]) {
      return null;
    }

    // BFS initialization.
    const queue = [startName];
    const visited = new Set([startName]);
    // previous mapping: key = node name, value = { prev: previous node name, link: Link used }
    const previous = {};

    while (queue.length > 0) {
      const currentName = queue.shift();
      if (currentName === endName) {
        break;
      }

      const currentNode = this.nodes[currentName];
      for (const neighbor of currentNode.neighbors) {
        const neighborName = neighbor.node.name;
        if (!visited.has(neighborName)) {
          visited.add(neighborName);
          previous[neighborName] = { prev: currentName, link: neighbor.link };
          queue.push(neighborName);
        }
      }
    }

    // If the destination was not reached, return null.
    if (!visited.has(endName)) {
      return null;
    }

    // Reconstruct the node path from endName back to startName.
    const nodePath = [];
    for (let at = endName; at !== undefined; at = previous[at] ? previous[at].prev : undefined) {
      nodePath.push(at);
      if (at === startName) break;
    }
    nodePath.reverse();

    // Reconstruct segments with interface details.
    const segments = [];
    for (let i = 1; i < nodePath.length; i++) {
      const prevNode = nodePath[i - 1];
      const currNode = nodePath[i];
      const edgeInfo = previous[currNode].link;
      let segment = null;

      // Determine which interface belongs to which node.
      if (edgeInfo.node1 === prevNode && edgeInfo.node2 === currNode) {
        segment = {
          from: prevNode,
          to: currNode,
          fromInterface: edgeInfo.intf1,
          toInterface: edgeInfo.intf2,
          fromIp: edgeInfo.ip1,
          toIp: edgeInfo.ip2,
          status: edgeInfo.status
        };
      } else if (edgeInfo.node2 === prevNode && edgeInfo.node1 === currNode) {
        segment = {
          from: prevNode,
          to: currNode,
          fromInterface: edgeInfo.intf2,
          toInterface: edgeInfo.intf1,
          fromIp: edgeInfo.ip2,
          toIp: edgeInfo.ip1,
          status: edgeInfo.status
        };
      }
      segments.push(segment);
    }

    return { nodes: nodePath, segments };
  }

  /**
   * Compute an IP mapping for all nodes reachable from the given start node.
   * For each node (other than the start node), this function computes the shortest path
   * from the start node and uses the IP address in the last segment of that path.
   *
   * @param {string} startName - The starting node name (e.g., "nat0").
   * @returns {Object} A mapping object where keys are node names and values are the IP address
   *                   that should be used to contact that device from the start node.
   */
  getIpMappingFrom(startName) {
    const mapping = {};
    // Iterate over all nodes in the graph.
    for (const nodeName in this.nodes) {
      if (nodeName === startName) {
        continue; // Skip the start node.
      }
      const pathResult = this.shortestPath(startName, nodeName);
      if (pathResult && pathResult.segments.length > 0) {
        // Use the 'toIp' of the last segment in the path.
        const ip = pathResult.segments[pathResult.segments.length - 1].toIp;
        if (ip && ip.length && ip.toUpperCase() !== "N/A") {
          mapping[nodeName] = ip;
        }
      }
    }
    return mapping;
  }
}

/**
 * Builds and returns a Graph instance from the provided data object.
 * The data object is expected to have "nodes" and "links" arrays.
 *
 * @param {Object} data - The JSON data containing nodes and links.
 * @returns {Graph} The built graph with a connected structure.
 */
function buildGraph(data) {
    const graph = new Graph();

    // First, add all nodes with their types.
    if (Array.isArray(data.nodes)) {
        data.nodes.forEach(nodeData => {
            const name = nodeData.name;
            const type = nodeData.type || "Unknown";
            graph.addNode(name, type);
        });
    }

    // Next, add all links.
    if (Array.isArray(data.links)) {
        data.links.forEach(linkData => {
            graph.addLink(linkData);
        });
    }

    return graph;
}

// Export the classes and buildGraph function so that other modules (like app.js) can use them.
module.exports = {
    Node,
    Link,
    Graph,
    buildGraph
};
