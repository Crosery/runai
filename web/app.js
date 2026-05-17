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
  const detailState = { name: '', current: null };

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
    // Close the event-detail dialog whenever we navigate to a new route.
    // Without this, clicking a chosen-skill chip inside the dialog opens
    // the skill detail page but leaves the dialog floating on top — the
    // user sees the skill page through the backdrop.
    const dlg = document.getElementById('detail');
    if (dlg && dlg.open) dlg.close();
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
      <div class="section-label">user prompt (hook 收到的原文)</div>
      <div class="prompt-block">${escapeHTML(e.user_prompt) || '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">router LLM 实际收到的完整输入</div>
      <div class="prompt-block">${e.llm_input ? escapeHTML(e.llm_input) : '<span class="dim">(legacy row — 升级到 schema v13 之后的新事件才有)</span>'}</div>
      <div class="section-label">router LLM 原始返回</div>
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
      const sa = a.llm_score == null ? -1 : a.llm_score;
      const sb = b.llm_score == null ? -1 : b.llm_score;
      switch (sort) {
        case 'score-asc':  return sa - sb || a.name.localeCompare(b.name);
        case 'used-desc':  return (b.usage_count - a.usage_count) || sb - sa;
        case 'name':       return a.name.localeCompare(b.name);
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
        <td class="num">${scoreBadge(s.llm_score)}</td>
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
      `${data.total} 个 skill · ${data.enriched} 已富集`;
    renderSkillsRows();
  }

  // ------------------------------------------------------------------
  //  Skill detail page
  // ------------------------------------------------------------------
  async function loadSkillDetail(name) {
    detailState.name = name;
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
    $('#skill-detail-llm').innerHTML = `${d.llm_score ?? '—'}<span class="of"> / 10</span>`;
    $('#skill-detail-summary').textContent = d.summary || '(尚未富集 — 跑 `runai recommend enrich` 生成)';

    // Usage history: events where this skill was chosen
    const tbody = $('#skill-detail-events-body');
    tbody.innerHTML = '';
    const events = d.events || [];
    $('#skill-detail-events-meta').textContent = events.length ? `${events.length} 次注入` : '';
    $('#skill-detail-events-empty').hidden = events.length !== 0;
    for (const e of events) {
      const tr = document.createElement('tr');
      tr.dataset.id = e.id ?? '';
      const injected = e.injected
        ? `<span class="chip" style="background:var(--success-bg);color:var(--success);border-color:rgba(63,185,80,0.4)">是</span>`
        : `<span class="chip-empty">否</span>`;
      const promptShort = e.user_prompt ? e.user_prompt.slice(0, 140) : '<span class="dim">(legacy)</span>';
      tr.innerHTML = `
        <td class="mono muted">${fmtTs(e.ts)}</td>
        <td class="mono muted" title="${escapeHTML(e.session_id || '')}">${escapeHTML((e.session_id || '').slice(0, 8) || '—')}</td>
        <td class="muted mono">${escapeHTML(e.mode)}</td>
        <td class="num">${fmtMs(e.latency_ms)} ms</td>
        <td class="num">${fmtTok(e.prompt_tokens)}</td>
        <td>${injected}</td>
        <td class="prompt" title="${escapeHTML(e.user_prompt || '')}">${e.user_prompt ? escapeHTML(promptShort) : promptShort}</td>
      `;
      tr.addEventListener('click', () => openDetail(e.id));
      tbody.appendChild(tr);
    }

    await loadFileTree(name);
  }

  async function loadFileTree(name) {
    const res = await fetch(`/api/skill/${encodeURIComponent(name)}/files`);
    if (!res.ok) {
      $('#file-tree').innerHTML = '<div class="muted" style="padding:8px">无法读取 skill 目录</div>';
      $('#file-viewer-body').textContent = '';
      return;
    }
    const data = await res.json();
    $('#skill-detail-dir-meta').textContent =
      `${data.entries.length} 个文件 · ${data.skill_dir}`;
    const tree = $('#file-tree');
    tree.innerHTML = '';
    if (data.entries.length === 0) {
      tree.innerHTML = '<div class="muted" style="padding:8px">空目录</div>';
      $('#file-viewer-body').textContent = '';
      return;
    }
    for (const entry of data.entries) {
      const div = document.createElement('div');
      div.className = 'ftree-entry' + (entry.is_text ? '' : ' binary');
      div.dataset.path = entry.path;
      div.dataset.text = entry.is_text ? '1' : '0';
      div.innerHTML = `
        <span class="ftree-name">${escapeHTML(entry.path)}</span>
        <span class="ftree-size">${fmtBytes(entry.size)}</span>
      `;
      div.addEventListener('click', () => selectFile(name, entry.path));
      tree.appendChild(div);
    }
    // Auto-open SKILL.md if present, else the first text file, else first file
    const preferred =
      data.entries.find((e) => e.path === 'SKILL.md') ||
      data.entries.find((e) => e.is_text) ||
      data.entries[0];
    if (preferred) selectFile(name, preferred.path);
  }

  function fmtBytes(n) {
    if (n == null) return '—';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(2)} MB`;
  }

  async function selectFile(name, path) {
    document.querySelectorAll('#file-tree .ftree-entry').forEach((el) => {
      el.classList.toggle('active', el.dataset.path === path);
    });
    $('#file-viewer-path').textContent = path;
    $('#file-viewer-body').textContent = '加载中...';
    const url = `/api/skill/${encodeURIComponent(name)}/file?path=${encodeURIComponent(path)}`;
    const res = await fetch(url);
    if (!res.ok) {
      $('#file-viewer-body').textContent = '(读取失败)';
      $('#file-viewer-meta').textContent = '';
      return;
    }
    const f = await res.json();
    $('#file-viewer-meta').textContent =
      `${fmtBytes(f.size)}${f.truncated ? ' (truncated)' : ''}${f.is_text ? '' : ' · 二进制'}`;
    if (f.is_text) {
      $('#file-viewer-body').textContent = f.content || '(空文件)';
    } else {
      $('#file-viewer-body').textContent = `(二进制文件 — ${fmtBytes(f.size)} — 不显示内容)`;
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
  //  Custom dropdown — replaces native <select> popup so the option
  //  panel can be styled to match the dark glass theme. Keeps the
  //  underlying <select> in the DOM as data source so existing
  //  change-event wiring stays untouched.
  // ------------------------------------------------------------------
  function initCustomDropdown(select) {
    if (select.dataset.cddInit === '1') return;
    select.dataset.cddInit = '1';
    const wrap = document.createElement('div');
    wrap.className = 'cdd';
    select.parentNode.insertBefore(wrap, select);
    wrap.appendChild(select);
    select.classList.add('cdd-native');

    const trigger = document.createElement('button');
    trigger.type = 'button';
    trigger.className = 'cdd-trigger';
    trigger.innerHTML = '<span class="cdd-label"></span><span class="cdd-caret">&#9662;</span>';
    wrap.appendChild(trigger);

    const panel = document.createElement('div');
    panel.className = 'cdd-panel';
    panel.hidden = true;
    wrap.appendChild(panel);

    function render() {
      const cur = select.value;
      const opts = [...select.options];
      const sel = opts.find((o) => o.value === cur) || opts[0];
      trigger.querySelector('.cdd-label').textContent = sel ? sel.textContent : '';
      panel.innerHTML = '';
      for (const o of opts) {
        const item = document.createElement('div');
        item.className = 'cdd-item' + (o.value === cur ? ' active' : '');
        item.textContent = o.textContent;
        item.dataset.value = o.value;
        item.addEventListener('click', () => {
          if (select.value === o.value) { close(); return; }
          select.value = o.value;
          select.dispatchEvent(new Event('change', { bubbles: true }));
          render();
          close();
        });
        panel.appendChild(item);
      }
    }
    function open() {
      // close any other open dropdown first
      document.querySelectorAll('.cdd.cdd-open').forEach((o) => {
        if (o !== wrap) {
          o.querySelector('.cdd-panel').hidden = true;
          o.classList.remove('cdd-open');
        }
      });
      panel.hidden = false;
      wrap.classList.add('cdd-open');
    }
    function close() {
      panel.hidden = true;
      wrap.classList.remove('cdd-open');
    }
    trigger.addEventListener('click', (e) => {
      e.stopPropagation();
      if (panel.hidden) open(); else close();
    });
    document.addEventListener('click', (e) => {
      if (!wrap.contains(e.target)) close();
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape' && !panel.hidden) close();
    });
    // Watch for option list mutations (e.g. /api/overview rebuilds the
    // #model <select>'s children when a new model shows up).
    new MutationObserver(render).observe(select, { childList: true });
    render();
  }

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
    // Wrap every <select> in the filters bar AND the skills page with
    // the custom dropdown so the popup uses our dark glass theme
    // instead of the OS-native popup.
    document.querySelectorAll('.filters select, .skills-filters select').forEach(initCustomDropdown);
  }

  bind();
  applyRoute();
  startPolling();
})();
