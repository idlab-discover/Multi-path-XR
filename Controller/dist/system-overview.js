/* system-overview.js — fixed-position graph + animated beams
   - Nodes use percent-based positioning (resize-safe)
   - Edges have per-link style: color, width, dashed, curvature
   - Beams (moving “border-beam” style) have per-beam gradient + id/tag
   - Per-node policy controls how arriving beams propagate:
       'drop' | 'bounce' | 'forwardAll' | arrayOfEdgeIds | custom function
*/

(function () {
  // ---------- tiny helpers ----------
  const $ = (sel) => document.querySelector(sel);
  const clamp = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

  const MAX_BEAMS = 40; // max simultaneous beams

  // Compute a quadratic curve path between two points with optional curvature
  // curvature in [-1..+1], positive bends “up” (perpendicular to the straight line)
  function qPath(x1, y1, x2, y2, curvature = 0) {
    if (!curvature) return `M ${x1},${y1} L ${x2},${y2}`;
    const mx = (x1 + x2) / 2;
    const my = (y1 + y2) / 2;
    // perpendicular unit normal
    const dx = x2 - x1, dy = y2 - y1;
    const len = Math.hypot(dx, dy) || 1;
    const nx = -dy / len, ny = dx / len;
    const amp = curvature * len * 0.25; // 1.0 → quarter-length bow
    const cx = mx + nx * amp, cy = my + ny * amp;
    return `M ${x1},${y1} Q ${cx},${cy} ${x2},${y2}`;
  }

  // ---------- NetViz core ----------
  class NetViz {
    constructor(cfg) {
      this.cfg = cfg;
      this.mount = typeof cfg.mount === 'string' ? $(cfg.mount) : cfg.mount;

      console.log('NetViz', this.mount, cfg);

      // containers
      this.svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      this.svg.id = 'netLines';
      this.svg.setAttribute('width', '100%');
      this.svg.setAttribute('height', '100%');
      this.svg.style.position = 'absolute';
      this.svg.style.inset = '0';

      this.nodesLayer = document.createElement('div');
      this.nodesLayer.id = 'netNodes';
      this.nodesLayer.style.position = 'absolute';
      this.nodesLayer.style.inset = '0';

      // defs (glow + per-beam gradients)
      const defs = document.createElementNS('http://www.w3.org/2000/svg', 'defs');
      defs.innerHTML = `
        <filter id="glow" x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="2.2" result="b"/>
          <feMerge>
            <feMergeNode in="b"/><feMergeNode in="SourceGraphic"/>
          </feMerge>
        </filter>
      `;
      this.svg.appendChild(defs);
      this.defs = defs;

      // stage scaffolding
      this.mount.innerHTML = '';
      this.mount.appendChild(this.svg);
      this.mount.appendChild(this.nodesLayer);

      // internal maps
      this.nodes = new Map();    // id -> {id, xPct, yPct, el, box, icon}
      this.edges = new Map();    // id -> {id, from, to, el, style, curvature, length}
      this.neighborEdges = new Map(); // nodeId -> [edgeId,...]        // NEW
      this.neighborNodes = new Map(); // nodeId -> [neighborNodeId,...]// NEW
      this.beams = new Map();    // id -> Beam

      this.animRunning = false;
      this.maxFps = clamp(cfg.maxFps ?? 15, 1, 60);
      this._frameInterval = 1000 / this.maxFps;

      // install & draw initial
      this.install();
      this.layout();
      window.addEventListener('resize', () => this.layout());

      // optional hook
      if (typeof cfg.onReady === 'function') cfg.onReady(this);
    }

    // ---------- public API ----------
    static mount(target, cfg) {
      return new NetViz({ mount: target, ...cfg });
    }

    setMaxFps(fps) {
    this.maxFps = clamp(fps, 1, 60);
    this._frameInterval = 1000 / this.maxFps;
  }

    addBeam(opts) {
      // Validate
      if (!opts || !opts.edge) return null;

      // opts: { id, edge, dir(+1|-1), speed(px/s)=240, length(px)=100, gradient:[from,to], tag?, hops? }
      const e = this.edges.get(opts.edge);
      if (!e) return null;

      // We cap the maximum amount of beams to 40 to avoid overloading the browser
      if (this.beams.size >= MAX_BEAMS) {
        console.warn('Max beams reached, dropping new beam', opts);
        return null;
      }

      const id = opts.id || `bm_${Math.random().toString(36).slice(2, 8)}`;
      const dir = opts.dir == null ? +1 : Math.sign(opts.dir) || +1;
      const speed = opts.speed ?? 240;
      const segLen = clamp(opts.length ?? 110, 10, 600);
      const tag = opts.tag ?? '';
      const hops = clamp(opts.hops ?? 32, 0, 128);
      const widthFactor = opts.widthFactor ?? 1.5;

      // overlay path (copy geometry)
      const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
      p.setAttribute('class', 'beam');
      p.setAttribute('d', e.el.getAttribute('d'));
      p.setAttribute('fill', 'none');
      p.setAttribute('stroke-width', String((e.style?.width ?? 3) + widthFactor));
      p.setAttribute('stroke-linecap', 'round');
      p.setAttribute('filter', 'url(#glow)');

      // gradient
      const gradId = `grad_${id}`;
      const grad = document.createElementNS('http://www.w3.org/2000/svg', 'linearGradient');
      grad.setAttribute('id', gradId);
      grad.setAttribute('gradientUnits', 'userSpaceOnUse');

      // approximate gradient along straight line between node centers
      const a = this.nodeCenter(e.from), b = this.nodeCenter(e.to);
      grad.setAttribute('x1', a.x); grad.setAttribute('y1', a.y);
      grad.setAttribute('x2', b.x); grad.setAttribute('y2', b.y);

      const [cFrom, cTo] = (opts.gradient && opts.gradient.length === 2)
        ? opts.gradient
        : ['#FFC94B', '#FFF2B0'];

      //console.log('beam gradient', { cFrom, cTo, tag: opts.tag, edge: e.id });

      grad.innerHTML = `
        <stop offset="0%" stop-color="${cFrom}" stop-opacity="0"/>
        <stop offset="25%" stop-color="${cFrom}" />
        <stop offset="75%" stop-color="${cTo}" />
        <stop offset="100%" stop-color="${cTo}" stop-opacity="0"/>
      `;
      this.defs.appendChild(grad);
      p.setAttribute('stroke', `url(#${gradId})`);

      // dash animation set-up
      const total = e.el.getTotalLength();
      const patternLength = segLen + total;
      // pattern length = segLen + total (gap).
      p.style.strokeDasharray = `${segLen}, ${Math.max(1, total)}`;
      // dir > 0 starts at 0 (left), dir < 0 starts at total (right).
      const startOffsetDefault = dir > 0 ? -segLen : total;
      const startOffset = (opts.startOffset != null) ? opts.startOffset : startOffsetDefault;
      const clampedStart = clamp(startOffset, -segLen, patternLength);
      // The rendered offset actually works in the opposite direction
      p.style.strokeDashoffset = String(-startOffset);

      this.svg.appendChild(p);

      const beam = {
        id, edge: e.id, dir, speed, segLen, total, path: p,
        grad, gradId, tag, hopsLeft: hops,
        gradientColors: [cFrom, cTo],
        offset: clampedStart,
        state: 'active',             // 'active' | 'finishing'
        spawnedNext: false,          // guard: did we already fan-out?
        widthFactor: widthFactor
      };

      // This allows our beam length to be updated
      Object.defineProperty(beam, 'length', {
        get() { return this.segLen; },
        set(v) {
          this.segLen = clamp(v, 10, 600);
          this.path.style.strokeDasharray = `${this.segLen}, ${Math.max(1, this.total)}`;
        }
      });

      this.beams.set(id, beam);
      //console.log('added beam', beam);
      this.ensureAnim();
      return id;
    }

    // Replace / extend policy for a node
    setNodePolicy(nodeId, fnOrKeyword) {
      this.policies[nodeId] = fnOrKeyword;
    }

    // ---------- install drawing ----------
    install() {
      const { nodes, edges, policies } = this.cfg;

      // build nodes
      Object.entries(nodes).forEach(([id, n]) => {
        const el = document.createElement('div');
        el.className = `node ${n.type || ''}`;
        // icon element
        const iconEl = document.createElement('div');
        const type = n.type || '';                 // keep explicit type
        const iconName = n.icon || type;           // icon may differ from type
        iconEl.className = `icon ${iconClass(iconName)}`;
        el.appendChild(iconEl);
        this.nodesLayer.appendChild(el);

        // persist full node info (type + icon + original cfg for convenience)
        this.nodes.set(id, {
          id,
          xPct: n.x,
          yPct: n.y,
          el,
          iconEl,          // Keep the actual icon element
          type,            // <— now available in policies as node.type
          icon: iconName,  // e.g., 'antenna' | 'server' | 'vr' | 'switch' | 'invisible'
          data: n          // optional: original user-supplied node config
        });
      });

      // build edges
      edges.forEach((e) => {
        const path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
        path.setAttribute('class', `link ${e.kind || ''}`);
        const style = e.style || {};
        path.setAttribute('stroke', style.color || (e.kind === 'broadcast' ? '#67AB9F44' : '#E6B93E44'));
        path.setAttribute('stroke-width', String(style.width ?? 3));
        if (style.dashed || e.dashed) {
          path.setAttribute('stroke-dasharray', style.dasharray || '6 6');
        }
        path.setAttribute('fill', 'none');
        this.svg.appendChild(path);

        const item = {
          id: e.id,
          from: e.from,
          to: e.to,
          curvature: e.curvature || 0,
          el: path,
          style,
        };
        this.edges.set(e.id, item);

        // neighbor maps
        this._linkNeighbors(e.from, e.to, e.id);
      });

      // policies
      this.policies = policies || {};
    }

    _linkNeighbors(a, b, edgeId) {
      // edges
      if (!this.neighborEdges.has(a)) this.neighborEdges.set(a, []);
      if (!this.neighborEdges.has(b)) this.neighborEdges.set(b, []);
      this.neighborEdges.get(a).push(edgeId);
      this.neighborEdges.get(b).push(edgeId);

      // nodes
      if (!this.neighborNodes.has(a)) this.neighborNodes.set(a, []);
      if (!this.neighborNodes.has(b)) this.neighborNodes.set(b, []);
      this.neighborNodes.get(a).push(b);
      this.neighborNodes.get(b).push(a);
    }

    _edgeIdBetween(a, b) {
      const eids = this.neighborEdges.get(a) || [];
      for (const eid of eids) {
        const e = this.edges.get(eid);
        if (!e) continue;
        if ((e.from === a && e.to === b) || (e.from === b && e.to === a)) return eid;
      }
      return null;
    }

    // ---------- geometry / layout ----------
    nodeCenter(id) {
      const n = this.nodes.get(id);
      const stageRect = this.mount.getBoundingClientRect();

      // Preferred: measure the actual icon box center in viewport coords,
      // then convert to stage-local coords by subtracting stage origin.
      if (n?.iconEl) {
        const r = n.iconEl.getBoundingClientRect();
        const cx = r.left + r.width / 2 - stageRect.left;
        const cy = r.top  + r.height / 2 - stageRect.top;
        return { x: cx, y: cy, w: r.width, h: r.height, stageW: stageRect.width, stageH: stageRect.height };
      }

      // Fallback: approximate using the node wrapper if iconEl is missing.
      const x = (n.xPct / 100) * stageRect.width;
      const y = (n.yPct / 100) * stageRect.height;
      const w = n.el.offsetWidth || 46, h = n.el.offsetHeight || 46;
      return { x: x + w / 2, y: y + h / 2, w, h, stageW: stageRect.width, stageH: stageRect.height };
    }

    layout() {
      // place nodes
      this.nodes.forEach((n) => {
        n.el.style.left = `calc(${n.xPct}% - ${n.el.offsetWidth / 2}px)`;
        n.el.style.top  = `calc(${n.yPct}% - ${n.el.offsetHeight / 2}px)`;
      });

      // draw edges
      this.edges.forEach((e) => {
        const a = this.nodeCenter(e.from), b = this.nodeCenter(e.to);
        const d = qPath(a.x, a.y, b.x, b.y, e.curvature);
        e.el.setAttribute('d', d);
      });

      // realign beam geometry to updated paths
      this.beams.forEach((bm) => {
        const edge = this.edges.get(bm.edge);
        if (!edge) return;
        bm.path.setAttribute('d', edge.el.getAttribute('d'));
        bm.total = edge.el.getTotalLength();
        bm.path.style.strokeDasharray = `${bm.segLen}, ${Math.max(1, bm.total)}`;
        //bm.path.style.strokeDashoffset = String(bm.total);

        // update gradient line
        const a = this.nodeCenter(edge.from), b = this.nodeCenter(edge.to);
        bm.grad.setAttribute('x1', a.x); bm.grad.setAttribute('y1', a.y);
        bm.grad.setAttribute('x2', b.x); bm.grad.setAttribute('y2', b.y);
      });
    }

    // ---------- animation ----------
    ensureAnim() {
      if (this.animRunning) return;
      this.animRunning = true;
      this._last = performance.now();
      const loop = (t) => {
        if (!this.animRunning) return;
        const elapsed = t - this._last; // ms since last step
        // Only step when we've reached the capped frame interval (<= 30 fps)
        if (elapsed >= this._frameInterval) {
          // dt in seconds; clamp for safety as before
          const dt = Math.min(0.05, elapsed / 1000);
          // Reduce drift if rAF cadence doesn't divide evenly into our cap
          this._last = t - (elapsed % this._frameInterval);
          this._step(dt);
        }
        if (this.animRunning) requestAnimationFrame(loop);
      };
      requestAnimationFrame(loop);
    }

    _step(dt) {
      if (this.beams.size === 0) {
        this.animRunning = false;
        return;
      }

      const toDelete = [];
      this.beams.forEach((bm) => {
        const edge = this.edges.get(bm.edge);
        if (!edge) { toDelete.push(bm.id); return; }

        // move dash
        const delta = bm.speed * dt;
        // Forward (dir > 0) moves right → offset increases
        bm.offset += (bm.dir > 0 ? +delta : -delta);
        // The rendered offset actually works in the opposite direction
        bm.path.style.strokeDashoffset = String(-bm.offset);

        // reached end?  (use head-based thresholds; no time-based linger)
        // forward: head reaches right when offset >= total - len
        // reverse: head reaches left  when offset <= len
        const reachedNode = bm.dir > 0
          ? (bm.offset >= (bm.total - bm.length))
          : (bm.offset <= 0);
        if (reachedNode) {
          // If we’re already finishing (lingering), just count down
          if (bm.state === 'finishing') {
            // Natural delete when the dash is fully off the path (no time-based linger):
            const fullyOff = bm.dir > 0 ? (bm.offset >= bm.total) : (bm.offset <= -bm.length);
            if (fullyOff) {
              toDelete.push(bm.id);
              return;
            }
          }

          // arriving node
          const nodeId = bm.dir > 0 ? edge.to : edge.from;
          const action = this._policy(nodeId, bm, edge.id);

          // Absorb/drop (end of life): still linger a hair so it visually "lands"
          if (action === 'drop' || action === 'absorb' || bm.hopsLeft <= 0) {
            bm.state = 'finishing';
            return;
          }

          // Bounce: spawn nothing, just reverse, and keep going (no gap)
          if (action === 'bounce') {
            // reverse and nudge just inside so we don't immediately retrigger
            bm.dir *= -1;
            const nudge = Math.max(1, Math.min(4, bm.segLen * 0.2)); // ~1–4 px
            bm.offset = bm.dir > 0 ? nudge : (bm.total - nudge);
            bm.hopsLeft--;
            return;
          }

          // Prevent multi-spawn loops if the same frame hits again
          if (!bm.spawnedNext) {
            bm.spawnedNext = true;

            // who did we come from (as node id)?
            const incomingNode =
              (edge.from === nodeId) ? edge.to :
              (edge.to   === nodeId) ? edge.from : null;

            // Resolve outgoing edges (edge ids)
            let outEdges = [];
            if (action === 'forwardAll') {
              const neighNodes = this.neighborNodes.get(nodeId) || [];
              const targets = neighNodes.filter(n => n !== incomingNode);
              outEdges = targets
                .map(n => this._edgeIdBetween(nodeId, n))
                .filter(Boolean);
            } else if (Array.isArray(action)) {
              outEdges = action.map(str => {
                if (this.edges.has(str)) return str; // edge id
                return this._edgeIdBetween(nodeId, str); // node id → edge id
              }).filter(Boolean);
            } else if (typeof action === 'object' && action && action.forward) {
              outEdges = (action.forward || [])
                .map(n => this._edgeIdBetween(nodeId, n)).filter(Boolean);
            }

            // Spawn children with visual continuity.
            outEdges.forEach((eid, idx) => {
              const e2 = this.edges.get(eid);
              if (!e2) return;
              const total2 = e2.el.getTotalLength();
              const dir2 = (e2.from === nodeId) ? +1 : -1;

              this.addBeam({
                id: `${bm.id}_${idx}_${Date.now().toString(36).slice(5)}`,
                edge: eid,
                dir: dir2,
                speed: bm.speed,
                length: bm.segLen,
                gradient: this._gradientFromBeam(bm),
                tag: bm.tag,
                hops: bm.hopsLeft - 1,
                widthFactor: bm.widthFactor,
              });
            });
          }
          // Now linger the parent so it visually "finishes into" the node
          bm.state = 'finishing';
          return;
        }
      });

      toDelete.forEach((id) => this._removeBeam(id));
    }

    _removeBeam(id) {
      const bm = this.beams.get(id);
      if (!bm) return;
      bm.path.remove();
      bm.grad.remove();
      this.beams.delete(id);
    }

    _policy(nodeId, beam, incomingEdgeId) {
      const p = this.policies[nodeId] ?? this.policies['*'];

      if (!p) return 'forwardAll';
      if (typeof p === 'string') return p;
      if (Array.isArray(p)) return p; // kept for backward-compat (edge ids or node ids)

      if (typeof p === 'function') {

        // compute incoming nodeId from the edge
        let incomingNode = null;
        if (incomingEdgeId && this.edges.has(incomingEdgeId)) {
          const e = this.edges.get(incomingEdgeId);
          incomingNode = (e.from === nodeId) ? e.to : e.from;
        }

        // neighbor node ids (not edge ids)
        const neighNodes = this.neighborNodes.get(nodeId) || [];
        const nodeObj = this.nodes.get(nodeId);

        return p({
          node: nodeObj,        // <— NEW: full node object
          nodeId,               // still provided for convenience
          incoming: incomingNode, // <— NEW: incoming as neighbor node id (or null)
          neighbors: neighNodes,  // <— NEW: array of neighbor node ids
          beam
        });
      }
      return 'forwardAll';
    }

    _gradientFromBeam(bm) {
      // Reuse the original beam’s gradient for all clones/hops
      if (bm && bm.gradientColors && bm.gradientColors.length === 2) {
        return bm.gradientColors;
      }
      // Fallback by tag (should rarely be used if you pass a gradient on spawn)
      return bm?.tag === 'broadcast'
        ? ['#6DFFCE', '#44C2A9']
        : ['#FFC94B', '#FFF2B0'];
    }
  }

  function iconClass(kind) {
    switch ((kind || '').toLowerCase()) {
      case 'server': return 'icon-server';
      case 'antenna': return 'icon-antenna';
      case 'vr':
      case 'client': return 'icon-vr';
      case 'switch': return 'icon-switch';
      case 'invisible': return 'icon-invisible';
      default: return 'icon-invisible';
    }
  }

  // Boot
  window.NetViz = NetViz; // (optional) expose for console tinkering
})();
