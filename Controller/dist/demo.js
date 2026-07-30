(function () {
  const $ = (id) => document.getElementById(id);
  const status = $("status");

  const nodeId = $("nodeId");
  const iface = $("iface");
  const endpoint = $("endpoint");

  const bw = $("bw");
  const lat = $("lat");
  const loss = $("loss");
  const bwVal = $("bwVal");
  const latVal = $("latVal");
  const lossVal = $("lossVal");

  const applyBtn = $("apply");
  const resetBtn = $("reset");

  // ---- Config: instances & metrics ----
  const OBJECT_ONE_MAX_POINTS = 15000;
  const STREAM_A_HINT = "_sid_client_1_";
  const STREAM_B_HINT = "_sid_flute_";

  // Metric names
  const M_BROADCAST = "n1_eth1_tx_bytes"; // server, counter (bytes)
  const M_UNICAST = "n1_eth2_tx_bytes";   // server, counter (bytes)
  const M_POINTS = "total_point_count";   // client, gauge (points)
  const M_CPU = "cpu_usage";              // client, gauge (%)
  const M_MEM = "memory_usage";           // client, gauge (bytes)
  const M_FRAMES_PER_STREAM = "frames_received_total_per_stream"; // Client per stream, counter (frames)
  const M_LATENCY_PER_STREAM = "send_to_receive_time_diff_per_stream"; // client per stream, gauge (microseconds)

  let isRunning = false;
  let metricTargets = null;

  function sortInterfaceNames(left, right) {
    const leftMatch = String(left).match(/-eth(\d+)$/);
    const rightMatch = String(right).match(/-eth(\d+)$/);
    const leftIndex = leftMatch ? Number.parseInt(leftMatch[1], 10) : Number.POSITIVE_INFINITY;
    const rightIndex = rightMatch ? Number.parseInt(rightMatch[1], 10) : Number.POSITIVE_INFINITY;
    if (leftIndex !== rightIndex) {
      return leftIndex - rightIndex;
    }
    return String(left).localeCompare(String(right));
  }

  async function fetchStatusData() {
    const res = await fetch("/status", { cache: "no-store" });
    if (!res.ok) throw new Error(`GET /status → ${res.status}`);
    return res.json();
  }

  async function fetchCurrentExperiment() {
    const res = await fetch("/current_experiment", { cache: "no-store" });
    if (!res.ok) throw new Error(`GET /current_experiment → ${res.status}`);
    const json = await res.json();
    return json?.experiment ?? null;
  }

  function buildRoleTargetMap(experiment) {
    const roles = Array.isArray(experiment?.environment?.roles) ? experiment.environment.roles : [];
    const map = new Map();
    for (const role of roles) {
      if (!role || typeof role.target !== "string") continue;
      map.set(role.target, role.target);
      if (typeof role.alias === "string" && role.alias.length > 0) {
        map.set(role.alias, role.target);
      }
    }
    return map;
  }

  function normalizeRoleReference(reference, roleTargetMap) {
    if (typeof reference !== "string" || !reference.length) {
      return null;
    }
    return roleTargetMap.get(reference) || reference;
  }

  function resolveTrafficControlClientTarget(experiment) {
    const roleTargetMap = buildRoleTargetMap(experiment);
    const actions = Array.isArray(experiment?.actions) ? experiment.actions : [];
    const tcActions = actions.filter((action) => action && action.type === "tc");
    const unicastAction = tcActions.find((action) =>
      /unicast/i.test(action?.action || "") || /unicast/i.test(action?.target || "")
    );

    const fromAction = normalizeRoleReference(unicastAction?.connected_node, roleTargetMap);
    if (fromAction) {
      return fromAction;
    }

    const roles = Array.isArray(experiment?.environment?.roles) ? experiment.environment.roles : [];
    const visibleClient = roles.find((role) => role?.role === "client" && role?.visible === true);
    if (visibleClient?.target) {
      return visibleClient.target;
    }

    const firstClient = roles.find((role) => role?.role === "client" && typeof role?.target === "string");
    return firstClient?.target || null;
  }

  function resolveClientAttachedUnicastHop(statusData, clientNodeId) {
    const links = Array.isArray(statusData?.links) ? statusData.links : [];
    const candidates = links
      .map((link) => {
        if (!link || link.status !== "up") {
          return null;
        }

        if (
          link.node1 === clientNodeId &&
          typeof link.ip1 === "string" &&
          link.ip1.startsWith("13.") &&
          typeof link.node2 === "string" &&
          /^r/i.test(link.node2)
        ) {
          return { routerNodeId: link.node2, routerInterface: link.intf2 };
        }

        if (
          link.node2 === clientNodeId &&
          typeof link.ip2 === "string" &&
          link.ip2.startsWith("13.") &&
          typeof link.node1 === "string" &&
          /^r/i.test(link.node1)
        ) {
          return { routerNodeId: link.node1, routerInterface: link.intf1 };
        }

        return null;
      })
      .filter(Boolean)
      .sort((left, right) => sortInterfaceNames(left.routerInterface, right.routerInterface));

    return candidates[0] ?? null;
  }

  function resolveUnicastRouterTarget(experiment) {
    const roleTargetMap = buildRoleTargetMap(experiment);
    const actions = Array.isArray(experiment?.actions) ? experiment.actions : [];
    const tcActions = actions.filter((action) => action && action.type === "tc");
    const unicastAction = tcActions.find((action) =>
      /unicast/i.test(action?.action || "") || /unicast/i.test(action?.target || "")
    );
    const fromAction = normalizeRoleReference(unicastAction?.target, roleTargetMap);
    if (fromAction) {
      return fromAction;
    }

    const roles = Array.isArray(experiment?.environment?.roles) ? experiment.environment.roles : [];
    const unicastRouter = roles.find((role) =>
      role?.role === "router" && /unicast/i.test(role?.alias || "")
    );
    if (unicastRouter?.target) {
      return unicastRouter.target;
    }

    const firstRouter = roles.find((role) => role?.role === "router" && typeof role?.target === "string");
    return firstRouter?.target || null;
  }

  function resolvePreferredRouterInterface(statusData, targetNodeId) {
    const links = Array.isArray(statusData?.links) ? statusData.links : [];
    const routerInterfaces = links
      .flatMap((link) => {
        if (link?.node1 === targetNodeId) return [link.intf1];
        if (link?.node2 === targetNodeId) return [link.intf2];
        return [];
      })
      .filter((name) => typeof name === "string" && name.startsWith(`${targetNodeId}-eth`))
      .sort(sortInterfaceNames);

    if (!routerInterfaces.length) {
      return `${targetNodeId}-eth1`;
    }

    const preferred = `${targetNodeId}-eth1`;
    return routerInterfaces.includes(preferred) ? preferred : routerInterfaces[0];
  }

  async function syncTrafficControlTarget() {
    const [experiment, statusData] = await Promise.all([
      fetchCurrentExperiment(),
      fetchStatusData(),
    ]);

    const clientNodeId = resolveTrafficControlClientTarget(experiment);
    const clientAttachedHop = clientNodeId
      ? resolveClientAttachedUnicastHop(statusData, clientNodeId)
      : null;

    if (clientAttachedHop) {
      nodeId.value = clientAttachedHop.routerNodeId;
      iface.value = clientAttachedHop.routerInterface || `${clientAttachedHop.routerNodeId}-eth1`;
      return;
    }

    const targetNodeId = resolveUnicastRouterTarget(experiment);
    if (!targetNodeId) {
      throw new Error("Unable to resolve the active unicast router target");
    }

    nodeId.value = targetNodeId;
    iface.value = resolvePreferredRouterInterface(statusData, targetNodeId);
  }

  // returns true iff backend says {"status":"running"}
  async function fetchRunning() {
    try {
      const res = await fetch("/status", { cache: "no-store" });
      if (!res.ok) throw new Error(`GET /status → ${res.status}`);
      const data = await res.json();
      return data && data.status === "running";
    } catch (e) {
      // Treat network errors like "not running"
      console.debug("status poll failed:", e);
      return false;
    }
  }

  async function fetchMetricInstances() {
    const res = await fetch("/debug/metrics/instances", { cache: "no-store" });
    if (!res.ok) throw new Error(`GET /debug/metrics/instances → ${res.status}`);
    const json = await res.json();
    return Array.isArray(json.data) ? json.data : [];
  }

  async function fetchMetricsForInstance(instance) {
    const url = `/debug/metrics/instance?instance=${encodeURIComponent(instance)}`;
    const res = await fetch(url, { cache: "no-store" });
    if (!res.ok) throw new Error(`GET ${url} → ${res.status}`);
    const json = await res.json();
    const entry = Array.isArray(json.data) ? json.data[0] : null;
    return Array.isArray(entry?.metrics) ? entry.metrics : [];
  }

  function hasAllMetrics(entry, metrics) {
    return metrics.every((metric) => entry.metrics.has(metric));
  }

  function pickInstance(entries, preferredPrefixes, preferredSubstring) {
    const filtered = preferredSubstring
      ? entries.filter((entry) => entry.instance.includes(preferredSubstring))
      : entries;
    const pool = filtered.length ? filtered : entries;
    for (const prefix of preferredPrefixes) {
      const match = pool.find((entry) => entry.instance.startsWith(prefix));
      if (match) return match;
    }
    return pool[0] ?? null;
  }

  async function resolveMetricTargets(force = false) {
    if (metricTargets && !force) return metricTargets;

    const instances = await fetchMetricInstances();
    if (!instances.length) {
      throw new Error("No metric instances are available yet");
    }

    const details = (await Promise.all(instances.map(async (instance) => {
      try {
        const metrics = await fetchMetricsForInstance(instance);
        return { instance, metrics: new Set(metrics) };
      } catch (error) {
        console.debug(`metrics lookup failed for ${instance}:`, error);
        return null;
      }
    }))).filter(Boolean);

    const serverCandidates = details.filter((entry) =>
      hasAllMetrics(entry, [M_BROADCAST, M_UNICAST])
    );
    const clientCandidates = details.filter((entry) =>
      !entry.instance.includes("_sid_") && hasAllMetrics(entry, [M_POINTS])
    );
    const streamCandidates = details.filter((entry) =>
      entry.instance.includes("_sid_") && hasAllMetrics(entry, [M_FRAMES_PER_STREAM, M_LATENCY_PER_STREAM])
    );

    const server = pickInstance(serverCandidates, ["prom__", "agent__"]);
    const client = pickInstance(clientCandidates, ["agent__", "prom__"]);
    const streamA = pickInstance(streamCandidates, ["agent__", "prom__"], STREAM_A_HINT)
      || pickInstance(streamCandidates, ["agent__", "prom__"]);
    const remainingStreams = streamCandidates.filter((entry) => entry.instance !== streamA?.instance);
    const streamB = pickInstance(remainingStreams, ["agent__", "prom__"], STREAM_B_HINT)
      || pickInstance(remainingStreams, ["agent__", "prom__"]);

    if (!server || !client || !streamA || !streamB) {
      throw new Error("Unable to resolve all metric sources from the current instances");
    }

    metricTargets = {
      server: server.instance,
      client: client.instance,
      streamA: streamA.instance,
      streamB: streamB.instance,
    };
    console.debug("Resolved metric targets:", metricTargets);
    return metricTargets;
  }

  // ---- UI helpers ----
  function setStatus(text, kind = "info") {
    status.textContent = text;
    status.classList.remove("ok", "warn", "err");
    if (kind === "ok") status.classList.add("ok");
    if (kind === "warn") status.classList.add("warn");
    if (kind === "err") status.classList.add("err");
  }

  function updateLabels() {
    bwVal.textContent = bw.value;
    latVal.textContent = lat.value;
    lossVal.textContent = Number(loss.value).toFixed(1);
  }

  function payload() {
    const bwStr = `${bw.value}mbit`;
    const latStr = `${lat.value}ms`;
    const lossStr = `${Number(loss.value).toFixed(1)}%`;
    return {
      node_id: nodeId.value.trim(),
      bandwidth: bwStr,
      latency: latStr,
      loss: lossStr,
      interface: iface.value.trim(),
    };
  }

  async function apply() {
    const url = endpoint.value.trim() || "/update_network_conditions";
    try {
      await syncTrafficControlTarget();
    } catch (error) {
      console.debug("traffic control target resolution failed:", error);
    }

    const body = payload();
    if (!body.node_id) {
      setStatus("Missing node_id", "warn");
      return;
    }
    try {
      //setStatus(`POST ${url} → Node ID: ${body.node_id}, Bandwidth: ${body.bandwidth}, Latency: ${body.latency}, Loss: ${body.loss}, Interface: ${body.interface}`);
      const res = await fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      if (res.ok) {
        const json = await res.json();
        const text = json.status === 'success' ? json.message : JSON.stringify(json);
        setStatus(`✔ Applied: ${text}`, "ok");
      } else {
        const json = await res.json().catch(() => res.text());
        const text = typeof json === 'string' ? json : (json.status === 'error' ? json.error : JSON.stringify(json));
        setStatus(`✖ Error ${res.status}: ${text}`, "err");
      }
    } catch (e) {
      setStatus(`Network error: ${e}`, "err");
    }
  }

  function reset() {
    bw.value = 200; // match slider range (max 200 Mbit default)
    lat.value = 0;
    loss.value = 0.0;
    updateLabels();
    setStatus("Values reset.");
    apply();
  }

  [bw, lat, loss].forEach((el) => el.addEventListener("input", updateLabels));
  let timer = null;
  applyBtn.addEventListener("click", apply);
  resetBtn.addEventListener("click", reset);

  // ---- Charts ----
  const WINDOW_SECONDS = 60;
  const BUCKET_MS = 1000;
  const WINDOW_MS = WINDOW_SECONDS * BUCKET_MS;
  const windowSize = WINDOW_SECONDS;
  const labels = Array.from({ length: windowSize }, (_, i) => `${i - (windowSize - 1)}s`);

  const pointsChart = new Chart($("pointsChart"), {
    type: "line",
    data: {
      labels,
      datasets: [
        { label: "Piano player", data: Array(windowSize).fill(null), tension: 0.2, fill: false, borderColor: "#db516fff", backgroundColor: "#9c253fff" },
        { label: "Long dress", data: Array(windowSize).fill(null), tension: 0.2, fill: false, borderColor: "#36A2EB", backgroundColor: "#1d6899ff" },
      ],
    },
    options: {
      animation: false,
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, suggestedMax: 15, title: { display: true, text: "k points" } },
      },
      plugins: { legend: { labels: { color: "#e8ecff" } } },
    },
  });

  const bwChart = new Chart($("bwChart"), {
    type: "line",
    data: {
      labels,
      datasets: [
        { label: "Unicast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#E6B93E", backgroundColor: "#9e7b19ff" },
        { label: "Broadcast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#67AB9F", backgroundColor: "#3a6961ff"},
      ],
    },
    options: {
      animation: false,
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, suggestedMax: 100, title: { display: true, text: "Mbps" } },
      },
      plugins: { legend: { labels: { color: "#e8ecff" } } },
    },
  });

  const latencyChart = new Chart($("latencyChart"), {
    type: "line",
    data: {
      labels,
      datasets: [
        { label: "Unicast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#E6B93E", backgroundColor: "#9e7b19ff" },
        { label: "Broadcast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#67AB9F", backgroundColor: "#3a6961ff"},
      ],
    },
    options: {
      animation: false,
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, suggestedMax: 40, title: { display: true, text: "ms" } },
      },
      plugins: { legend: { labels: { color: "#e8ecff" } } },
    },
  });

    const fpsChart = new Chart($("fpsChart"), {
    type: "line",
    data: {
      labels,
      datasets: [
        { label: "Unicast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#E6B93E", backgroundColor: "#9e7b19ff" },
        { label: "Broadcast", data: Array(windowSize).fill(null), fill: false, tension: 0.25, borderColor: "#67AB9F", backgroundColor: "#3a6961ff"},
      ],
    },
    options: {
      animation: false,
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, suggestedMax: 30, position: "left", grid: { drawOnChartArea: false }, title: { display: true, text: "fps" } },
      },
      plugins: { legend: { labels: { color: "#e8ecff" } } },
    },
  });

  /*
  const sysChart = new Chart($("sysChart"), {
    type: "line",
    data: {
      labels,
      datasets: [
        { label: "CPU", data: Array(windowSize).fill(null), tension: 0.2, fill: false, borderColor: "#e06534ff", backgroundColor: "#944323ff" },
        { label: "Memory", data: Array(windowSize).fill(null), tension: 0.2, fill: false, yAxisID: "y1", borderColor: "#6487c9ff", backgroundColor: "#3d5786ff" },
      ],
    },
    options: {
      animation: false,
      responsive: true,
      maintainAspectRatio: false,
      scales: {
        y: { beginAtZero: true, suggestedMax: 100, title: { display: true, text: "%" } },
        y1: { beginAtZero: true, suggestedMax: 32, position: "right", grid: { drawOnChartArea: false }, title: { display: true, text: "GB" } },
      },
      plugins: { legend: { labels: { color: "#e8ecff" } } },
    },
  });
  */

  // ---- Data fetch helpers ----
  async function fetchLatest(instance, metric, windowMs = WINDOW_MS) {
    const url = `/get_latest_metrics?instance=${encodeURIComponent(instance)}&metric=${encodeURIComponent(metric)}&window_ms=${windowMs}`;
    const res = await fetch(url, { cache: "no-store" });
    if (!res.ok) throw new Error(`GET ${url} → ${res.status}`);
    const json = await res.json();
    const pairs = (json.values || []).map((pair) => {
      if (Array.isArray(pair)) return { t: pair[0], v: pair[1] };
      return { t: pair.t, v: pair.v };
    });
    return pairs.sort((left, right) => (left.t ?? 0) - (right.t ?? 0));
  }

  function maxTimestampOrNow(seriesList) {
    let maxTimestamp = 0;
    for (const series of seriesList) {
      const last = series.at(-1);
      if ((last?.t ?? 0) > maxTimestamp) {
        maxTimestamp = last.t;
      }
    }
    return maxTimestamp || Date.now();
  }

  function alignWindowEndMs(timestampMs) {
    return Math.floor(timestampMs / BUCKET_MS) * BUCKET_MS;
  }

  function buildWindowSeries(samples, windowEndMs, reducer) {
    const bucketStartMs = windowEndMs - (windowSize - 1) * BUCKET_MS;
    const buckets = Array.from({ length: windowSize }, () => []);

    for (const sample of samples) {
      const timestampMs = sample?.t ?? null;
      if (timestampMs == null || timestampMs < bucketStartMs || timestampMs > windowEndMs) {
        continue;
      }

      const index = Math.min(
        windowSize - 1,
        Math.floor((timestampMs - bucketStartMs) / BUCKET_MS)
      );
      if (index >= 0) {
        buckets[index].push(sample.v ?? null);
      }
    }

    return buckets.map((bucket) => reducer(bucket));
  }

  function lastBucketValue(values) {
    for (let i = values.length - 1; i >= 0; i--) {
      if (values[i] != null) {
        return values[i];
      }
    }
    return null;
  }

  function averageBucketValue(values) {
    const filtered = values.filter((value) => value != null && Number.isFinite(value));
    if (!filtered.length) {
      return null;
    }
    return filtered.reduce((sum, value) => sum + value, 0) / filtered.length;
  }

  function fillForwardSeries(values) {
    const out = [...values];
    let lastKnown = null;
    for (let i = 0; i < out.length; i++) {
      const value = out[i];
      if (value != null && Number.isFinite(value)) {
        lastKnown = value;
        continue;
      }
      if (lastKnown != null) {
        out[i] = lastKnown;
      }
    }
    return out;
  }

  function ratesFromCounterPairs(pairs) {
    const out = [];
    for (let i = 1; i < pairs.length; i++) {
      const dt = (pairs[i].t - pairs[i - 1].t) / 1000.0; // ms → s
      const dv = (pairs[i].v ?? 0) - (pairs[i - 1].v ?? 0);
      const rate = dt > 0 && dv >= 0 ? dv / dt : null;
      out.push({ t: pairs[i].t, v: rate });
    }
    return out;
  }

  function bytesCounterToMbitPerSecSeries(pairs, windowEndMs) {
    const rates = ratesFromCounterPairs(pairs).map((pair) => ({
      t: pair.t,
      v: pair.v == null ? null : (pair.v * 8) / 1e6,
    }));
    return buildWindowSeries(rates, windowEndMs, averageBucketValue);
  }

  function framesCounterToFpsSeries(pairs, windowEndMs) {
    const rates = ratesFromCounterPairs(pairs).map((pair) => ({
      t: pair.t,
      v: pair.v == null ? null : clamp(pair.v, 0, 120),
    }));
    return buildWindowSeries(rates, windowEndMs, averageBucketValue);
  }

  function clamp(v, lo, hi) {
    if (v == null) return v;
    return Math.max(lo, Math.min(hi, v));
  }

  function lastOrNull(arr) {
    return arr.length ? arr[arr.length - 1] : null;
  }

  // ---- Periodic update ----
  async function updateFromBackend() {
    try {
      const targets = await resolveMetricTargets();

      // Fire all requests concurrently
      const [
        serverBroadcast, // bytes counter
        serverUnicast,   // bytes counter
        clientFramesA,   // counter
        clientLatencyA,  // µs gauge
        clientFramesB,   // counter
        clientLatencyB,  // µs gauge
        clientPoints,    // points gauge
        //clientCPU,       // % gauge
        //clientMEM,       // bytes gauge
      ] = await Promise.all([
        fetchLatest(targets.server, M_BROADCAST),
        fetchLatest(targets.server, M_UNICAST),
        // Per-stream metrics (note the per-stream instance keys)
        fetchLatest(targets.streamA, M_FRAMES_PER_STREAM),
        fetchLatest(targets.streamA, M_LATENCY_PER_STREAM),
        fetchLatest(targets.streamB, M_FRAMES_PER_STREAM),
        fetchLatest(targets.streamB, M_LATENCY_PER_STREAM),
        fetchLatest(targets.client, M_POINTS),
        //fetchLatest(targets.client, M_CPU),
        //fetchLatest(targets.client, M_MEM),
      ]);

      const windowEndMs = alignWindowEndMs(
        maxTimestampOrNow([
          serverBroadcast,
          serverUnicast,
          clientFramesA,
          clientLatencyA,
          clientFramesB,
          clientLatencyB,
          clientPoints,
        ])
      );

      // Bandwidth (Mbit/s)
      const bcastMbit = fillForwardSeries(
        bytesCounterToMbitPerSecSeries(serverBroadcast, windowEndMs)
      );
      const unicastMbit = fillForwardSeries(
        bytesCounterToMbitPerSecSeries(serverUnicast, windowEndMs)
      );

      // FPS from counter (per stream)
      const fpsRatesA = framesCounterToFpsSeries(clientFramesA, windowEndMs);
      const fpsRatesB = framesCounterToFpsSeries(clientFramesB, windowEndMs);

      // Latency (µs → ms) per stream
      const latencyMsA = buildWindowSeries(
        clientLatencyA.map((sample) => ({ t: sample.t, v: sample.v == null ? null : sample.v / 1000.0 })),
        windowEndMs,
        lastBucketValue
      );
      const latencyMsB = buildWindowSeries(
        clientLatencyB.map((sample) => ({ t: sample.t, v: sample.v == null ? null : sample.v / 1000.0 })),
        windowEndMs,
        lastBucketValue
      );

      const pointsSeries = buildWindowSeries(clientPoints, windowEndMs, lastBucketValue);
      const bwBcastSeries = bcastMbit;
      const bwUniSeries = unicastMbit;
      const fpsSeriesA = fpsRatesA;
      const fpsSeriesB = fpsRatesB;
      const latSeriesA = latencyMsA;
      const latSeriesB = latencyMsB;
      const objASeries = pointsSeries.map((value) => {
        if (value == null) return null;
        return Math.max(0, Math.min(value, OBJECT_ONE_MAX_POINTS)) / 1000.0;
      });
      const objBSeries = pointsSeries.map((value) => {
        if (value == null) return null;
        return Math.max(0, value - OBJECT_ONE_MAX_POINTS) / 1000.0;
      });

      //const cpuSeries = padTo(clientCPU.map((p) => (p.v == null ? null : clamp(p.v, 0, 100))));
      //const memSeries = padTo(clientMEM.map((p) => (p.v == null ? null : p.v / (1024 * 1024 * 1024)))); // GB

      // Update charts
      // Latency
      latencyChart.data.datasets[0].data = latSeriesA;
      latencyChart.data.datasets[1].data = latSeriesB;

      // fps
      fpsChart.data.datasets[0].data = fpsSeriesA;
      fpsChart.data.datasets[1].data = fpsSeriesB;

      // Bandwidth
      bwChart.data.datasets[0].data = bwUniSeries;
      bwChart.data.datasets[1].data = bwBcastSeries;

      // Points (k points)
      pointsChart.data.datasets[0].data = objASeries;
      pointsChart.data.datasets[1].data = objBSeries;

      // System
      //sysChart.data.datasets[0].data = cpuSeries;
      //sysChart.data.datasets[1].data = memSeries;

      latencyChart.update();
      fpsChart.update();
      bwChart.update();
      pointsChart.update();
      //sysChart.update();

      //setStatus("Metrics updated.", "ok");
    } catch (e) {
      metricTargets = null;
      setStatus(`Metrics pending: ${e.message || e}`, "warn");
      console.error(e);
    }
  }



  // one scheduler that adapts its cadence depending on status
  async function tick() {
    const previousStatus = isRunning;
    isRunning = await fetchRunning();

    if (isRunning) {
      if (!previousStatus) {
        metricTargets = null;
        try {
          await syncTrafficControlTarget();
        } catch (error) {
          console.debug("initial traffic control target sync failed:", error);
        }
      }
      setStatus("Network running", "ok");
      // Only pull metrics while running
      await updateFromBackend().catch((e) => console.debug(e));
      // fast cadence while running
      setTimeout(tick, 1000);
    } else {
      metricTargets = null;
      setStatus("Network stopped", "warn");
      // slow updates when stopped/unreachable
      setTimeout(tick, 3000);
    }
  }

  // ---- Kickoff ----
  updateLabels();
  setStatus("Ready.");
  tick();
})();
