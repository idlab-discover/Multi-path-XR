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
  const SERVER_INSTANCE_RAW = "11.0.1.2_3001_server";
  const CLIENT_INSTANCE_RAW = "11.0.2.2_3380_client";

  // Sanitize like the logger: non-alnum → underscore
  function sanitizeLabelValue(s) {
    return (s || "").replace(/[^0-9A-Za-z]/g, "_");
  }

  // Normalization: "11.0.1.2_3001_server" -> "11.0.1.2:3001_server"
  //                "metrics_11.0.2.2_3380_client" -> "11.0.2.2:3380_client"
  function normalizeInstance(raw) {
    let s = raw;
    if (s.startsWith("metrics_")) s = s.slice("metrics_".length);
    // replace only the first "_" after the IPv4 (turn IP_port_mode into IP:port_mode)
    const parts = s.split("_");
    if (parts.length >= 3) {
      const [ip, port, ...rest] = parts;
      return `${ip}:${port}_${rest.join("_")}`;
    }
    return s;
  }

  const SERVER_INSTANCE = normalizeInstance(SERVER_INSTANCE_RAW);
  const CLIENT_INSTANCE = normalizeInstance(CLIENT_INSTANCE_RAW);

  // --- Per-stream instances (match the logger’s instance key) ---
  // From your Prometheus examples:
  //   stream_id="client_1_"  → instance "11.0.2.2:3380_client_sid_client_1_"
  //   stream_id="flute_239.0.2.1:40085" → sanitized "flute_239_0_2_1_40085"
  //   → instance "11.0.2.2:3380_client_sid_flute_239_0_2_1_40085"
  const STREAM_A_ID = "client_1_";
  const STREAM_B_ID = "flute_239.0.2.1:40085";
  const CLIENT_INSTANCE_STREAM_A = normalizeInstance(
    `${CLIENT_INSTANCE_RAW}_sid_${sanitizeLabelValue(STREAM_A_ID)}`
  );
  const CLIENT_INSTANCE_STREAM_B = normalizeInstance(
    `${CLIENT_INSTANCE_RAW}_sid_${sanitizeLabelValue(STREAM_B_ID)}`
  );

  // Metric names
  const M_BROADCAST = "n1_eth1_tx_bytes"; // server, counter (bytes)
  const M_UNICAST = "n1_eth2_tx_bytes";   // server, counter (bytes)
  const M_POINTS = "total_point_count";   // client, gauge (points)
  const M_CPU = "cpu_usage";              // client, gauge (%)
  const M_MEM = "memory_usage";           // client, gauge (bytes)
  const M_FRAMES_PER_STREAM = "frames_received_total_per_stream"; // Client per stream, counter (frames)
  const M_LATENCY_PER_STREAM = "send_to_receive_time_diff_per_stream"; // client per stream, gauge (microseconds)

  let isRunning = false;

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
  const windowSize = 60;
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
  async function fetchLatest(instance, metric, n = windowSize) {
    const url = `/get_latest_metrics?instance=${encodeURIComponent(instance)}&metric=${encodeURIComponent(metric)}&n=${n}`;
    const res = await fetch(url);
    if (!res.ok) throw new Error(`GET ${url} → ${res.status}`);
    const json = await res.json();
    // json.values: [[t, v], ...] or [{t,v}]? Our server sends Vec<(i64,f64)>, serialized as [t,v]
    // But we built a struct that serializes as {instance, metric, count, values:[ [t,v], ... ]}
    const pairs = (json.values || []).map((pair) => {
      // tolerate either form
      if (Array.isArray(pair)) return { t: pair[0], v: pair[1] };
      return { t: pair.t, v: pair.v };
    });
    return pairs;
  }

  function toWindow(series, targetLen = windowSize) {
    // returns exactly targetLen numbers (pad front with nulls if fewer)
    const values = series.map((p) => (typeof p === "number" ? p : p.v ?? null));
    if (values.length >= targetLen) return values.slice(values.length - targetLen);
    const pad = Array(targetLen - values.length).fill(null);
    return pad.concat(values);
  }

  function ratesFromCounter(pairs) {
    // pairs sorted oldest→newest, compute per-second rate
    const out = [];
    for (let i = 1; i < pairs.length; i++) {
      const dt = (pairs[i].t - pairs[i - 1].t) / 1000.0; // ms → s
      const dv = (pairs[i].v ?? 0) - (pairs[i - 1].v ?? 0);
      const rate = dt > 0 ? dv / dt : null;
      out.push(rate);
    }
    return out;
  }

  function bytesCounterToMbitPerSec(pairs) {
    const rates = ratesFromCounter(pairs); // bytes/s
    return rates.map((r) => (r == null ? null : (r * 8) / 1e6)); // → Mbit/s
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
        fetchLatest(SERVER_INSTANCE, M_BROADCAST),
        fetchLatest(SERVER_INSTANCE, M_UNICAST),
        // Per-stream metrics (note the per-stream instance keys)
        fetchLatest(CLIENT_INSTANCE_STREAM_A, M_FRAMES_PER_STREAM),
        fetchLatest(CLIENT_INSTANCE_STREAM_A, M_LATENCY_PER_STREAM),
        fetchLatest(CLIENT_INSTANCE_STREAM_B, M_FRAMES_PER_STREAM),
        fetchLatest(CLIENT_INSTANCE_STREAM_B, M_LATENCY_PER_STREAM),
        fetchLatest(CLIENT_INSTANCE, M_POINTS),
        //fetchLatest(CLIENT_INSTANCE, M_CPU),
        //fetchLatest(CLIENT_INSTANCE, M_MEM),
      ]);

      // Bandwidth (Mbit/s)
      const bcastMbit = bytesCounterToMbitPerSec(serverBroadcast);
      const unicastMbit = bytesCounterToMbitPerSec(serverUnicast);

      // FPS from counter (per stream)
      const fpsRatesA = ratesFromCounter(clientFramesA).map((r) => (r == null ? null : clamp(r, 0, 120)));
      const fpsRatesB = ratesFromCounter(clientFramesB).map((r) => (r == null ? null : clamp(r, 0, 120)));

      // Latency (µs → ms) per stream
      const latencyMsA = clientLatencyA.map((p) => (p.v == null ? null : p.v / 1000.0));
      const latencyMsB = clientLatencyB.map((p) => (p.v == null ? null : p.v / 1000.0));

      // Build 60-second series for points:
      // Use last known pps for the latest slot; earlier values are derived by pairing each historic point with its historic fps.
      const pointsSeries = [];
      const pointsSeriesA = [];
      const pointsSeriesB = [];
      for (let i = 0; i < clientPoints.length; i++) {
        const frameIdx = i;
        const pts = clientPoints[frameIdx]?.v ?? 0;
        const v = pts;// * fpsVal;
        const a = Math.max(0, Math.min(v, OBJECT_ONE_MAX_POINTS));
        const b = Math.max(0, v - OBJECT_ONE_MAX_POINTS);
        pointsSeries.push(v / 1000.0); // in k points
        pointsSeriesA.push(a / 1000.0);
        pointsSeriesB.push(b / 1000.0);
      }
      // Ensure length = windowSize by padding
      const padTo = (arr) => toWindow(arr, windowSize);
      const bwBcastSeries = padTo(bcastMbit);
      const bwUniSeries = padTo(unicastMbit);
      const fpsSeriesA = padTo(fpsRatesA);
      const fpsSeriesB = padTo(fpsRatesB);
      const latSeriesA = padTo(latencyMsA);
      const latSeriesB = padTo(latencyMsB);
      const objASeries = padTo(pointsSeriesA);
      const objBSeries = padTo(pointsSeriesB);

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
      //setStatus(`Metrics fetch error: ${e.message || e}`, "err");
      console.error(e);
      isRunning = false; // treat as stopped
    }
  }



  // one scheduler that adapts its cadence depending on status
  async function tick() {

    if (isRunning) {
      // Only pull metrics while running
      await updateFromBackend().catch((e) => console.debug(e));
      // fast cadence while running
      setTimeout(tick, 1000);
    } else {
      const previousStatus = isRunning;
      isRunning = await fetchRunning();


      if (isRunning) {
        if (previousStatus !== isRunning) {
          setStatus("Network running", "ok");
        }
        setTimeout(tick, 1000);
      } else {
        setStatus("Network stopped", "warn");
        // slow updates when stopped/unreachable
        setTimeout(tick, 3000);
      }
    }
  }

  // ---- Kickoff ----
  updateLabels();
  setStatus("Ready.");
  tick();
})();
