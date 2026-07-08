  // ------------------------------------------------------------------
  //  API helpers
  // ------------------------------------------------------------------
  // Overview semantics: hero shows lifetime cumulative — no time filter
  // at all. Numbers only grow as new events come in. Activity tab keeps
  // its own rolling windows via the dropdown for narrower views.
  function buildEventsQs() {
    const p = new URLSearchParams();
    p.set('limit', state.limit);
    p.set('offset', state.offset);
    return p.toString();
  }
  function summaryQs() {
    return '';
  }
  function timelineQs() {
    // Chart needs a window for bucketing — default to last 30 days so the
    // chart isn't dominated by a single ancient bar but stays readable.
    // Lifetime totals come from /api/summary; chart is just the trend.
    const p = new URLSearchParams();
    p.set('hours', '720');
    return p.toString();
  }

  // ------------------------------------------------------------------
  //  Overview: hero numbers + side strip
  // ------------------------------------------------------------------
  let lastSummary = null;
  async function loadSummary() {
    const res = await fetch(`/api/summary?${summaryQs()}`);
    const data = await res.json();
    lastSummary = data;

    $('#hero-eyebrow').textContent = todayLabel();
    $('#hero-total').textContent = fmtInt(data.total);
    const hitPct = fmtPctRaw(data.hit_rate);

    $('#hero-strip-hit').innerHTML = `${hitPct ?? '—'}<em>%</em>`;
    $('#hero-strip-hit-delta').textContent = `${fmtInt(data.hits)} / ${fmtInt(data.total)} 命中`;

    // 平均延迟
    const latNum = data.avg_latency_ms;
    $('#hero-strip-lat').innerHTML = latNum == null
      ? '—'
      : `${fmtMs(latNum)}<em class="ms-tail">ms</em>`;
    $('#hero-strip-lat-delta').textContent = latNum == null ? '' : 'router rtt';

    // 累计 token
    $('#hero-strip-tok').textContent = fmtTok(data.total_tokens);
    $('#hero-strip-tok-delta').textContent = data.total_tokens ? 'in + out' : '';

    // 错误数
    const errs = data.errors ?? 0;
    $('#hero-strip-err').textContent = fmtInt(errs);
    const errEl = $('#hero-strip-err-delta');
    if (errs > 0) {
      errEl.textContent = `${((errs / Math.max(1, data.total)) * 100).toFixed(1)}% rate`;
      errEl.className = 'delta err';
    } else {
      errEl.textContent = 'no failures';
      errEl.className = 'delta';
    }

    // sidebar live strip
    $('#side-total').textContent = fmtInt(data.total);
    $('#side-hit').textContent = hitPct == null ? '—' : `${hitPct}%`;
    $('#side-lat').textContent = data.avg_latency_ms == null ? '—' : `${fmtMs(data.avg_latency_ms)}ms`;
    $('#side-err').textContent = fmtInt(data.errors);

    // Model-usage panel rides the SAME /api/summary payload — per_model now
    // carries calls/total_tokens/avg_latency_ms/hits, so there is no separate
    // /api/events paging (loadModelUsage lives in 07-polling-models.js).
    if (typeof loadModelUsage === 'function') loadModelUsage(data.per_model);
  }

  // ------------------------------------------------------------------
  //  Overview: 24h chart (single area path, editorial style)
  // ------------------------------------------------------------------
  async function loadTimeline() {
    const res = await fetch(`/api/timeline?${timelineQs()}`);
    const data = await res.json();
    const pts = data.points || [];
    const svg = $('#trend-chart');
    if (!svg) return;
    const W = 800, H = 240;
    const TOP = 8, BOT = 32;
    const innerH = H - TOP - BOT;

    const maxTotal = Math.max(1, ...pts.map((p) => p.total));
    let strokeD = '';
    let fillD = '';
    if (pts.length >= 2) {
      const xStep = W / (pts.length - 1);
      const segs = pts.map((p, i) => {
        const x = i * xStep;
        const y = TOP + innerH - (p.total / maxTotal) * innerH;
        return [x, y];
      });
      strokeD = segs.map(([x, y], i) => `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)},${y.toFixed(1)}`).join(' ');
      fillD = strokeD +
        ` L ${segs[segs.length - 1][0].toFixed(1)},${(H - BOT).toFixed(1)}` +
        ` L 0,${(H - BOT).toFixed(1)} Z`;
    } else if (pts.length === 1) {
      const y = TOP + innerH - (pts[0].total / maxTotal) * innerH;
      strokeD = `M 0,${y.toFixed(1)} L ${W},${y.toFixed(1)}`;
      fillD = `${strokeD} L ${W},${(H - BOT).toFixed(1)} L 0,${(H - BOT).toFixed(1)} Z`;
    }

    svg.querySelector('.area-stroke').setAttribute('d', strokeD);
    svg.querySelector('.area-fill').setAttribute('d', fillD);

    const axis = svg.querySelector('.axis');
    axis.innerHTML = '';
    if (pts.length >= 2) {
      const first = pts[0].ts_start;
      const last  = pts[pts.length - 1].ts_start;
      axis.insertAdjacentHTML('beforeend', `<text x="2"   y="${H - 6}">${fmtTs(first)}</text>`);
      const mid = pts[Math.floor(pts.length / 2)].ts_start;
      axis.insertAdjacentHTML('beforeend', `<text x="${W / 2}" y="${H - 6}" text-anchor="middle">${fmtTs(mid)}</text>`);
      axis.insertAdjacentHTML('beforeend', `<text x="${W - 2}" y="${H - 6}" text-anchor="end">${fmtTs(last)}</text>`);
    }
    $('#chart-meta').textContent = `${pts.length} buckets · ${Math.round(data.bucket_secs / 60)}min/bucket`;
  }

  // ------------------------------------------------------------------
  //  Overview: live activity stream (right column) +
  //            recent exec list (bottom)
  // ------------------------------------------------------------------
  async function loadEvents() {
    const qs = new URLSearchParams();
    qs.set('limit', state.limit);
    qs.set('offset', 0);
    const res = await fetch(`/api/events?${qs.toString()}`);
    const data = await res.json();
    const events = data.events || [];

    // ----- live stream (top 7) -----
    const stream = $('#live-stream-body');
    stream.innerHTML = '';
    const streamItems = events.slice(0, 7);
    if (streamItems.length === 0) {
      stream.innerHTML = '<div class="stream-empty muted">这个区间还没有事件</div>';
    } else {
      for (const e of streamItems) {
        const item = document.createElement('div');
        const errKlass = e.status !== 'ok' ? ' err' : '';
        item.className = `stream-item${errKlass}`;
        item.dataset.id = e.id ?? '';
        const skill = (e.chosen && e.chosen[0]) || '';
        const nameHtml = skill
          ? `<div class="nm">${escapeHTML(skill)}</div>`
          : `<div class="nm-empty">(no skill)</div>`;
        const dur = e.status !== 'ok' && e.latency_ms == null
          ? 'error'
          : fmtMsDur(e.latency_ms);
        item.innerHTML = `
          <div class="ts">${fmtTime(e.ts)}</div>
          ${nameHtml}
          <div class="dur">${escapeHTML(dur)}</div>
        `;
        item.addEventListener('click', () => openDetail(e.id));
        stream.appendChild(item);
      }
    }

    // ----- hero strip cell 3: last activity -----
    if (events.length > 0) {
      const latest = events[0];
      $('#hero-strip-last').textContent = fmtAgo(latest.ts);
      const skill = (latest.chosen && latest.chosen[0]) || '';
      const latTxt = fmtMsDur(latest.latency_ms);
      $('#hero-strip-last-delta').textContent = skill ? `${skill} · ${latTxt}` : latTxt;
    } else {
      $('#hero-strip-last').textContent = '—';
      $('#hero-strip-last-delta').textContent = '';
    }

    // ----- recent exec list (8 rows) -----
    const list = $('#recent-list');
    list.innerHTML = '';
    if (events.length === 0) {
      list.innerHTML = '<div class="stream-empty muted">这个区间还没有事件</div>';
    } else {
      for (const e of events) {
        const row = document.createElement('div');
        row.className = 'recent-row';
        row.dataset.id = e.id ?? '';
        const skill = (e.chosen && e.chosen[0]) || '';
        const modeChar = (e.mode || '').toLowerCase().startsWith('e') ? 'e' : 'c';
        const modeText = (e.mode || '').toLowerCase();
        const okErr = e.status === 'ok' ? 'ok' : 'err';
        const okText = e.status === 'ok' ? 'ok' : escapeHTML(e.status || 'err');
        const promptShort = e.user_prompt ? e.user_prompt.slice(0, 80) : '';
        const nameHtml = skill
          ? `<div class="nm">${escapeHTML(skill)}${promptShort ? `<small>${escapeHTML(promptShort)}</small>` : ''}</div>`
          : `<div class="nm"><span class="nm-empty">(no skill)</span>${promptShort ? `<small>${escapeHTML(promptShort)}</small>` : ''}</div>`;
        row.innerHTML = `
          <div class="ts">${fmtTime(e.ts)}</div>
          ${nameHtml}
          <div class="mode ${modeChar}">${escapeHTML(modeText || '—')}</div>
          <div class="dur">${escapeHTML(fmtMsDur(e.latency_ms))}</div>
          <div class="tok">${fmtTok(e.prompt_tokens)} tok</div>
          <div class="st ${okErr}">${okText}</div>
        `;
        row.addEventListener('click', () => openDetail(e.id));
        list.appendChild(row);
      }
    }
    $('#recent-meta').textContent = `${events.length} 条 / 共 ${fmtInt(data.total)}`;
  }

