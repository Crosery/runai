  // ------------------------------------------------------------------
  //  Polling lifecycle
  // ------------------------------------------------------------------
  async function refresh() {
    if (inFlight) return;
    inFlight = true;
    try {
      // loadModelUsage is NOT listed here: loadSummary now renders the
      // model-usage panel from its own /api/summary per_model (0 extra
      // requests). See loadModelUsage below.
      await Promise.all([loadSummary(), loadTimeline(), loadEvents(), refreshSkillsCount()]);
      $('#live-text').textContent = '实时';
    } catch (_e) {
      $('#live-text').textContent = '断开';
    } finally {
      inFlight = false;
    }
  }

  // 模型用量：直接消费 loadSummary 已拿到的 /api/summary per_model，0 额外请求，
  // 跟 hero 总数完全同口径。旧实现分页拉最多 20 次 /api/events（每页 500）在浏览器
  // 聚合 7000 行，冷启动/高负载下 Promise.all 一旦超时就整面板清空并报"断开"——
  // 现在服务端一条 GROUP BY model 查询就返回 calls/total_tokens/avg_latency_ms/hits。
  // loadSummary 拿到数据后调用本函数（见 03-api-overview.js）；也可传 null 从
  // lastSummary 回退取。这里不再有任何 fetch。
  function loadModelUsage(perModel) {
    const list = document.getElementById('models-list');
    const meta = document.getElementById('models-meta');
    if (!list) return;
    const raw = Array.isArray(perModel)
      ? perModel
      : (lastSummary && lastSummary.per_model) || [];
    const sorted = raw
      .map((m) => ({
        name: m.model || '(unknown)',
        calls: m.calls || 0,
        avgLat: m.avg_latency_ms != null ? Math.round(m.avg_latency_ms) : null,
        hitRate: m.calls ? (m.hits || 0) / m.calls : 0,
        totalTok: m.total_tokens || 0,
      }))
      .sort((x, y) => y.calls - x.calls);

    const totalCalls = sorted.reduce((s, m) => s + m.calls, 0);
    if (meta) meta.textContent = `${sorted.length} 个模型 · 共 ${fmtInt(totalCalls)} 次`;

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
      const enriching = data.enriching || 0;
      $('#hero-strip-skills-delta').textContent = enriching
        ? `${data.enriched} 已富集 · ${enriching} 富集中`
        : `${data.enriched} 已富集`;
    } catch (_e) { /* ignore */ }
  }

  // Live-refresh the Library installed list so enrichment tags update on their
  // own (富集中 → 已富集) while sitting on the tab — the watcher / enrich run
  // server-side and there is no SSE, so we re-fetch on the shared poll timer.
  // Skipped while in select mode so a re-render can't drop the user's
  // selection mid-action.
  function maybeRefreshLibrary() {
    const rows = document.getElementById('skill-rows');
    if (rows && rows.classList.contains('select-mode')) return;
    if (typeof loadSkills === 'function') loadSkills();
  }

  function startPolling() {
    stopPolling();
    pollTimer = setInterval(() => {
      const dlg = document.getElementById('detail-dialog');
      if (dlg && dlg.open) return;
      const view = parseRoute().view;
      if (view === 'overview') { refresh(); return; }
      if (view === 'library') { maybeRefreshLibrary(); return; }
      // Skill detail: re-pull /api/skill/{name} so the feedback radar +
      // recent-votes list pick up feedback recorded from elsewhere (another
      // tab, another user, an event-dialog 准/不准 click) without the user
      // having to navigate away and back. This calls the LIGHT refresh
      // (06-library-detail.js::refreshSkillDetailLive), not loadSkillDetail
      // — the full paint blanks and rebuilds the whole page (file tree,
      // event table, file content re-fetch included) on every tick, which
      // is what made this view feel janky. The light path only moves the
      // radar polygon + feedback numbers, and no-ops entirely when nothing
      // changed since the last tick.
      if (view === 'detail' && detailState.name) { refreshSkillDetailLive(detailState.name); return; }
    }, POLL_INTERVAL_MS);
  }
  function stopPolling() {
    if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
  }
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) stopPolling();
    else { startPolling(); applyRoute(); }
  });

