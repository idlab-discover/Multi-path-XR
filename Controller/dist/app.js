document.addEventListener('DOMContentLoaded', () => {
    const experimentSection = document.getElementById('experiment');
    const experimentSelect = document.getElementById('experimentSelect');
    const startBtn = document.getElementById('startBtn');
    const stopBtn = document.getElementById('stopBtn');
    const autoNextExperimentCheckbox = document.getElementById('autoNextExperiment');
    const autoSweepRepeatCountInput = document.getElementById('autoSweepRepeatCount');
    const statusBar = document.getElementById('status');
    const networkSections = document.getElementById('networkSections');
    const serverSections = document.getElementById('serverSections');
    const topologyImage = document.getElementById('topologyImage');
    const nodeSelect = document.getElementById('nodeSelect');
    const nodeSelect2 = document.getElementById('nodeSelect2');
    const protocolSelect = document.getElementById('protocolSelect');
    const apiPort = document.getElementById('apiPort');
    const openXtermBtn = document.getElementById('openXtermBtn');
    const disallowedTargets = new Set(['r1']);
    let natIp = null;
    let cachedStatus = null;
    let cacheTimestamp = null;
    let graph = null;
    let nodeIps = {};
    let autoStopTimer = null;
    let autoNextWhenDone = false;
    let autoSweepState = null;
    let startEnvironmentAttemptId = 0;
    let experiments = [];
    let routerMetricsTargetsStarted = new Set();
    let agentLaunchRecords = new Map();
    const SELECTED_EXPERIMENT_STORAGE_KEY = 'networkController.selectedExperiment';
    const EXPERIMENT_ERROR_GRACE_MS = 5 * 60 * 1000;
    const EXPERIMENT_RETRY_DELAY_MS = 5 * 60 * 1000;
    const EXPERIMENT_ERROR_POLL_MS = 30 * 1000;
    const START_FAILURE_STATUS_POLL_ATTEMPTS = 10;
    const START_FAILURE_RETRY_DELAY_MS = 30 * 1000;
    let currentStatusState = { message: '', level: 'info', timestampMs: 0 };
    let experimentRecoveryArmTimer = null;
    let experimentRecoveryPollTimer = null;
    let experimentRecoveryRestartTimer = null;
    let experimentRecoveryContext = null;
    let experimentRecoveryInProgress = false;
    let startFailureRecoveryTimer = null;
    let startFailureRecoveryContext = null;

    function getSavedExperimentName() {
        try {
            return localStorage.getItem(SELECTED_EXPERIMENT_STORAGE_KEY);
        } catch (error) {
            console.warn('Unable to read selected experiment from local storage:', error);
            return null;
        }
    }

    function saveExperimentName(experimentName) {
        if (!experimentName) {
            return;
        }

        try {
            localStorage.setItem(SELECTED_EXPERIMENT_STORAGE_KEY, experimentName);
        } catch (error) {
            console.warn('Unable to save selected experiment to local storage:', error);
        }
    }

    function clearSavedExperimentName() {
        try {
            localStorage.removeItem(SELECTED_EXPERIMENT_STORAGE_KEY);
        } catch (error) {
            console.warn('Unable to clear selected experiment from local storage:', error);
        }
    }

    function normalizeStatusLevel(level) {
        let normalizedLevel = String(level ?? '').trim().toLowerCase();
        if (normalizedLevel === 'succes') {
            normalizedLevel = 'info';
        }
        return normalizedLevel || 'info';
    }

    function clearExperimentRecoveryTimers() {
        if (experimentRecoveryArmTimer) {
            clearTimeout(experimentRecoveryArmTimer);
        }
        if (experimentRecoveryPollTimer) {
            clearInterval(experimentRecoveryPollTimer);
        }
        if (experimentRecoveryRestartTimer) {
            clearTimeout(experimentRecoveryRestartTimer);
        }
        experimentRecoveryArmTimer = null;
        experimentRecoveryPollTimer = null;
        experimentRecoveryRestartTimer = null;
    }

    function resetExperimentRecoveryState() {
        clearExperimentRecoveryTimers();
        experimentRecoveryContext = null;
        experimentRecoveryInProgress = false;
        resetStartFailureRecoveryState();
    }

    function clearStartFailureRecoveryTimer() {
        if (startFailureRecoveryTimer) {
            clearTimeout(startFailureRecoveryTimer);
        }
        startFailureRecoveryTimer = null;
    }

    function resetStartFailureRecoveryState() {
        clearStartFailureRecoveryTimer();
        startFailureRecoveryContext = null;
    }

    function isStartFailureRecoveryContextCurrent(context) {
        return Boolean(context)
            && isCurrentStartEnvironmentAttempt(context.attemptId)
            && getSelectedExperimentName() === context.experimentName
            && window.current_experiment_name === context.experimentName;
    }

    function scheduleStartFailureRecovery(attemptId, experimentName, errorMessage) {
        resetStartFailureRecoveryState();

        if (!experimentName) {
            setStatus(`Error starting environment: ${errorMessage}`, 'error');
            return;
        }

        const context = {
            attemptId,
            experimentName,
        };
        startFailureRecoveryContext = context;

        const retryDelaySeconds = Math.round(START_FAILURE_RETRY_DELAY_MS / 1000);
        setStatus(
            `Environment start failed for ${experimentName}. Retrying automatically in ${retryDelaySeconds}s. Last error: ${errorMessage}`,
            'warning'
        );

        startFailureRecoveryTimer = setTimeout(() => {
            startFailureRecoveryTimer = null;

            if (!isStartFailureRecoveryContextCurrent(context)) {
                resetStartFailureRecoveryState();
                return;
            }

            setStatus(`Retrying environment start for ${experimentName}...`, 'warning');
            void startEnvironment({ continueAutoSweep: true });
        }, START_FAILURE_RETRY_DELAY_MS);
    }

    function getSelectedExperimentName() {
        const selectedExperimentIndex = Number.parseInt(experimentSelect.value || '-1', 10);
        if (selectedExperimentIndex >= 0 && selectedExperimentIndex < experiments.length) {
            return experiments[selectedExperimentIndex];
        }
        return window.current_experiment_name || null;
    }

    function getConfiguredAutoStopDurationMs() {
        const autoStopInput = document.getElementById('autoStopTime');
        const autoStopSeconds = Number.parseInt(autoStopInput?.value || '0', 10);
        if (Number.isFinite(autoStopSeconds) && autoStopSeconds > 0) {
            return autoStopSeconds * 1000;
        }
        return 0;
    }

    function isAutomaticExperimentModeEnabled() {
        return Boolean(autoNextExperimentCheckbox?.checked);
    }

    function getConfiguredAdditionalSweepCount() {
        const additionalSweepCount = Number.parseInt(autoSweepRepeatCountInput?.value || '0', 10);
        if (Number.isFinite(additionalSweepCount) && additionalSweepCount > 0) {
            return additionalSweepCount;
        }
        return 0;
    }

    function setConfiguredAdditionalSweepCount(additionalSweepCount) {
        if (!autoSweepRepeatCountInput) {
            return;
        }

        const normalizedAdditionalSweepCount = Number.isFinite(additionalSweepCount)
            ? Math.max(Math.floor(additionalSweepCount), 0)
            : 0;
        autoSweepRepeatCountInput.value = String(normalizedAdditionalSweepCount);
    }

    function normalizeAutoSweepRepeatCountInput() {
        if (!autoSweepRepeatCountInput) {
            return;
        }
        setConfiguredAdditionalSweepCount(getConfiguredAdditionalSweepCount());
    }

    function syncAutoSweepRepeatInputState() {
        if (!autoSweepRepeatCountInput) {
            return;
        }
        autoSweepRepeatCountInput.disabled = !isAutomaticExperimentModeEnabled();
    }

    function resetAutoSweepState() {
        autoSweepState = null;
    }

    function configureAutoSweepState(startIndex) {
        normalizeAutoSweepRepeatCountInput();
        autoSweepState = {
            startIndex,
            currentSweep: 1,
        };
    }

    function ensureAutoSweepState(startIndex) {
        if (!autoSweepState) {
            configureAutoSweepState(startIndex);
        }
    }

    function getNextAutoExperimentIndex(currentIndex) {
        const nextIndex = currentIndex + 1;
        if (nextIndex < experiments.length) {
            return {
                index: nextIndex,
                startsNewSweep: false,
                sweepNumber: autoSweepState?.currentSweep || 1,
            };
        }

        const remainingAdditionalSweeps = getConfiguredAdditionalSweepCount();
        if (
            autoSweepState
            && remainingAdditionalSweeps > 0
            && autoSweepState.startIndex >= 0
            && autoSweepState.startIndex < experiments.length
        ) {
            setConfiguredAdditionalSweepCount(remainingAdditionalSweeps - 1);
            autoSweepState.currentSweep += 1;
            return {
                index: autoSweepState.startIndex,
                startsNewSweep: true,
                sweepNumber: autoSweepState.currentSweep,
                remainingAdditionalSweeps: getConfiguredAdditionalSweepCount(),
            };
        }

        return null;
    }

    function getExperimentDurationMs(experiment) {
        const actions = Array.isArray(experiment?.actions) ? experiment.actions : [];
        const actionDurationMs = actions.reduce((maxDurationMs, action) => {
            const executionDelayMs = Number(action?.execution_delay);
            if (Number.isFinite(executionDelayMs) && executionDelayMs > maxDurationMs) {
                return executionDelayMs;
            }
            return maxDurationMs;
        }, 0);
        const configuredAutoStopMs = getConfiguredAutoStopDurationMs();
        const durationMs = Math.max(actionDurationMs, configuredAutoStopMs);
        return durationMs > 0 ? durationMs : null;
    }

    function isExperimentRecoveryContextCurrent(context) {
        return Boolean(context)
            && isCurrentStartEnvironmentAttempt(context.attemptId)
            && window.current_experiment_name === context.experimentName
            && isAutomaticExperimentModeEnabled()
            && getSelectedExperimentName() === context.experimentName;
    }

    function armExperimentRecoveryWatchdog(attemptId, experimentName, experiment) {
        resetExperimentRecoveryState();

        if (!isAutomaticExperimentModeEnabled()) {
            console.info('Automatic experiment recovery is disabled because auto-next mode is not enabled.');
            return;
        }

        const experimentDurationMs = getExperimentDurationMs(experiment);
        if (!experimentName || !experimentDurationMs) {
            console.info('Automatic experiment recovery is disabled because no experiment duration could be derived.');
            return;
        }

        const recoveryDeadlineMs = Date.now() + experimentDurationMs + EXPERIMENT_ERROR_GRACE_MS;
        experimentRecoveryContext = {
            attemptId,
            experimentName,
            recoveryDeadlineMs,
        };

        experimentRecoveryArmTimer = setTimeout(() => {
            void evaluateExperimentRecovery();

            if (experimentRecoveryContext?.attemptId === attemptId) {
                experimentRecoveryPollTimer = setInterval(() => {
                    void evaluateExperimentRecovery();
                }, EXPERIMENT_ERROR_POLL_MS);
            }
        }, Math.max(recoveryDeadlineMs - Date.now(), 0));
    }

    async function evaluateExperimentRecovery() {
        const recoveryContext = experimentRecoveryContext;
        if (!recoveryContext) {
            return;
        }

        if (!isExperimentRecoveryContextCurrent(recoveryContext)) {
            resetExperimentRecoveryState();
            return;
        }

        if (experimentRecoveryInProgress || Date.now() < recoveryContext.recoveryDeadlineMs) {
            return;
        }

        if (normalizeStatusLevel(currentStatusState.level) !== 'error' && !currentStatusState.message.toLocaleLowerCase().includes('error')) {
            return;
        }

        experimentRecoveryInProgress = true;
        clearExperimentRecoveryTimers();

        setStatus(
            `Persistent error detected for ${recoveryContext.experimentName}. Stopping environment before retry.`,
            'warning'
        );

        const stopped = await stopEnvironment({ launchNext: false, resetRecovery: false });
        if (!stopped) {
            experimentRecoveryInProgress = false;
            experimentRecoveryContext = recoveryContext;
            experimentRecoveryPollTimer = setInterval(() => {
                void evaluateExperimentRecovery();
            }, EXPERIMENT_ERROR_POLL_MS);
            return;
        }

        if (!isExperimentRecoveryContextCurrent(recoveryContext)) {
            resetExperimentRecoveryState();
            return;
        }

        setStatus(
            `Persistent error detected for ${recoveryContext.experimentName}. Waiting 5 minutes before retrying.`,
            'warning'
        );

        experimentRecoveryRestartTimer = setTimeout(async () => {
            experimentRecoveryRestartTimer = null;
            experimentRecoveryInProgress = false;

            if (!isExperimentRecoveryContextCurrent(recoveryContext)) {
                resetExperimentRecoveryState();
                return;
            }

            await startEnvironment({ continueAutoSweep: true });
        }, EXPERIMENT_RETRY_DELAY_MS);
    }

    // A map that converts a log level str to a number
    const logLevelMap = {
        "trace": 0,
        "debug": 1,
        "info": 2,
        "warn": 3,
        "error": 4
    };
    const LOG_LEVEL = 'info'; // Change this to control the log level (trace, debug, info, warn, error)
    const currentLogLevel = LOG_LEVEL;
    
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

    function isCurrentStartEnvironmentAttempt(attemptId) {
        return attemptId === startEnvironmentAttemptId;
    }

    async function waitForEnvironmentRunning(maxAttempts = 40, pollIntervalMs = 1500, shouldContinue = () => true) {
        for (let attempt = 0; attempt < maxAttempts; attempt++) {
            if (!shouldContinue()) {
                return null;
            }

            const status = await fetchStatus(false);
            if (status && (status.status === 'running' || status.status === 'success')) {
                return status;
            }

            if (attempt < maxAttempts - 1) {
                await new Promise(resolve => setTimeout(resolve, pollIntervalMs));
            }
        }

        return null;
    }

    async function attemptStartFailureRecovery(attemptId, errorMessage, autoStopSeconds, recoveredMessage) {
        if (!isCurrentStartEnvironmentAttempt(attemptId)) {
            return;
        }

        setStatus('Environment start reported an error. Waiting briefly to see whether it recovers...', 'warning');
        const status = await waitForEnvironmentRunning(
            START_FAILURE_STATUS_POLL_ATTEMPTS,
            1500,
            () => isCurrentStartEnvironmentAttempt(attemptId)
        );

        if (!isCurrentStartEnvironmentAttempt(attemptId)) {
            return;
        }

        if (status && (status.status === 'running' || status.status === 'success')) {
            resetStartFailureRecoveryState();
            armExperimentRecoveryWatchdog(
                attemptId,
                window.current_experiment_name,
                window.current_experiment
            );
            await finalizeEnvironmentStart(
                attemptId,
                recoveredMessage,
                'warning',
                autoStopSeconds
            );
            return;
        }

        scheduleStartFailureRecovery(
            attemptId,
            window.current_experiment_name || getSelectedExperimentName(),
            errorMessage
        );
    }

    async function finalizeEnvironmentStart(attemptId, message, level, autoStopSeconds) {
        if (!isCurrentStartEnvironmentAttempt(attemptId)) {
            return false;
        }

        setStatus(message, level);
        networkSections.classList.remove('hidden');
        serverSections.classList.remove('hidden');
        await fetchTopologyImage();

        if (!isCurrentStartEnvironmentAttempt(attemptId)) {
            return false;
        }

        if ((await fetchAndPopulateNodes()) && window.current_experiment) {
            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return false;
            }

            await new Promise(resolve => setTimeout(resolve, 2000));

            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return false;
            }

            const rolesAssigned = await giveRoles(window.current_experiment);

            if (!rolesAssigned) {
                return false;
            }

            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return false;
            }

            const actionsStarted = await startScheduledActions();

            if (!actionsStarted) {
                return false;
            }

            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return false;
            }

            if (autoStopTimer) { clearTimeout(autoStopTimer); }
            if (autoStopSeconds > 0) {
                autoStopTimer = setTimeout(() => stopEnvironment(autoNextWhenDone),
                                        autoStopSeconds * 1000);
                setStatus(`Will auto-stop in ${autoStopSeconds}s`, 'info');
            }
        }

        return true;
    }

    async function startScheduledActions() {
        setStatus('Starting scheduled actions...', 'info');

        try {
            const response = await fetch('/start_actions', {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json'
                }
            });
            const data = await response.json();

            if (data.status === 'success') {
                setStatus(data.message || 'Scheduled actions started.', 'success');
                return true;
            }

            setStatus(`Error starting scheduled actions: ${data.error}`, 'error');
            return false;
        } catch (error) {
            setStatus(`Error starting scheduled actions: ${error}`, 'error');
            return false;
        }
    }

    function mainNode() {
        if (window.current_environment === 'mininet') {
            return 'nat0';
        }

        return 'server';
    }

    function generateGraph(status) {

        try {
            graph = buildGraph(status);
            nodeIps = graph.getIpMappingFrom(mainNode());
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
            const response = await fetch('/list_experiments', { cache: 'no-store' });
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
            const savedExperimentName = getSavedExperimentName();
            const savedExperimentIndex = savedExperimentName ? experiments.indexOf(savedExperimentName) : -1;

            if (savedExperimentName && savedExperimentIndex === -1) {
                clearSavedExperimentName();
            }

            const selectedIndex = savedExperimentIndex >= 0 ? savedExperimentIndex : 0;
            const selectedExperiment = experiments[selectedIndex];

            experimentSelect.value = String(selectedIndex);
            saveExperimentName(selectedExperiment);
            loadExperiment(selectedExperiment);
        }

    }

    async function loadExperiment(experiment) {
        setStatus(`Loading experiment: ${experiment}`, 'info');
        try {
            const response = await fetch(`/experiments/${experiment}`, { cache: 'no-store' });
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

            // Store the experiment in global state
            window.current_experiment_name = experiment;
            window.current_experiment = parsedYaml;
            window.current_environment = environment.name;

            setStatus('Experiment loaded.', 'success');
        } catch (error) {
            setStatus(`Error loading experiment: ${error}`, 'error');
        }
    }

    async function startEnvironment(options = {}) {
        const continueAutoSweep = options?.continueAutoSweep === true;
        resetExperimentRecoveryState();
        const attemptId = ++startEnvironmentAttemptId;
        routerMetricsTargetsStarted = new Set();

        let autoStopSeconds   = parseInt(
            document.getElementById('autoStopTime').value || '0', 10
        );
        if (isNaN(autoStopSeconds) || autoStopSeconds < 0) {
            autoStopSeconds = 0;
        }
        autoNextWhenDone = isAutomaticExperimentModeEnabled();

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

        if (autoNextWhenDone) {
            if (continueAutoSweep) {
                ensureAutoSweepState(selectedExperimentIndex);
            } else {
                configureAutoSweepState(selectedExperimentIndex);
            }
        } else {
            resetAutoSweepState();
        }

        saveExperimentName(selectedExperiment);
        
        await loadExperiment(selectedExperiment);   

        const payload = {
            experimentName: window.current_experiment_name,
            environment: window.current_environment,
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
            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return;
            }

            if (data.status === 'success') {
                armExperimentRecoveryWatchdog(
                    attemptId,
                    window.current_experiment_name,
                    window.current_experiment
                );
                await finalizeEnvironmentStart(attemptId, data.message, 'success', autoStopSeconds);
            } else {
                await attemptStartFailureRecovery(
                    attemptId,
                    data.error || 'Unknown environment start error',
                    autoStopSeconds,
                    'Environment became available after an initial start error. Continuing...'
                );
            }
        } catch (error) {
            if (!isCurrentStartEnvironmentAttempt(attemptId)) {
                return;
            }

            console.warn('start_environment fetch failed, checking runtime status:', error);
            await attemptStartFailureRecovery(
                attemptId,
                String(error),
                autoStopSeconds,
                'Environment started, but the start response was interrupted. Continuing...'
            );
        }
    }


    function normalizeStopEnvironmentOptions(launchNextOrOptions, maybeOptions = undefined) {
        if (typeof launchNextOrOptions === 'object' && launchNextOrOptions !== null) {
            return {
                launchNext: Boolean(launchNextOrOptions.launchNext),
                resetRecovery: launchNextOrOptions.resetRecovery !== false,
            };
        }

        return {
            launchNext: Boolean(launchNextOrOptions),
            resetRecovery: maybeOptions?.resetRecovery !== false,
        };
    }

    async function stopEnvironment(launchNextOrOptions = false, maybeOptions = undefined) {
        const { launchNext, resetRecovery } = normalizeStopEnvironmentOptions(launchNextOrOptions, maybeOptions);
        if (autoStopTimer) { clearTimeout(autoStopTimer); }
        autoStopTimer = null;
        if (resetRecovery) {
            resetExperimentRecoveryState();
        } else {
            clearExperimentRecoveryTimers();
        }
        if (!launchNext && resetRecovery) {
            resetAutoSweepState();
        }
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
                routerMetricsTargetsStarted = new Set();
                networkSections.classList.add('hidden');
                serverSections.classList.add('hidden');
                if (launchNext) {
                    console.log('We need to launch the next experiment');
                    const currentIndex   = parseInt(experimentSelect.value || "0", 10);
                    if (isNaN(currentIndex)) {
                        setStatus('No current experiment selected.', 'warning');
                        return;
                    }
                    const nextExperiment = getNextAutoExperimentIndex(currentIndex);
    
                    if (nextExperiment && experiments[nextExperiment.index]) {
                        experimentSelect.value = String(nextExperiment.index);
                        saveExperimentName(experiments[nextExperiment.index]);
                        if (nextExperiment.startsNewSweep) {
                            setStatus(
                                `Starting sweep ${nextExperiment.sweepNumber}. ${nextExperiment.remainingAdditionalSweeps} additional sweep${nextExperiment.remainingAdditionalSweeps === 1 ? '' : 's'} remaining.`,
                                'info'
                            );
                        }
                        // Give the UI a tick to update before we click start
                        setTimeout(() => startEnvironment({ continueAutoSweep: true }), 300);
                    } else {
                        const completedSweeps = autoSweepState?.currentSweep || 1;
                        resetAutoSweepState();
                        setStatus(`All experiments finished after ${completedSweeps} sweep${completedSweeps === 1 ? '' : 's'}`, 'info');
                    }
                }
                return true;
            } else {
                setStatus(`Error: ${data.error}`, 'error');
                return false;
            }
        } catch (error) {
            setStatus(`Error stopping environment: ${error}`, 'error');
            return false;
        }
    }

    experimentSelect.addEventListener('change', async () => {
        resetExperimentRecoveryState();
        const selectedExperimentIndex = parseInt(experimentSelect.value || '-1', 10);
        if (selectedExperimentIndex < 0 || selectedExperimentIndex >= experiments.length) {
            return;
        }

        const selectedExperiment = experiments[selectedExperimentIndex];
        if (!selectedExperiment) {
            return;
        }

        saveExperimentName(selectedExperiment);
        await loadExperiment(selectedExperiment);
    });

    startBtn.addEventListener('click', async () => startEnvironment());
    stopBtn.addEventListener('click', async () => stopEnvironment(false));
    autoNextExperimentCheckbox?.addEventListener('change', syncAutoSweepRepeatInputState);
    autoSweepRepeatCountInput?.addEventListener('change', () => {
        normalizeAutoSweepRepeatCountInput();
        syncAutoSweepRepeatInputState();
    });
    syncAutoSweepRepeatInputState();

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

                let mNode = mainNode();

                status.links.forEach(link => {
                    if (link.node1 === mNode && link.ip1 !== 'N/A') {
                        natIp = link.ip1;
                    } else if (link.node2 === mNode && link.ip2 !== 'N/A') {
                        natIp = link.ip2;
                    }
                });

                if (natIp) {
                    setStatus(`Main IP: ${natIp}`, 'info');
                    if (start_agents) {
                        return await startAgents(status.nodes, natIp);
                    } else {
                        checkAgentsConnected(window.current_experiment);
                    }
                    return true;
                } else {
                    setStatus('Main IP not found.', 'warning');
                    console.log('Main node:', mNode);
                    console.log('Links:', status.links);
                    console.log('Nodes:', status.nodes);
                }
            } else {
                setStatus('Failed to fetch nodes and links.', 'warning');
            }
        } catch (error) {
            setStatus(`Error fetching nodes and links: ${error}`, 'error');
        }

        return false;
    }

    function maybeParseJsonString(value) {
        if (typeof value !== 'string') {
            return value;
        }
        try {
            return JSON.parse(value);
        } catch (_error) {
            return value;
        }
    }

    function shellQuoteSingle(value) {
        return `'${String(value).replace(/'/g, `'"'"'`)}'`;
    }

    async function execRemoteCommand(nodeName, command, background = false) {
        const params = new URLSearchParams({
            node: nodeName,
            command,
            background: background ? 'true' : 'false',
        });

        const response = await fetch(`/exec?${params.toString()}`);
        const data = await response.json();
        if (!response.ok || data.status !== 'success') {
            throw new Error(data.error || data.message || response.statusText || `Remote exec failed on ${nodeName}`);
        }

        return {
            raw: data,
            message: maybeParseJsonString(data.message),
        };
    }

    async function cleanupEnvironmentProcesses() {
        const response = await fetch('/cleanup_environment_processes', {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json'
            }
        });
        const data = await response.json();
        if (!response.ok || data.status !== 'success') {
            throw new Error(data.error || data.message || 'Environment process cleanup failed');
        }
        return data.message;
    }

    async function collectAgentLaunchDiagnostics(missingAgents) {
        const uniqueMissingAgents = [...new Set((missingAgents || []).filter(Boolean))];
        if (uniqueMissingAgents.length === 0) {
            return;
        }

        console.error('Collecting agent launch diagnostics for missing agents:', uniqueMissingAgents);

        await Promise.allSettled(uniqueMissingAgents.map(async (nodeName) => {
            const launchRecord = agentLaunchRecords.get(nodeName);
            if (!launchRecord) {
                console.error(`No launch record found for ${nodeName}`);
                return;
            }

            console.error(`Agent launch record for ${nodeName}:`, launchRecord);

            const commands = [];
            if (launchRecord.stderr) {
                commands.push(`printf '%s\\n' ${shellQuoteSingle(`STDERR (${launchRecord.stderr})`)}; tail -n 80 ${shellQuoteSingle(launchRecord.stderr)} 2>/dev/null || true`);
            }
            if (launchRecord.stdout) {
                commands.push(`printf '%s\\n' ${shellQuoteSingle(`STDOUT (${launchRecord.stdout})`)}; tail -n 80 ${shellQuoteSingle(launchRecord.stdout)} 2>/dev/null || true`);
            }

            if (commands.length === 0) {
                return;
            }

            try {
                const result = await execRemoteCommand(nodeName, commands.join(`; printf '\\n'; `), false);
                console.error(`Agent launch diagnostics for ${nodeName}:\n${result.message || '<empty>'}`);
            } catch (error) {
                console.error(`Failed to collect agent launch diagnostics for ${nodeName}:`, error);
            }
        }));
    }

    // Start the agents on the nodes and the routers
    async function startAgents(nodes, natIp) {
        setStatus('Starting agents...', 'info');
        agentLaunchRecords = new Map();

        const AGENTS_ONLY_ON_REFERENCED_NODES = true;
        const DISABLE_MININET_TUNNELS = true;
        const controllerPort = parseInt(window.location.port || '3000', 10);
        const controllerHost = window.location.hostname || '127.0.0.1';
        const env = window.current_environment || '';
        const urlParams = new URLSearchParams(window.location.search);
        const tunnelHostOverride = urlParams.get('tunnelHost') || urlParams.get('controllerHost');
        const tunnelTargetHost = tunnelHostOverride || controllerHost;
        console.log(`startAgents: env=${env} controllerHost=${controllerHost} controllerPort=${controllerPort} tunnelTargetHost=${tunnelTargetHost}`);

        const targetNodes = nodes.filter(
            node => {
                const supportedType =
                    node.type === 'EdgeNode' ||
                    node.type === 'LinuxRouter' ||
                    node.type === 'VirtualWall';
                if (!supportedType || disallowedTargets.has(node.name)) {
                    return false;
                }

                if (!AGENTS_ONLY_ON_REFERENCED_NODES) {
                    return true;
                }

                // Start agents only on nodes that are explicitly referenced by roles.
                // This avoids launching on every GEANT router when only a subset is used.
                const roles = window.current_experiment?.environment?.roles || [];
                const roleTargets = new Set(
                    roles
                        .map((role) => role?.target)
                        .filter((target) => typeof target === 'string' && target.length > 0)
                );

                if (roleTargets.size === 0) {
                    return true;
                }

                return roleTargets.has(node.name);
            }
        ).sort((a, b) => a.name.localeCompare(b.name));

        let releasearg = '';
        if (urlParams.get('release') === 'true') {
            releasearg = '--release';
        }

        const shouldResetTcOnStartup =
            env === 'virtualwall' ||
            env === 'virtualwalllite' ||
            env === 'bigvirtualwall';

        if (env === 'mininet') {
            setStatus('Cleaning leaked Mininet host processes before starting agents...', 'info');
            try {
                const cleanupMessage = await cleanupEnvironmentProcesses();
                console.log(`Mininet host cleanup completed: ${cleanupMessage}`);
            } catch (error) {
                console.error('Failed to clean leaked Mininet host processes before agent startup:', error);
                setStatus(`Failed to clean leaked Mininet host processes: ${error}`, 'error');
                return false;
            }
            setStatus('Starting agents...', 'info');
        }

        // Best-effort tunnel opener (remote forward: node listens, forwards to controller).
        async function ensureTunnel(nodeName) {
            try {
                // Use explicit override when provided; otherwise the controller host where UI is served.
                let targetHost = tunnelTargetHost;
                if (!tunnelHostOverride && env === 'mininet' && natIp) {
                    targetHost = natIp;
                }
                const payload = {
                    node: nodeName,
                    direction: 'remote',
                    listen_host: '127.0.0.1',
                    listen_port: `${controllerPort}`,
                    target_host: targetHost,
                    target_port: `${controllerPort}`
                };
                const resp = await fetch('/tunnels/open', {
                    method: 'POST',
                    headers: { 'Content-Type': 'application/json' },
                    body: JSON.stringify(payload)
                });
                const data = await resp.json();
                if (resp.ok && data.status !== 'error') {
                    console.log(`Tunnel for ${nodeName} ready`, data.tunnel || data);
                    return true;
                }
                console.warn(`Tunnel for ${nodeName} failed:`, data);
                setStatus(`Tunnel for ${nodeName} failed: ${data.error || resp.statusText}`, 'warning');
            } catch (err) {
                console.warn(`Tunnel request for ${nodeName} failed:`, err);
                setStatus(`Tunnel for ${nodeName} failed: ${err}`, 'warning');
            }
            return false;
        }

        const launchResults = await Promise.allSettled(
            targetNodes.map(async (node) => {
                let agentUrl = `http://${natIp}:${controllerPort}`;
                let runPath = env === 'mininet' ? '../../run.sh' : './MultipathXR/run.sh';

                // Hard-disable tunnels for mininet; keep existing behavior for other environments.
                const shouldAttemptTunnel =
                    env === 'virtualwall' ||
                    env === 'virtualwalllite' ||
                    env === 'bigvirtualwall' ||
                    (env === 'mininet' && !DISABLE_MININET_TUNNELS);

                if (shouldAttemptTunnel) {
                    const ok = await ensureTunnel(node.name);
                    if (ok) {
                        // Wait a short time to ensure tunnel is ready
                        await new Promise(resolve => setTimeout(resolve, 1000));
                        agentUrl = `http://127.0.0.1:${controllerPort}`;
                    }
                }

                if (env !== 'mininet') {
                    await execRemoteCommand(node.name, 'sudo -n true', false);

                    const killCommand = `sudo -n sh -lc "pkill -x pc-agent || true; pkill -x pc-receiver || true; pkill -x cdn_proxy || true; pkill -x pc-server || true; pkill -x metrics || true"`;
                    console.log("Killing potential programs from previous experiment");
                    await execRemoteCommand(node.name, killCommand, false);
                }

                const sudoPrefix = env === 'mininet' ? 'sudo' : 'sudo -n';
                const agentEnvPrefix = shouldResetTcOnStartup
                    ? 'env PC_AGENT_RESET_TC_ON_STARTUP=1 '
                    : '';
                const command = `${sudoPrefix} ${agentEnvPrefix}${runPath} --agent ${releasearg} --url ${agentUrl} --node-id ${node.name}`;
                console.log(`Starting agent on ${node.name} with command: ${command}`);

                const launchResult = await execRemoteCommand(node.name, command, true);
                const launchMessage = launchResult.message;
                const launchRecord = (launchMessage && typeof launchMessage === 'object')
                    ? { ...launchMessage, command }
                    : { message: launchMessage, command };
                agentLaunchRecords.set(node.name, launchRecord);
                console.log(`Background agent launch accepted on ${node.name}:`, launchRecord);
            })
        );

        const launchFailures = launchResults
            .filter(result => result.status === 'rejected')
            .map(result => result.reason instanceof Error ? result.reason.message : String(result.reason));

        if (launchFailures.length > 0) {
            console.error('Agent launch preflight failures:', launchFailures);
            setStatus(`Agent launch failed before connect: ${launchFailures[0]}`, 'error');
            return false;
        }

        const agentsStarted = await checkAgentsConnected(window.current_experiment);
        if (!agentsStarted) {
            setStatus('Error starting agents.', 'error');
            return false;
        }

        setStatus('Agents started.', 'success');
        return true;
    }

    /**
     * Sleep for the given amount of time.
     */
    function delay(ms) {
        return new Promise((resolve) => setTimeout(resolve, ms));
    }

    /**
     * Run async work on items with bounded concurrency.
     */
    async function mapWithConcurrency(
        items,
        concurrency,
        worker,
        natIp, controllerPort, releasearg, env
    ) {
        if (!Number.isInteger(concurrency) || concurrency <= 0) {
            throw new Error(`Invalid concurrency: ${concurrency}`);
        }

        const results = new Array(items.length);
        let nextIndex = 0;

        async function runWorker() {
            while (true) {
                const currentIndex = nextIndex;
                nextIndex += 1;

                if (currentIndex >= items.length) {
                    return;
                }

                try {
                    const value = await worker(items[currentIndex], natIp, controllerPort, releasearg, env);
                    results[currentIndex] = { status: 'fulfilled', value };
                } catch (error) {
                    results[currentIndex] = { status: 'rejected', reason: error };
                }
            }
        }

        const workerCount = Math.min(concurrency, items.length);
        await Promise.all(Array.from({ length: workerCount }, () => runWorker()));

        return results;
    }

    async function checkAgentsConnected(experiment, timeoutMs = 30000, pollIntervalMs = 2000) {
        if (!experiment || !experiment.environment) {
            return false;
        }

        const start = Date.now();
    
        // Extract roles from the experiment object
        const roles = experiment.environment.roles || [];
        const requiredAgents = roles.map(role => role.target).filter(target => !disallowedTargets.has(target));

        if (requiredAgents.length === 0) {
            console.log('No agents to start.');
            return true;
        }

        console.log(requiredAgents);
        let lastMissingAgents = [...requiredAgents];
    
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
    
                // Check which agents are not connected
                const missingAgents = requiredAgents.filter(agent => {
                    const socketId = agentsData[agent];
                    return !(socketId && connectedSockets.has(socketId));
                });
                lastMissingAgents = missingAgents;
    
                if (missingAgents.length === 0) {
                    console.log('All required agents are connected.');
                    setStatus('All required agents are connected.', 'success'); 
                    return true;
                } else {
                    console.log('Waiting for agents to connect. Missing:', missingAgents);
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
        await collectAgentLaunchDiagnostics(lastMissingAgents);
        setStatus('Timeout reached. Not all agents are connected. Check the browser console for remote launch diagnostics.', 'error');
        return false;
    }

    async function sendAgentCommand(target, command) {
        const params = new URLSearchParams({ node_id: target, command });
        try {
            const response = await fetch(`/exec_on_agent?${params.toString()}`);
            const data = await response.json();
            if (data.status === 'success') {
                console.log(`Command executed on ${target}: ${command}`);
                return true;
            }
            console.error(`Failed to execute command on ${target}:`, data.error);
            return false;
        } catch (error) {
            console.error(`Error executing command on ${target}:`, error);
            return false;
        }
    }

    async function ensureRouterMetricsProcess(role, commandBase, releasearg, port = 'DYNAMIC_PORT') {
        const target = role?.target;
        if (!target || disallowedTargets.has(target)) {
            return true;
        }

        if (routerMetricsTargetsStarted.has(target)) {
            return true;
        }

        const command = `${commandBase} --metrics ${releasearg} --port ${port} --log-level ${currentLogLevel}`;
        const success = await sendAgentCommand(target, command);
        if (!success) {
            console.error(`Failed to start router metrics for ${role.role} role ${role.alias || target} on ${target}`);
            return false;
        }

        routerMetricsTargetsStarted.add(target);
        return true;
    }

    function parseNodeIndex(nodeId) {
        if (typeof nodeId !== 'string') return null;
        const m = nodeId.match(/^n(\d+)$/i);
        if (!m) return null;
        const idx = parseInt(m[1], 10);
        return Number.isFinite(idx) && idx > 0 ? idx : null;
    }

    function resolveMulticastGroupIndex(role, statusData) {
        // 1) Server role itself always uses its own node id.
        const fromOwnTarget = parseNodeIndex(role?.target);
        if (role?.role === 'server' && fromOwnTarget !== null) {
            return fromOwnTarget;
        }

        // 2) If this role points to a specific HTTP IP, try to map back to a node id.
        const httpIp = role?.http_ip || null;
        if (httpIp) {
            const mappedNode = Object.keys(nodeIps || {}).find((k) => nodeIps[k] === httpIp);
            const mappedIdx = parseNodeIndex(mappedNode);
            if (mappedIdx !== null) return mappedIdx;

            // Fallback to status.links if nodeIps map did not contain the endpoint.
            const links = statusData?.links || [];
            for (const link of links) {
                if (link.ip1 === httpIp) {
                    const idx = parseNodeIndex(link.node1);
                    if (idx !== null) return idx;
                }
                if (link.ip2 === httpIp) {
                    const idx = parseNodeIndex(link.node2);
                    if (idx !== null) return idx;
                }
            }
        }

        // 3) Common case: single server role in experiment.
        const allRoles = window.current_experiment?.environment?.roles || [];
        const serverRoles = allRoles.filter((r) => r && r.role === 'server');
        if (serverRoles.length === 1) {
            const idx = parseNodeIndex(serverRoles[0].target);
            if (idx !== null) return idx;
        }

        return null;
    }

    async function assignRole(role, statusData) {
        const commandBase = '../run.sh';
        let command = null;
        const serverPort = 3001;
        const websocketPort = serverPort;
        const dynamicPort = 'DYNAMIC_PORT';
        let releasearg = '';
        // If the url contains the query string "debug=true", set releasearg to empty string
        const urlParams = new URLSearchParams(window.location.search);
        if (urlParams.get('release') === 'true') {
            releasearg = '--release';
        }

        const nodeDefaultIp = nodeIps[role.target] || '127.0.0.1';
        const httpIp = role.http_ip || nodeDefaultIp;
        const websocketIp = role.websocket_ip || httpIp;
        const multicastGroupIndex = resolveMulticastGroupIndex(role, statusData);

        if (role.role === 'router') {
            let port = dynamicPort;
            return await ensureRouterMetricsProcess(role, commandBase, releasearg, port);
        } else if (role.role === 'server') {
            const fluteEndpointArg = multicastGroupIndex ? ` --flute-endpoint-url 239.0.${multicastGroupIndex}.1` : '';
            command = `${commandBase} --server ${releasearg} --port ${serverPort} --log-level ${currentLogLevel}${fluteEndpointArg}`;
        } else if (role.role === 'client') {
            const disableParser = role.disable_parser ? '--disable-parser ' : '';
            console.log(`Server IP for client ${role.alias}: ${httpIp}`);
            if (!httpIp) {
                console.error(`Failed to find server IP for client: ${role.alias}`);
                return false;
            }
            console.log(`WebSocket IP for client ${role.alias}: ${websocketIp}`);
            if (!websocketIp) {
                console.error(`Failed to find WebSocket IP for client: ${role.alias}`);
                return false;
            }
            let port = dynamicPort;

            const multicastUrlArg = multicastGroupIndex ? ` --multicast-url udp://239.0.${multicastGroupIndex}.1:40085` : '';
            if (role.visible) {
                command = `${commandBase} --client ${releasearg} --port ${port} --http-url http://${httpIp}:${serverPort} --websocket-url http://${websocketIp}:${websocketPort}${multicastUrlArg} ${disableParser}--log-level ${logLevelMap[currentLogLevel] ?? '2'}`;
            } else {
                command = `${commandBase} --client ${releasearg} --headless --port ${port} --http-url http://${httpIp}:${serverPort} --websocket-url http://${websocketIp}:${websocketPort}${multicastUrlArg} ${disableParser}--log-level ${currentLogLevel}`;
            }
        } else if (role.role === 'proxy') {
            if (!httpIp) {
                console.error(`Failed to find server IP for proxy: ${role.alias}`);
                return false;
            }
            if (!await ensureRouterMetricsProcess(role, commandBase, releasearg, dynamicPort)) {
                return false;
            }
            command = `${commandBase} --proxy ${releasearg} --origin-base-url http://${httpIp}:${serverPort} --listen-addr 0.0.0.0:${serverPort} --log-level ${currentLogLevel}`;
        } else if (role.role === 'nothing') {
            return true;
        } else {
            console.warn(`Unknown role: ${role.role}`);
            return false;
        }

        if (!command) {
            return true;
        }

        const success = await sendAgentCommand(role.target, command);
        if (!success) {
            console.error(`Failed to assign role ${role.role} to ${role.target}`);
        }
        return success;
    }

    async function giveRoles(experiment) {
        if (!experiment || !experiment.environment || !experiment.environment.roles) {
            setStatus('No roles to assign.', 'warning');
            return true;
        }
    
        const roles = expandRolesWithClientProxy(experiment.environment?.roles);
        const statusData = await fetchStatus();
    
        if (!statusData) {
            setStatus('Failed to fetch status for assigning roles.', 'error');
            return false;
        }
    
        setStatus('Assigning roles...', 'info');

                const rolePriority = {
                        router: 1,
                        server: 2,
                        relay: 3,
                        proxy: 4,
                        client: 5,
                        nothing: 6
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
                return false;
            } else {
                // Wait for a short time before assigning the next role
                await new Promise(resolve => setTimeout(resolve, 1000));
            }
        }
    
        setStatus('Roles assigned successfully.', 'success');
        return true;
    }

    function setStatus(message, level) {
        const normalizedLevel = normalizeStatusLevel(level);
        const normalizedMessage = String(message ?? '').trim();
        currentStatusState = {
            message: normalizedMessage,
            level: normalizedLevel,
            timestampMs: Date.now(),
        };
        statusBar.textContent = normalizedMessage;
        statusBar.className = `status-bar ${normalizedLevel}`;

        console.log(`[${normalizedLevel.toUpperCase()}] ${normalizedMessage}`);
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

    /**
     * Coerce a YAML/JSON value into a boolean (accepts booleans and "true"/"false" strings).
     * @param {unknown} v
     * @returns {boolean|undefined}
     */
    function coerceBool(v) {
        if (typeof v === "boolean") return v;
        if (typeof v === "string") {
            const s = v.trim().toLowerCase();
            if (s === "true") return true;
            if (s === "false") return false;
        }
        return undefined;
    }

    /**
     * Coerce a YAML/JSON value into a positive integer count.
     * @param {unknown} v
     * @returns {number|undefined}
     */
    function coercePositiveInt(v) {
        if (typeof v === 'number' && Number.isInteger(v) && v > 0) {
            return v;
        }
        if (typeof v === 'string') {
            const parsed = Number.parseInt(v.trim(), 10);
            if (Number.isInteger(parsed) && parsed > 0) {
                return parsed;
            }
        }
        return undefined;
    }

    /**
     * Expand roles so a counted client role becomes multiple concrete client launches on the same node,
     * while a `client` with `proxy: true` still spawns a single colocated proxy role.
     *
     * @param {unknown} rolesRaw
     * @returns {RoleConfig[]}
     */
    function expandRolesWithClientProxy(rolesRaw) {
        if (!Array.isArray(rolesRaw)) return [];

        /** @type {RoleConfig[]} */
        const out = [];

        for (const r of rolesRaw) {
            if (!r || typeof r !== "object") continue;

            const role = typeof r.role === "string" ? r.role : String(r.role ?? "");
            const target = typeof r.target === "string" ? r.target : String(r.target ?? "");
            const alias = typeof r.alias === "string" ? r.alias : String(r.alias ?? "");
            const count = coercePositiveInt(r.count) ?? 1;

            // Legacy: for backwards compatiblity.
            // We use http_ip and websocket_ip for the client to connect to the server/relay now.
            const server_ip = typeof r.server_ip === "string" ? r.server_ip : String(r.server_ip ?? "");
            if (server_ip.length > 0 && !r.http_ip) {
                r.http_ip = server_ip;
            }

            // If the websocket ip is not set, we default it to the http_ip (if set).
            const http_ip = typeof r.http_ip === "string" ? r.http_ip : String(r.http_ip ?? "");
            if (http_ip.length > 0 && !r.websocket_ip) {
                r.websocket_ip = http_ip;
            }

            if (!role || !target || !alias) {
                // Keep behavior predictable: if malformed, pass through as-is so you can surface an error elsewhere.
                // (Or skip entirely if your current UI already does strict validation.)
                // @ts-ignore
                out.push(r);
                continue;
            }

            const baseRole = { ...r };
            const proxyEnabled = role === "client" && coerceBool(baseRole.proxy) === true;

            if (proxyEnabled) {
                /** @type {RoleConfig} */
                const proxyRole = {
                    // @ts-ignore
                    ...baseRole,
                    role: "proxy",
                    target,
                    alias: `${alias}_proxy`,
                };
                delete proxyRole.count;
                out.push(proxyRole);

                if (baseRole.http_ip) {
                    baseRole.http_ip = "0.0.0.0";
                }
                delete baseRole.proxy;
            }

            for (let index = 0; index < count; index++) {
                /** @type {RoleConfig} */
                const expandedRole = {
                    // @ts-ignore
                    ...baseRole,
                    alias: index === 0 ? alias : `${alias}_${index + 1}`,
                };
                delete expandedRole.count;
                out.push(expandedRole);
            }
        }

        return out;
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
