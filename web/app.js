(() => {
  // ------------------------------------------------------------------
  //  Shared state + polling
  // ------------------------------------------------------------------
  const state = {
    hours: '24',
    model: '',
    hitOnly: false,
    offset: 0,
    limit: 50,
    total: 0,
  };
  const POLL_INTERVAL_MS = 5000;
  let pollTimer = null;
  let inFlight = false;

  // Skills page state
  const skillsState = { filter: '', sort: 'score-desc', cache: [] };

  // Skill detail page state
  const detailState = { name: '', current: null, pendingScore: null };

  const $ = (sel) => document.querySelector(sel);

  // ------------------------------------------------------------------
  //  Formatters
  // ------------------------------------------------------------------
  const pad = (n) => String(n).padStart(2, '0');
  const fmtTs = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  const fmtTsFull = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtInt = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMs  = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtTok = (n) => {
    if (n == null) return '—';
    if (n >= 10000) return `${(n / 1000).toFixed(1)}k`;
    return Math.round(n).toLocaleString();
  };
  const fmtPctRaw = (r) => (r == null ? null : (r * 100).toFixed(1));
  const escapeHTML = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    })[c]);

  function scoreBadge(score) {
    if (score == null) return `<span class="score-badge unknown">—</span>`;
    const klass = score >= 7 ? 'high' : score >= 4 ? 'mid' : 'low';
    return `<span class="score-badge ${klass}">${score}</span>`;
  }

  // ------------------------------------------------------------------
  //  Router (hash-based)
  // ------------------------------------------------------------------
  function parseRoute() {
    const h = location.hash.replace(/^#\/?/, '');
    if (!h) return { view: 'dashboard' };
    if (h === 'skills') return { view: 'skills' };
    const m = h.match(/^skill\/(.+)$/);
    if (m) return { view: 'skill', name: decodeURIComponent(m[1]) };
    return { view: 'dashboard' };
  }

  function applyRoute() {
    const r = parseRoute();
    document.querySelectorAll('.view').forEach((el) => (el.hidden = true));
    document.querySelectorAll('.route-tabs .tab').forEach((tab) => {
      const r2 = tab.dataset.route;
      const isActive =
        (r.view === 'dashboard' && r2 === '') ||
        (r.view === 'skills' && r2 === 'skills') ||
        (r.view === 'skill' && r2 === 'skills');
      tab.classList.toggle('active', isActive);
    });
    const filters = document.getElementById('dashboard-filters');
    if (filters) filters.style.visibility = r.view === 'dashboard' ? 'visible' : 'hidden';
    switch (r.view) {
      case 'skills':
        $('#view-skills').hidden = false;
        loadSkills();
        break;
      case 'skill':
        $('#view-skill-detail').hidden = false;
        loadSkillDetail(r.name);
        break;
      case 'dashboard':
      default:
        $('#view-dashboard').hidden = false;
        refresh();
        break;
    }
  }

  window.addEventListener('hashchange', applyRoute);

  // ------------------------------------------------------------------
  //  API helpers
  // ------------------------------------------------------------------
  function buildEventsQs() {
    const p = new URLSearchParams();
    if (state.hours) p.set('hours', state.hours);
    if (state.model) p.set('model', state.model);
    if (state.hitOnly) p.set('hit_only', 'true');
    p.set('limit', state.limit);
    p.set('offset', state.offset);
    return p.toString();
  }
  function summaryQs() {
    const p = new URLSearchParams();
    if (state.hours) p.set('hours', state.hours);
    return p.toString();
  }
  function timelineQs() {
    const p = new URLSearchParams();
    p.set('hours', state.hours || '24');
    return p.toString();
  }

  // ------------------------------------------------------------------
  //  Dashboard: summary
  // ------------------------------------------------------------------
  async function loadSummary() {
    const res = await fetch(`/api/summary?${summaryQs()}`);
    const data = await res.json();
    $('#kpi-total').innerHTML = fmtInt(data.total);
    const hit = fmtPctRaw(data.hit_rate);
    $('#kpi-hit').innerHTML = `${hit ?? '—'}<span class="unit">%</span>`;
    $('#kpi-hit-sub').textContent = `${fmtInt(data.hits)} / ${fmtInt(data.total)} 命中`;
    $('#kpi-latency').innerHTML = `${fmtMs(data.avg_latency_ms)}<span class="unit">ms</span>`;
    $('#kpi-tokens').innerHTML = `${fmtInt(data.avg_prompt_tokens)}<span class="unit">tok</span>`;
    $('#kpi-tokens-sub').textContent = `累计 ${fmtTok(data.total_tokens)} tok`;
    $('#kpi-errors').textContent = fmtInt(data.errors);

    const strip = $('#model-strip');
    strip.innerHTML = '';
    for (const m of data.per_model) {
      const el = document.createElement('div');
      el.className = 'model-pill';
      el.innerHTML = `<span class="name">${escapeHTML(m.model)}</span>
        <span class="stat"><span class="num">${fmtInt(m.calls)}</span> 调用 · <span class="num">${fmtTok(m.total_tokens)}</span> tok</span>`;
      strip.appendChild(el);
    }
    $('#model-count').textContent = `${data.per_model.length} 个模型`;

    const sel = $('#model');
    const current = sel.value;
    sel.innerHTML = '<option value="">全部模型</option>';
    for (const m of data.per_model) {
      const opt = document.createElement('option');
      opt.value = m.model;
      opt.textContent = m.model;
      if (m.model === current) opt.selected = true;
      sel.appendChild(opt);
    }
  }

  // ------------------------------------------------------------------
  //  Dashboard: trend chart
  // ------------------------------------------------------------------
  async function loadTimeline() {
    const res = await fetch(`/api/timeline?${timelineQs()}`);
    const data = await res.json();
    const pts = data.points;
    const svg = $('#trend-chart');
    const W = 800, H = 200;
    const PAD = { top: 14, right: 14, bottom: 22, left: 36 };
    const innerW = W - PAD.left - PAD.right;
    const innerH = H - PAD.top - PAD.bottom;

    const maxTotal = Math.max(1, ...pts.map((p) => p.total));
    const xStep = innerW / Math.max(1, pts.length - 1);
    const xy = (i, v) => [PAD.left + i * xStep, PAD.top + innerH - (v / maxTotal) * innerH];

    const grid = svg.querySelector('.grid');
    grid.innerHTML = '';
    for (let g = 0; g <= 4; g++) {
      const y = PAD.top + (innerH * g) / 4;
      grid.insertAdjacentHTML(
        'beforeend',
        `<line x1="${PAD.left}" y1="${y}" x2="${W - PAD.right}" y2="${y}"/>` +
          `<text x="${PAD.left - 6}" y="${y + 3}" text-anchor="end" fill="var(--fg-subtle)" font-size="10">${Math.round(maxTotal - (maxTotal * g) / 4)}</text>`
      );
    }

    const axisX = svg.querySelector('.axis-x');
    axisX.innerHTML = '';
    const tickCount = Math.min(6, pts.length);
    for (let t = 0; t < tickCount; t++) {
      const i = Math.round(((pts.length - 1) * t) / (tickCount - 1));
      const x = PAD.left + i * xStep;
      const ts = pts[i]?.ts_start;
      if (ts == null) continue;
      axisX.insertAdjacentHTML('beforeend', `<text x="${x}" y="${H - 6}" text-anchor="middle">${fmtTs(ts)}</text>`);
    }

    const series = svg.querySelector('.series');
    series.innerHTML = '';
    const barW = Math.max(2, xStep * 0.5);
    for (let i = 0; i < pts.length; i++) {
      const e = pts[i].errors;
      if (!e) continue;
      const [x, y] = xy(i, e);
      const h = PAD.top + innerH - y;
      series.insertAdjacentHTML('beforeend', `<rect class="bar-err" x="${x - barW / 2}" y="${y}" width="${barW}" height="${h}" rx="1"/>`);
    }
    if (pts.length > 1) {
      const totalPath = pts.map((p, i) => { const [x, y] = xy(i, p.total); return `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`; }).join(' ');
      const areaPath = totalPath +
        ` L ${(PAD.left + (pts.length - 1) * xStep).toFixed(1)} ${(PAD.top + innerH).toFixed(1)}` +
        ` L ${PAD.left.toFixed(1)} ${(PAD.top + innerH).toFixed(1)} Z`;
      series.insertAdjacentHTML('beforeend', `<path class="area-total" d="${areaPath}"/>`);
      series.insertAdjacentHTML('beforeend', `<path class="line-total" d="${totalPath}"/>`);
      const hitPath = pts.map((p, i) => { const [x, y] = xy(i, p.hits); return `${i === 0 ? 'M' : 'L'} ${x.toFixed(1)} ${y.toFixed(1)}`; }).join(' ');
      series.insertAdjacentHTML('beforeend', `<path class="line-hit" d="${hitPath}"/>`);
    }
  }

  // ------------------------------------------------------------------
  //  Dashboard: events table
  // ------------------------------------------------------------------
  async function loadEvents() {
    const res = await fetch(`/api/events?${buildEventsQs()}`);
    const data = await res.json();
    state.total = data.total;
    const tbody = $('#events-body');
    tbody.innerHTML = '';
    $('#empty-state').hidden = data.events.length !== 0;

    for (const e of data.events) {
      const tr = document.createElement('tr');
      tr.dataset.id = e.id ?? '';
      const statusKlass = e.status === 'ok' ? 'status-ok' : 'status-error';
      const chosenHtml = e.chosen.length
        ? e.chosen.map((s) => `<span class="chip">${escapeHTML(s)}</span>`).join('')
        : `<span class="chip-empty">(空)</span>`;
      const ratio = e.candidate_count > 0 ? Math.min(1, e.bm25_kept / e.candidate_count) : 0;
      const bm25Html = e.candidate_count > 0
        ? `<div class="ratio-bar"><div class="fill" style="width:${(ratio * 100).toFixed(0)}%"></div></div><span class="ratio-text">${e.bm25_kept}/${e.candidate_count}</span>`
        : `<span class="dim">—</span>`;
      const promptShort = e.user_prompt ? e.user_prompt.slice(0, 140) : '<span class="dim">(legacy row)</span>';
      tr.innerHTML = `
        <td class="mono muted">${fmtTs(e.ts)}</td>
        <td><span class="mono">${escapeHTML(e.model)}</span></td>
        <td class="${statusKlass}"><span class="status-dot"></span>${escapeHTML(e.status)}</td>
        <td class="muted mono">${escapeHTML(e.mode)}</td>
        <td>${bm25Html}</td>
        <td class="num">${fmtTok(e.prompt_tokens)}</td>
        <td class="num">${fmtMs(e.latency_ms)} ms</td>
        <td>${chosenHtml}</td>
        <td class="prompt" title="${escapeHTML(e.user_prompt || '')}">${e.user_prompt ? escapeHTML(promptShort) : promptShort}</td>
      `;
      tr.addEventListener('click', () => openDetail(e.id));
      tbody.appendChild(tr);
    }
    $('#events-count').textContent = `${data.total.toLocaleString()} 行匹配`;
    const page = Math.floor(state.offset / state.limit) + 1;
    const pages = Math.max(1, Math.ceil(state.total / state.limit));
    $('#page-info').textContent = `第 ${page} / ${pages} 页`;
    $('#prev').disabled = state.offset === 0;
    $('#next').disabled = state.offset + state.limit >= state.total;
  }

  async function openDetail(id) {
    if (id == null) return;
    const res = await fetch(`/api/event/${id}`);
    if (!res.ok) return;
    const e = await res.json();
    $('#detail-id').textContent = `#${e.id}`;
    const body = $('#detail-body');
    const chosenInline = e.chosen.length
      ? e.chosen.map((s) => `<a class="chip" href="#/skill/${encodeURIComponent(s)}">${escapeHTML(s)}</a>`).join('')
      : '<span class="dim">空集</span>';
    const statusKlass = e.status === 'ok' ? 'status-ok' : 'status-error';
    const injectedBadge = e.injected
      ? `<span class="chip" style="background:var(--success-bg);color:var(--success);border-color:rgba(63,185,80,0.4)">已注入</span>`
      : `<span class="chip-empty">未注入</span>`;
    body.innerHTML = `
      <dl class="detail-grid">
        <dt>时间</dt><dd class="mono">${fmtTsFull(e.ts)}</dd>
        <dt>状态</dt><dd class="${statusKlass}"><span class="status-dot"></span>${escapeHTML(e.status)}${e.error_msg ? ` <span class="muted">— ${escapeHTML(e.error_msg)}</span>` : ''}</dd>
        <dt>注入</dt><dd>${injectedBadge}</dd>
        <dt>模型</dt><dd><span class="mono">${escapeHTML(e.provider)} · ${escapeHTML(e.model)}</span></dd>
        <dt>模式</dt><dd class="mono">${escapeHTML(e.mode)}</dd>
        <dt>session</dt><dd class="mono muted">${escapeHTML(e.session_id || '(none)')}</dd>
        <dt>BM25</dt><dd>${e.bm25_kept} / ${e.candidate_count} 候选</dd>
        <dt>token</dt><dd>prompt <span class="mono">${fmtInt(e.prompt_tokens)}</span> · completion <span class="mono">${fmtInt(e.completion_tokens)}</span> · total <span class="mono">${fmtInt(e.total_tokens)}</span></dd>
        <dt>延迟</dt><dd><span class="mono">${fmtMs(e.latency_ms)} ms</span></dd>
        <dt>cwd</dt><dd class="mono muted">${escapeHTML(e.cwd || '(none)')}</dd>
      </dl>
      <div class="section-label">chosen skills (点击进详情)</div>
      <div>${chosenInline}</div>
      <div class="section-label">user prompt</div>
      <div class="prompt-block">${escapeHTML(e.user_prompt) || '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">router 模型原始返回</div>
      <div class="prompt-block">${e.llm_raw_response ? escapeHTML(e.llm_raw_response) : '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">hook 注入给 Claude Code 的内容</div>
      <div class="prompt-block">${e.hook_output ? escapeHTML(e.hook_output) : '<span class="dim">(本次没有注入)</span>'}</div>
    `;
    $('#detail').showModal();
  }

  // ------------------------------------------------------------------
  //  Skills list page
  // ------------------------------------------------------------------
  function renderSkillsRows() {
    let rows = skillsState.cache.slice();
    const f = skillsState.filter.toLowerCase().trim();
    if (f) {
      rows = rows.filter((s) =>
        s.name.toLowerCase().includes(f) ||
        (s.description || '').toLowerCase().includes(f) ||
        (s.summary || '').toLowerCase().includes(f)
      );
    }
    const sort = skillsState.sort;
    rows.sort((a, b) => {
      const sa = a.combined_score == null ? -1 : a.combined_score;
      const sb = b.combined_score == null ? -1 : b.combined_score;
      switch (sort) {
        case 'score-asc':  return sa - sb || a.name.localeCompare(b.name);
        case 'used-desc':  return (b.usage_count - a.usage_count) || sb - sa;
        case 'name':       return a.name.localeCompare(b.name);
        case 'unrated':    return (a.user_score == null ? -1 : 1) - (b.user_score == null ? -1 : 1) || sb - sa;
        case 'unenriched': return ((a.summary ? 1 : -1) - (b.summary ? 1 : -1)) || sb - sa;
        case 'score-desc':
        default:           return sb - sa || a.name.localeCompare(b.name);
      }
    });

    const body = $('#skills-body');
    body.innerHTML = '';
    for (const s of rows) {
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td class="skill-name" data-name="${escapeHTML(s.name)}">${escapeHTML(s.name)}</td>
        <td class="num">${s.usage_count || 0}</td>
        <td class="num">${scoreBadge(s.summary ? s.llm_score : null)}</td>
        <td class="num">${scoreBadge(s.user_score)}</td>
        <td class="num">${scoreBadge(s.combined_score)}</td>
        <td class="skill-desc">${escapeHTML((s.description || '').slice(0, 200))}</td>
      `;
      tr.addEventListener('click', () => {
        location.hash = `#/skill/${encodeURIComponent(s.name)}`;
      });
      body.appendChild(tr);
    }
  }

  async function loadSkills() {
    const res = await fetch('/api/skills');
    if (!res.ok) return;
    const data = await res.json();
    skillsState.cache = data.skills;
    $('#skills-progress').textContent =
      `${data.total} 个 skill · ${data.enriched} 已富集 · ${data.rated} 已评分`;
    renderSkillsRows();
  }

  // ------------------------------------------------------------------
  //  Skill detail page
  // ------------------------------------------------------------------
  function renderScoreButtons(current) {
    const wrap = $('#score-buttons');
    wrap.innerHTML = '';
    for (let n = 1; n <= 10; n++) {
      const el = document.createElement('span');
      el.className = 'pip' + (current === n ? ' active' : '');
      el.textContent = n;
      el.dataset.n = n;
      el.addEventListener('mouseover', () => {
        wrap.querySelectorAll('.pip').forEach((p) => {
          p.classList.toggle('preview', Number(p.dataset.n) <= n && !p.classList.contains('active'));
        });
      });
      el.addEventListener('mouseout', () => {
        wrap.querySelectorAll('.pip').forEach((p) => p.classList.remove('preview'));
      });
      el.addEventListener('click', () => {
        detailState.pendingScore = n;
        wrap.querySelectorAll('.pip').forEach((p) => {
          p.classList.toggle('active', Number(p.dataset.n) === n);
          p.classList.remove('preview');
        });
      });
      wrap.appendChild(el);
    }
  }

  async function loadSkillDetail(name) {
    detailState.name = name;
    detailState.pendingScore = null;
    const res = await fetch(`/api/skill/${encodeURIComponent(name)}`);
    if (!res.ok) {
      $('#skill-detail-name').textContent = '加载失败';
      return;
    }
    const d = await res.json();
    detailState.current = d;
    $('#skill-detail-name').textContent = d.name;
    $('#skill-detail-desc').textContent = d.description || '(no description)';
    $('#skill-detail-used').textContent = d.usage_count;
    $('#skill-detail-llm').innerHTML = `${d.summary ? d.llm_score : '—'}<span class="of"> / 10</span>`;
    $('#skill-detail-user').innerHTML = `${d.user_score ?? '—'}<span class="of"> / 10</span>`;
    $('#skill-detail-combined').innerHTML = `${d.combined_score ?? '—'}<span class="of"> / 10</span>`;
    renderScoreButtons(d.user_score);
    $('#rating-note').value = d.user_note || '';
    $('#rating-clear').hidden = d.user_score == null;
    $('#rating-meta').textContent = d.rating_updated_at
      ? `上次更新：${fmtTsFull(d.rating_updated_at)}`
      : '';
    $('#skill-detail-summary').textContent = d.summary || '(尚未富集 — 跑 `runai recommend enrich` 生成)';
    const md = d.skill_md_content || '(没找到 SKILL.md)';
    $('#skill-detail-md').textContent = md;
    $('#skill-detail-md-meta').textContent = d.skill_md_size
      ? `${d.skill_md_size.toLocaleString()} bytes${d.skill_md_truncated ? ' (truncated)' : ''}`
      : '';
  }

  async function saveRating() {
    const name = detailState.name;
    const score = detailState.pendingScore ?? detailState.current?.user_score;
    if (!score) {
      alert('先点 1-10 选个分数');
      return;
    }
    const note = $('#rating-note').value.trim();
    const res = await fetch(`/api/skills/${encodeURIComponent(name)}/rating`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ score, note }),
    });
    if (res.ok) {
      await loadSkillDetail(name);
    }
  }

  async function clearRating() {
    const name = detailState.name;
    const res = await fetch(`/api/skills/${encodeURIComponent(name)}/rating`, { method: 'DELETE' });
    if (res.ok) {
      await loadSkillDetail(name);
    }
  }

  // ------------------------------------------------------------------
  //  Polling lifecycle
  // ------------------------------------------------------------------
  async function refresh() {
    if (inFlight) return;
    inFlight = true;
    try {
      await Promise.all([loadSummary(), loadTimeline(), loadEvents()]);
      $('#live-text').textContent = '实时';
    } catch (_e) {
      $('#live-text').textContent = '断开';
    } finally {
      inFlight = false;
    }
  }

  function startPolling() {
    stopPolling();
    pollTimer = setInterval(() => {
      const dlg = document.getElementById('detail');
      if (dlg && dlg.open) return;
      if (parseRoute().view !== 'dashboard') return;
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

  // ------------------------------------------------------------------
  //  Wiring
  // ------------------------------------------------------------------
  function bind() {
    $('#hours').addEventListener('change', (e) => { state.hours = e.target.value; state.offset = 0; refresh(); });
    $('#model').addEventListener('change', (e) => { state.model = e.target.value; state.offset = 0; loadEvents(); });
    $('#hit_only').addEventListener('change', (e) => { state.hitOnly = e.target.checked; state.offset = 0; loadEvents(); });
    $('#refresh').addEventListener('click', refresh);
    $('#prev').addEventListener('click', () => { state.offset = Math.max(0, state.offset - state.limit); loadEvents(); });
    $('#next').addEventListener('click', () => { state.offset += state.limit; loadEvents(); });
    $('#detail-close').addEventListener('click', () => $('#detail').close());
    $('#skill-filter').addEventListener('input', (e) => { skillsState.filter = e.target.value; renderSkillsRows(); });
    $('#skill-sort').addEventListener('change', (e) => { skillsState.sort = e.target.value; renderSkillsRows(); });
    $('#rating-save').addEventListener('click', saveRating);
    $('#rating-clear').addEventListener('click', clearRating);
  }

  bind();
  applyRoute();
  startPolling();
})();
