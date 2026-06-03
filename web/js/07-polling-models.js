  // ------------------------------------------------------------------
  //  Polling lifecycle
  // ------------------------------------------------------------------
  async function refresh() {
    if (inFlight) return;
    inFlight = true;
    try {
      await Promise.all([loadSummary(), loadTimeline(), loadEvents(), refreshSkillsCount(), loadModelUsage()]);
      $('#live-text').textContent = '实时';
    } catch (_e) {
      $('#live-text').textContent = '断开';
    } finally {
      inFlight = false;
    }
  }

  // 模型用量：拉最近 24h 的 events，客户端按 model 聚合
  // 后端没有 /api/models 分组接口，分页拉全 24h 数据后客户端 reduce
  async function loadModelUsage() {
    const list = document.getElementById('models-list');
    const meta = document.getElementById('models-meta');
    if (!list) return;
    try {
      const agg = new Map();
      let totalCalls = 0;
      let offset = 0;
      const pageSize = 500;
      let backendTotal = null;

      // 分页拉全部历史事件，跟 hero 总数同口径，最多 10k 防失控
      while (offset < 10000) {
        const qs = new URLSearchParams();
        qs.set('limit', pageSize);
        qs.set('offset', offset);
        const res = await fetch(`/api/events?${qs.toString()}`);
        if (!res.ok) break;
        const data = await res.json();
        const rows = data.events || data.rows || [];
        if (backendTotal == null && data.total != null) backendTotal = data.total;

        for (const e of rows) {
          const m = e.model || '(unknown)';
          totalCalls++;
          let a = agg.get(m);
          if (!a) {
            a = { calls: 0, latSum: 0, latCount: 0, hits: 0, tokSum: 0 };
            agg.set(m, a);
          }
          a.calls++;
          if (e.latency_ms != null) { a.latSum += e.latency_ms; a.latCount++; }
          if (Array.isArray(e.chosen) && e.chosen.length) a.hits++;
          if (e.prompt_tokens != null) a.tokSum += e.prompt_tokens;
        }

        if (rows.length < pageSize) break;
        offset += pageSize;
      }

      const sorted = [...agg.entries()]
        .map(([name, a]) => ({
          name,
          calls: a.calls,
          avgLat: a.latCount ? Math.round(a.latSum / a.latCount) : null,
          hitRate: a.calls ? a.hits / a.calls : 0,
          totalTok: a.tokSum,
        }))
        .sort((x, y) => y.calls - x.calls);

      const totalForMeta = backendTotal != null ? backendTotal : totalCalls;
      meta.textContent = `${agg.size} 个模型 · 共 ${fmtInt(totalForMeta)} 次`;

      list.innerHTML = '';
      if (sorted.length === 0) {
        list.innerHTML = '<div class="models-empty">这个区间还没用过任何模型</div>';
        return;
      }
      for (const m of sorted.slice(0, 8)) {
        const parts = m.name.split('/');
        const brand = parts.length > 1 ? parts[0] : '';
        const model = parts.length > 1 ? parts.slice(1).join('/') : m.name;
        const row = document.createElement('div');
        row.className = 'model-row';
        row.innerHTML = `
          <div class="mname">${brand ? `<span class="mbrand">${escapeHTML(brand)}/</span>` : ''}${escapeHTML(model)}</div>
          <div class="mcalls">${fmtInt(m.calls)}</div>
          <div class="mlat">${m.avgLat != null ? fmtMsDur(m.avgLat) : '—'}</div>
          <div class="mhit">${Math.round(m.hitRate * 100)}%</div>
          <div class="mtok">${fmtTok(m.totalTok)} tok</div>
        `;
        list.appendChild(row);
      }
    } catch (_) {
      meta.textContent = '—';
    }
  }

  // Pulls /api/skills once on overview load too — purely to power the
  // "installed skills" cell in the hero strip. Cached after first call.
  let skillsCountCache = null;
  async function refreshSkillsCount() {
    if (skillsCountCache != null) return;
    try {
      const res = await fetch('/api/skills');
      if (!res.ok) return;
      const data = await res.json();
      skillsCountCache = data;
      $('#hero-strip-skills').textContent = data.total;
      $('#hero-strip-skills-delta').textContent = `${data.enriched} enriched`;
    } catch (_e) { /* ignore */ }
  }

  function startPolling() {
    stopPolling();
    pollTimer = setInterval(() => {
      const dlg = document.getElementById('detail-dialog');
      if (dlg && dlg.open) return;
      if (parseRoute().view !== 'overview') return;
      refresh();
    }, POLL_INTERVAL_MS);
  }
  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  }
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) stopPolling();
    else { startPolling(); applyRoute(); }
  });

