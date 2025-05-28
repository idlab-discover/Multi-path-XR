document.addEventListener('DOMContentLoaded', () => {
    const experimentSection = document.getElementById('experiment');
    const experimentSelect = document.getElementById('experimentSelect');
    const startBtn = document.getElementById('startBtn');
    const stopBtn = document.getElementById('stopBtn');
    const statusBar = document.getElementById('status');
    const networkSections = document.getElementById('networkSections');
    const serverSections = document.getElementById('serverSections');
    const topologyImage = document.getElementById('topologyImage');
    const nodeSelect = document.getElementById('nodeSelect');
    const nodeSelect2 = document.getElementById('nodeSelect2');
    const protocolSelect = document.getElementById('protocolSelect');
    const apiPort = document.getElementById('apiPort');
    const openXtermBtn = document.getElementById('openXtermBtn');
    let natIp = null;
    let cachedStatus = null;
    let cacheTimestamp = null;
    let graph = null;
    let nodeIps = {};
    let autoStopTimer = null;
    let autoNextWhenDone = false;
    let experiments = [];
    
    async function fetchStatus(useCache = true) {
        const cacheDurationMs = 10000; // Cache duration: 10 seconds
        const now = Date.now();
    
        if (useCache && cachedStatus && cacheTimestamp && now - cacheTimestamp < cacheDurationMs) {
            return cachedStatus;
        }
    
        try {
            const response = await fetch('/status');
            if (response.ok) {
                cachedStatus = await response.json();
                cacheTimestamp = now;

                if (cachedStatus.status === 'running' || cachedStatus.status === 'success') {
                    generateGraph(cachedStatus);
                }

                return cachedStatus;
            } else {
                throw new Error('Failed to fetch status.');
            }
        } catch (error) {
            console.error('Error fetching status:', error);
            graph = null; // Reset the graph if status fetch fails
            nodeIps = {}; // We should reset the node IPs cache as well
            return null;
        }
    }

    function generateGraph(status) {

        try {
            graph = buildGraph(status);
            nodeIps = graph.getIpMappingFrom('nat0');
            console.log("\nIP Mapping (destination node => IP to use):");
            for (const node in nodeIps) {
                console.log(` - ${node}: ${nodeIps[node]}`);
            }
        } catch (error) {
            console.log('Error generating graph:', error);
        }
    }

    // Initialize the page state
    async function initialize() {
        setStatus('Loading...', 'info');
        // Ensure the experiments list is fetched on page load
        fetchExperiments();
        try {
            const status = await fetchStatus(false);
            if (status) {
                if (status === 'running' || status.status === 'running') {
                    setStatus('Environment is already running. Loading data...', 'success');
                    networkSections.classList.remove('hidden');
                    serverSections.classList.remove('hidden');
                    await fetchTopologyImage();
                    await fetchAndPopulateNodes(false);
                } else {
                    setStatus('Environment is not running.', 'warning');
                    networkSections.classList.add('hidden');
                    serverSections.classList.add('hidden');
                }
            } else {
                setStatus('Failed to fetch status.', 'error');
                networkSections.classList.add('hidden');
                serverSections.classList.add('hidden');
            }
        } catch (error) {
            setStatus(`Error initializing page: ${error}`, 'error');
            networkSections.classList.add('hidden');
            serverSections.classList.add('hidden');
        }
    }

    async function fetchExperiments() {
        try {
            const response = await fetch('/list_experiments');
            if (response.ok) {
                const data = await response.json();
                experiments = (data.experiments) || [];
                experiments.sort((a, b) => a.localeCompare(b));
                populateExperimentList(experiments);
            } else {
                setStatus('Failed to fetch experiments.', 'error');
            }
        } catch (error) {
            setStatus(`Error fetching experiments: ${error}`, 'error');
        }
    }

    function populateExperimentList(experiments) {
        console.log(experiments);
        // Sort the experiments alphabetically
        // Clear the existing options and add a default option
        experimentSelect.innerHTML = '<option value="">Select an experiment</option>';
        experiments.forEach((exp, index) => {
            const option = document.createElement('option');
            option.value = index;
            option.textContent = exp;
            experimentSelect.appendChild(option);
        });
        experimentSection.classList.remove('hidden');
        if (experiments.length > 0) {
            experimentSelect.value = "0";
            loadExperiment(experiments[0]);
        }

    }

    async function loadExperiment(experiment) {
        setStatus(`Loading experiment: ${experiment}`, 'info');
        try {
            const response = await fetch(`/experiments/${experiment}`);
            const data = await response.text();
            const parsedYaml = jsyaml.load(data);
            console.log(parsedYaml);

            if (!parsedYaml.environment) {
                setStatus('Invalid experiment file.', 'error');
                return;
            }

            if (parsedYaml.description) {
                document.getElementById('experimentDescription').textContent = parsedYaml.description;
            }

            const environment = parsedYaml.environment;
            if (environment.name) {
                document.getElementById('environmentSelect').value = environment.name;
            }
            if (environment.number_of_nodes) {
                document.getElementById('n_nodes').value = environment.number_of_nodes;
            }
            if (environment.number_of_paths) {
                document.getElementById('n_paths').value = environment.number_of_paths;
            }

            // Store the experiment in global state
            window.current_experiment_name = experiment;
            window.current_experiment = parsedYaml;

            setStatus('Experiment loaded.', 'success');
        } catch (error) {
            setStatus(`Error loading experiment: ${error}`, 'error');
        }
    }

    async function startEnvironment() {
        const environment = document.getElementById('environmentSelect').value;

        let autoStopSeconds   = parseInt(
            document.getElementById('autoStopTime').value || '0', 10
        );
        if (isNaN(autoStopSeconds) || autoStopSeconds < 0) {
            autoStopSeconds = 0;
        }
        autoNextWhenDone = document.getElementById('autoNextExperiment').checked;

        const selectedExperimentIndex = parseInt(experimentSelect.value || "-1", 10);;
        if (selectedExperimentIndex < 0 || selectedExperimentIndex >= experiments.length) {
            alert('Please select an experiment.');
            return;
        }

        const selectedExperiment = experiments[selectedExperimentIndex];
        if (!selectedExperiment) {
            alert('Invalid experiment selected.');
            return;
        }
        
        await loadExperiment(selectedExperiment);   

        const payload = {
            experimentName: window.current_experiment_name,
            environment: environment,
        };

        setStatus('Starting environment...', 'info');
        try {
            const response = await fetch('/start_environment', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json'
                },
                body: JSON.stringify(payload)
            });

            const data = await response.json();
            if (data.status === 'success') {
                setStatus(data.message, 'success');
                networkSections.classList.remove('hidden');
                serverSections.classList.remove('hidden');
                await fetchTopologyImage();
                if ((await fetchAndPopulateNodes()) && window.current_experiment) {
                    // Await two seconds before assigning roles
                    await new Promise(resolve => setTimeout(resolve, 2000));

                    await giveRoles(window.current_experiment);
                    if (autoStopTimer) { clearTimeout(autoStopTimer); }
                    if (autoStopSeconds > 0) {
                        autoStopTimer = setTimeout(() => stopEnvironment(autoNextWhenDone),
                                                autoStopSeconds * 1000);
                        setStatus(`Will auto‑stop in ${autoStopSeconds}s`, 'info');
                    }
                }
            } else {
                setStatus(`Error: ${data.error}`, 'error');
            }
        } catch (error) {
            setStatus(`Error starting environment: ${error}`, 'error');
        }
    }


    async function stopEnvironment(launchNext) {
        if (autoStopTimer) { clearTimeout(autoStopTimer); }
        autoStopTimer = null;
        setStatus('Stopping environment...', 'info');
        try {
            const response = await fetch('/stop');
            const data = await response.json();
            if (data.status === 'success') {
                setStatus(data.message, 'success');
                topologyImage.src = '';
                nodeSelect.innerHTML = '<option value="">Select a node</option>';
                nodeSelect2.innerHTML = '<option value="">Select a node</option>';
                natIp = null;
                networkSections.classList.add('hidden');
                serverSections.classList.add('hidden');
                if (launchNext) {
                    console.log('We need to launch the next experiment');
                    const currentIndex   = parseInt(experimentSelect.value || "0", 10);
                    if (isNaN(currentIndex)) {
                        setStatus('No current experiment selected.', 'warning');
                        return;
                    }
                    const nextIndex = currentIndex + 1;
    
                    if (experiments[nextIndex]) {
                        experimentSelect.value = nextIndex;
                        // Give the UI a tick to update before we click start
                        setTimeout(() => startEnvironment(), 300);
                    } else {
                        setStatus('All experiments finished ✔', 'info');
                    }
                }
            } else {
                setStatus(`Error: ${data.error}`, 'error');
            }
        } catch (error) {
            setStatus(`Error stopping environment: ${error}`, 'error');
        }
    }

    startBtn.addEventListener('click', async () => startEnvironment());
    stopBtn.addEventListener('click', async () => stopEnvironment(false));

    async function fetchTopologyImage() {
        try {
            const response = await fetch('/visualize');
            if (response.ok) {
                const blob = await response.blob();
                const imageUrl = URL.createObjectURL(blob);
                topologyImage.src = imageUrl;
            } else {
                setStatus('Failed to fetch topology image.', 'warning');
            }
        } catch (error) {
            setStatus(`Error fetching topology image: ${error}`, 'error');
        }
    }

    async function fetchAndPopulateNodes(start_agents = true) {
        nodeSelect.innerHTML = '<option value="">Select a node</option>';
        nodeSelect2.innerHTML = '<option value="">Select a node</option>';
        try {
            const status = await fetchStatus(false);

            if (status && status.nodes && status.links) {
                status.nodes.forEach(node => {
                    const option = document.createElement('option');
                    option.value = node.name;
                    option.textContent = `${node.name} (${node.type})`;
                    nodeSelect.appendChild(option);

                    // Check if node.name is a key in nodeIps
                    if (nodeIps[node.name]) {
                        const option2 = document.createElement('option');
                        option2.value = node.name;
                        option2.textContent = `${node.name} (${nodeIps[node.name]})`;
                        nodeSelect2.appendChild(option2);
                    }
                });

                status.links.forEach(link => {
                    if (link.node1 === 'nat0' && link.ip1 !== 'N/A') {
                        natIp = link.ip1;
                    } else if (link.node2 === 'nat0' && link.ip2 !== 'N/A') {
                        natIp = link.ip2;
                    }
                });

                if (natIp) {
                    setStatus(`NAT0 IP: ${natIp}`, 'info');
                    if (start_agents) {
                        return await startAgents(status.nodes, natIp);
                    } else {
                        checkAgentsConnected(window.current_experiment);
                    }
                    return true;
                } else {
                    setStatus('NAT0 IP not found.', 'warning');
                }
            } else {
                setStatus('Failed to fetch nodes and links.', 'warning');
            }
        } catch (error) {
            setStatus(`Error fetching nodes and links: ${error}`, 'error');
        }

        return false;
    }

    // Start the agents on the nodes and the routers
    async function startAgents(nodes, natIp) {
        setStatus('Starting agents...', 'info');
        try {
            const port = window.location.port || 80; // Default to 80 if no port is specified
            const url = `http://${natIp}:${port}`;
            const targetNodes = nodes.filter(
                node => 
                    (node.type === 'EdgeNode' || node.type === 'LinuxRouter') && 
                    node.name !== 'r1'
            );
            let releasearg = '';
            // If the url contains the query string "debug=true", set releasearg to empty string
            const urlParams = new URLSearchParams(window.location.search);
            if (urlParams.get('release') === 'true') {
                releasearg = '--release';
            }

            // Iterate over the nodes and start the agents
            for (const node of targetNodes) {
                const command = `sudo ../../run.sh --agent ${releasearg} --url ${url} --node-id ${node.name}`;
                console.log(`Starting agent on ${node.name} with command: ${command}`);


                const params = new URLSearchParams({ node: node.name, command, background: "true" });
                await fetch(`/exec?${params.toString()}`);
            }

            const agentsStarted = await checkAgentsConnected(window.current_experiment);
            if (!agentsStarted) {
                setStatus('Error starting agents.', 'error');
                return false;
            }

            setStatus('Agents started.', 'success');
            return true;
        } catch (error) {
            setStatus(`Error starting agents: ${error}`, 'error');
            return false;
        }
    }

    async function checkAgentsConnected(experiment, timeoutMs = 30000, pollIntervalMs = 2000) {
        if (!experiment || !experiment.environment) {
            return false;
        }

        const start = Date.now();
    
        // Extract roles from the experiment object
        const roles = experiment.environment.roles || [];
        const requiredAgents = roles.map(role => role.target).filter(target => target !== 'r1');

        if (requiredAgents.length === 0) {
            console.log('No agents to start.');
            return true;
        }

        console.log(requiredAgents);
    
        while (Date.now() - start < timeoutMs) {
            try {
                // Fetch agents and sockets data
                const agentsResponse = await fetch('/list_agents');
                const socketsResponse = await fetch('/list_sockets');
    
                if (!agentsResponse.ok || !socketsResponse.ok) {
                    throw new Error('Failed to fetch agent or socket data');
                }
    
                const agentsData = await agentsResponse.json();
                const socketsData = await socketsResponse.json();
    
                // Map connected socket IDs
                const connectedSockets = new Set(
                    socketsData.sockets
                        .filter(socket => socket.connected)
                        .map(socket => socket.id)
                );

                console.log(connectedSockets);
                console.log(agentsData);
    
                // Check if all required agents are connected
                const allAgentsConnected = requiredAgents.every(agent => {
                    const socketId = agentsData[agent];
                    console.log(agent, socketId, connectedSockets.has(socketId));
                    return socketId && connectedSockets.has(socketId);
                });
    
                if (allAgentsConnected) {
                    console.log('All required agents are connected.');
                    setStatus('All required agents are connected.', 'success'); 
                    return true;
                } else {
                    console.log('Waiting for agents to connect...');
                    setStatus('Waiting for agents to connect...', 'success');
                }
            } catch (error) {
                console.error('Error while checking agent connectivity:', error);
                setStatus(`Error while checking agent connectivity: ${error}`, 'error');
            }
    
            // Wait for the next poll
            await new Promise(resolve => setTimeout(resolve, pollIntervalMs));
        }
    
        console.log('Timeout reached. Not all agents are connected.');
        setStatus('Timeout reached. Not all agents are connected.', 'error');
        return false;
    }

    async function assignRole(role, statusData) {
        const commandBase = 'sudo ../run.sh';
        let command = null;
        let serverPort = 3001;
        let releasearg = '';
        // If the url contains the query string "debug=true", set releasearg to empty string
        const urlParams = new URLSearchParams(window.location.search);
        if (urlParams.get('release') === 'true') {
            releasearg = '--release';
        }
    
        if (role.role === 'router') {
            command = `${commandBase} --metrics ${releasearg}`;
        } else if (role.role === 'server') {
            command = `${commandBase} --server ${releasearg} --port ${serverPort} --log-level info`;
        } else if (role.role === 'client') {
            const serverIp = role.server_ip;
            const disableParser = role.disable_parser ? '--disable-parser ' : '';
            console.log(`Server IP for client ${role.alias}: ${serverIp}`);
            if (!serverIp) {
                console.error(`Failed to find server IP for client: ${role.alias}`);
                return false;
            }
            if (role.visible) {
                command = `${commandBase} --client ${releasearg} --server-url http://${serverIp}:${serverPort} ${disableParser}--log-level 2`;
            } else {
                command = `${commandBase} --client ${releasearg} --headless --server-url http://${serverIp}:${serverPort} ${disableParser}--log-level info`;
            }
        } else if (role.role === 'nothing') {
            return true;
        } else {
            console.warn(`Unknown role: ${role.role}`);
            return false;
        }
    
        try {
            const params = new URLSearchParams({ node_id: role.target, command});
            console.log(`Assigning role ${role.role} to ${role.target} with command: ${command}`);
            const response = await fetch(`/exec_on_agent?${params.toString()}`);
            const data = await response.json();
    
            if (data.status === 'success') {
                console.log(`Role ${role.role} assigned to ${role.target} successfully.`);
                return true;
            } else {
                console.error(`Error assigning role ${role.role} to ${role.target}: ${data.error}`);
                return false;
            }
        } catch (error) {
            console.error(`Error assigning role ${role.role} to ${role.target}:`, error);
            return false;
        }
    }

    async function giveRoles(experiment) {
        if (!experiment || !experiment.environment || !experiment.environment.roles) {
            setStatus('No roles to assign.', 'warning');
            return;
        }
    
        const roles = experiment.environment.roles;
        const statusData = await fetchStatus();
    
        if (!statusData) {
            setStatus('Failed to fetch status for assigning roles.', 'error');
            return;
        }
    
        setStatus('Assigning roles...', 'info');

        const rolePriority = {
            router: 1,
            server: 2,
            client: 3,
            nothing: 4
          };
          
        roles.sort((a, b) => {
            const aPriority = rolePriority[a.role] ?? Infinity;
            const bPriority = rolePriority[b.role] ?? Infinity;
            return aPriority - bPriority;
        });          

    
        for (const role of roles) {
            const success = await assignRole(role, statusData);
            if (!success) {
                setStatus(`Failed to assign role: ${role.alias}`, 'error');
                return;
            } else {
                // Wait for a short time before assigning the next role
                await new Promise(resolve => setTimeout(resolve, 1000));
            }
        }
    
        setStatus('Roles assigned successfully.', 'success');
    }

    function setStatus(message, level) {
        level = level.trim();
        if (level.toLowerCase() === "succes") {
            level = "info";
        }
        statusBar.textContent = message.trim();
        statusBar.className = `status-bar ${level}`;

        console.log(`[${level.toUpperCase()}] ${message}`);
    }

    openXtermBtn.addEventListener('click', async () => {
        const selectedNode = nodeSelect.value;
        if (!selectedNode) {
            alert('Please select a node.');
            return;
        }

        setStatus('Opening xterm...', 'info');
        try {
            const params = new URLSearchParams({ node: selectedNode });
            const response = await fetch(`/start_xterm?${params.toString()}`);
            const data = await response.json();

            if (data.status === 'success') {
                setStatus(data.message, 'success');
            } else {
                setStatus(`Error: ${data.error}`, 'error');
            }
        } catch (error) {
            setStatus(`Error starting xterm: ${error}`, 'error');
        }
    });

    // Helper: extract method and path from a string like
    // "Method: GET    Path: /datasets"
    function extractMethodAndPath(text) {
        const regex = /Method:\s*(\w+).*Path:\s*(\S+)/i;
        const match = text.match(regex);
        if (match) {
        return { method: match[1].toUpperCase(), path: match[2] };
        }
        return null;
    }

    // Helper: from a control-group, get the parameter name from the label.
    // Assumes label text like: "dataset (String):"
    function getParamName(controlGroup) {
        const label = controlGroup.querySelector('label');
        if (!label) return null;
        // Split by '(' and take the first part, then trim any whitespace or colon
        return label.textContent.split('(')[0].replace(':','').trim();
    }

    // Helper: collect parameter values from all controls in an api-section.
    function collectParams(section) {
        const params = {};
        const controlGroups = section.querySelectorAll('.control-group');
        controlGroups.forEach(group => {
        const paramName = getParamName(group);
        if (!paramName) return;
        // Find the input, select, or textarea inside this control group.
        const input = group.querySelector('input, select, textarea');
        if (input) {
            // Only include if non-empty (or you can include even empty values)
            if (input.value !== '') {
                params[paramName] = input.value;
            }
        }
        });
        return params;
    }

    // Helper: Build a query string from an object.
    function buildQuery(params) {
        return Object.entries(params)
        .map(([k, v]) => encodeURIComponent(k) + '=' + encodeURIComponent(v))
        .join('&');
    }

    // For every API section, auto-wire the call.
    const apiSections = document.querySelectorAll('.api-section');
    apiSections.forEach(section => {
        // Assume that the first <p> in the section has text "Method: ... Path: ..."
        const p = section.querySelector('p');
        if (!p) return;
        const meta = extractMethodAndPath(p.textContent);
        if (!meta) return;
        const { method, path } = meta;

        // Find the button in the section (assume one button)
        const btn = section.querySelector('button');
        if (!btn) return;
        // Find the <pre> element where the response will be written.
        const pre = section.querySelector('.api-response');

        // Attach event listener on the button.
        btn.addEventListener('click', async () => {
            // Collect parameters from the controls within the section.
            const params = collectParams(section);

            // Determine the full URL using global host details.
            const selectedNode = nodeSelect2.value;
            if (!selectedNode) {
                alert('Please select a node in the "Call API" section.');
                return;
            }
            // Look up the node IP from the global nodeIps mapping.
            if (!nodeIps || !nodeIps[selectedNode]) {
                alert('IP not found for selected node.');
                return;
            }
            const host = `${protocolSelect.value}://${nodeIps[selectedNode]}:${apiPort.value}`;
            let url = host + path;

            try {
                let fetchOptions = { method };

                // For GET requests, attach parameters as a query string.
                if (method === 'GET') {
                const query = buildQuery(params);
                if (query) {
                    url += (url.includes('?') ? '&' : '?') + query;
                }
                } else if (method === 'POST') {
                // For POST, send parameters as JSON.
                fetchOptions.headers = { 'Content-Type': 'application/json' };
                fetchOptions.body = JSON.stringify(params);
                }

                pre.textContent = `Calling ${url} with options:\n${JSON.stringify(fetchOptions, null, 2)}`;
                const response = await fetch(url, fetchOptions);
                let respText = await response.text();
                pre.innerHTML = `Response from ${url}: <br><br>${respText}`;
            } catch (error) {
                pre.textContent = `Error calling ${url}:\n${error}`;
            }
        });
    });

    initialize();
});
