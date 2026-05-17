(() => {
  const state = {
    hours: '24',
    model: '',
    hitOnly: false,
    offset: 0,
    limit: 50,
    total: 0,
  };

  const $ = (sel) => document.querySelector(sel);
  const fmtTs = (ts) => {
    const d = new Date(ts * 1000);
    const pad = (n) => String(n).padStart(2, '0');
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtNum = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMs = (n) => (n == null ? '—' : `${Math.round(n)} ms`);
  const fmtPct = (r) => (r == null ? '—' : `${(r * 100).toFixed(1)}%`);
  const escapeHTML = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({
      '&': '&amp;',
      '<': '&lt;',
      '>': '&gt;',
      '"': '&quot;',
      "'": '&#39;',
    })[c]);

  function qs() {
    const p = new URLSearchParams();
    if (state.hours) p.set('hours', state.hours);
    if (state.model) p.set('model', state.model);
    if (state.hitOnly) p.set('hit_only', 'true');
    return p.toString();
  }

  async function loadSummary() {
    const p = new URLSearchParams();
    if (state.hours) p.set('hours', state.hours);
    const res = await fetch(`/api/summary?${p}`);
    const data = await res.json();
    $('#kpi-total').textContent = fmtNum(data.total);
    $('#kpi-hit').textContent = fmtPct(data.hit_rate);
    $('#kpi-latency').textContent = fmtMs(data.avg_latency_ms);
    $('#kpi-tokens').textContent = fmtNum(data.avg_prompt_tokens);
    $('#kpi-errors').textContent = fmtNum(data.errors);

    const tbody = $('#model-table tbody');
    tbody.innerHTML = '';
    for (const m of data.per_model) {
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td>${escapeHTML(m.model)}</td>
        <td class="num">${fmtNum(m.calls)}</td>
        <td class="num">${fmtNum(m.total_tokens)}</td>
      `;
      tbody.appendChild(tr);
    }

    // Populate the model filter dropdown (preserve current selection if any).
    const sel = $('#model');
    const current = sel.value;
    sel.innerHTML = '<option value="">全部</option>';
    for (const m of data.per_model) {
      const opt = document.createElement('option');
      opt.value = m.model;
      opt.textContent = m.model;
      if (m.model === current) opt.selected = true;
      sel.appendChild(opt);
    }
  }

  async function loadEvents() {
    const p = new URLSearchParams(qs());
    p.set('limit', state.limit);
    p.set('offset', state.offset);
    const res = await fetch(`/api/events?${p}`);
    const data = await res.json();
    state.total = data.total;

    const tbody = $('#events-table tbody');
    tbody.innerHTML = '';
    for (const e of data.events) {
      const tr = document.createElement('tr');
      tr.dataset.id = e.id ?? '';
      const chosen = e.chosen.length
        ? `<td class="chosen">${e.chosen.map(escapeHTML).join(', ')}</td>`
        : `<td class="empty-chosen">(空)</td>`;
      const statusClass = e.status === 'ok' ? 'status-ok' : 'status-error';
      const promptShort = e.user_prompt.slice(0, 120);
      tr.innerHTML = `
        <td>${fmtTs(e.ts)}</td>
        <td>${escapeHTML(e.model)}</td>
        <td class="${statusClass}">${e.status}</td>
        <td>${escapeHTML(e.mode)}</td>
        <td class="num">${e.bm25_kept}/${e.candidate_count}</td>
        <td class="num">${fmtNum(e.prompt_tokens)}</td>
        <td class="num">${fmtMs(e.latency_ms)}</td>
        ${chosen}
        <td class="prompt" title="${escapeHTML(e.user_prompt)}">${escapeHTML(promptShort)}</td>
      `;
      tr.addEventListener('click', () => openDetail(e.id));
      tbody.appendChild(tr);
    }

    $('#events-count').textContent = `(${data.total} 行匹配)`;
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
    body.innerHTML = `
      <dl>
        <dt>时间</dt><dd>${fmtTs(e.ts)}</dd>
        <dt>provider</dt><dd>${escapeHTML(e.provider)}</dd>
        <dt>model</dt><dd>${escapeHTML(e.model)}</dd>
        <dt>status</dt><dd>${escapeHTML(e.status)}${e.error_msg ? ` — <span class="muted">${escapeHTML(e.error_msg)}</span>` : ''}</dd>
        <dt>mode</dt><dd>${escapeHTML(e.mode)}</dd>
        <dt>session_id</dt><dd><code>${escapeHTML(e.session_id || '(none)')}</code></dd>
        <dt>BM25 prefilter</dt><dd>${e.bm25_kept} / ${e.candidate_count}</dd>
        <dt>token 用量</dt><dd>prompt ${fmtNum(e.prompt_tokens)} · completion ${fmtNum(e.completion_tokens)} · total ${fmtNum(e.total_tokens)}</dd>
        <dt>latency</dt><dd>${fmtMs(e.latency_ms)}</dd>
        <dt>chosen</dt><dd>${e.chosen.length ? e.chosen.map(escapeHTML).join(', ') : '<span class="muted">(空)</span>'}</dd>
        <dt>cwd</dt><dd><code>${escapeHTML(e.cwd || '(none)')}</code></dd>
        <dt>user prompt</dt>
        <dd><pre>${escapeHTML(e.user_prompt) || '<span class="muted">(empty)</span>'}</pre></dd>
      </dl>
    `;
    $('#detail').showModal();
  }

  function bind() {
    $('#hours').addEventListener('change', (e) => { state.hours = e.target.value; state.offset = 0; refresh(); });
    $('#model').addEventListener('change', (e) => { state.model = e.target.value; state.offset = 0; loadEvents(); });
    $('#hit_only').addEventListener('change', (e) => { state.hitOnly = e.target.checked; state.offset = 0; loadEvents(); });
    $('#refresh').addEventListener('click', refresh);
    $('#prev').addEventListener('click', () => { state.offset = Math.max(0, state.offset - state.limit); loadEvents(); });
    $('#next').addEventListener('click', () => { state.offset += state.limit; loadEvents(); });
    $('#detail-close').addEventListener('click', () => $('#detail').close());
  }

  async function refresh() {
    await loadSummary();
    await loadEvents();
  }

  bind();
  refresh();
})();
