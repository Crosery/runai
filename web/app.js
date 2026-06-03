(() => {
  // ==================================================================
  //  runai dashboard — editorial / Linear-style production frontend
  //  Two views via hash router: Overview (#/) and Library (#/skills),
  //  plus a Skill Detail view (#/skill/<name>) shown inside Library.
  //
  //  Backend contract:
  //    GET /api/summary?hours=N
  //    GET /api/timeline?hours=N
  //    GET /api/events?hours=N&limit=N&offset=N
  //    GET /api/event/{id}
  //    GET /api/skills
  //    GET /api/skill/{name}
  //    GET /api/skill/{name}/files
  //    GET /api/skill/{name}/file?path=X
  // ==================================================================

  const state = {
    hours: '24',
    offset: 0,
    limit: 8,
  };
  const POLL_INTERVAL_MS = 5000;
  let pollTimer = null;
  let inFlight = false;

  const skillsState = { filter: '', sort: 'score-desc', cache: [] };
  const detailState = { name: '', current: null, files: null, activeFile: null };

  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => Array.from(document.querySelectorAll(sel));

  // ------------------------------------------------------------------
  //  Formatters
  // ------------------------------------------------------------------
  const pad = (n) => String(n).padStart(2, '0');
  const fmtTime = (ts) => {
    const d = new Date(ts * 1000);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtTs = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  const fmtTsFull = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtAgo = (ts) => {
    if (!ts) return '—';
    const now = Math.floor(Date.now() / 1000);
    const s = Math.max(0, now - ts);
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86400)}d ago`;
  };
  const fmtInt = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMs  = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMsDur = (n) => {
    if (n == null) return '—';
    if (n >= 10000) return `${(n / 1000).toFixed(1)}s`;
    if (n >= 1000)  return `${(n / 1000).toFixed(2)}s`;
    return `${Math.round(n)}ms`;
  };
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
  const fmtBytes = (n) => {
    if (n == null) return '—';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(2)} MB`;
  };

  function todayLabel() {
    return 'lifetime · 全部历史累计';
  }

  // ------------------------------------------------------------------
  //  Router (hash-based)
  // ------------------------------------------------------------------
  function parseRoute() {
    const h = location.hash.replace(/^#\/?/, '');
    if (!h) return { view: 'overview' };
    if (h === 'skills') return { view: 'library' };
    if (h === 'activity') return { view: 'activity' };
    if (h === 'settings') return { view: 'settings' };
    if (h === 'admin') return { view: 'admin' };
    if (h === 'market') return { view: 'market' };
    const m = h.match(/^skill\/(.+)$/);
    if (m) return { view: 'detail', name: decodeURIComponent(m[1]) };
    return { view: 'overview' };
  }

  function setActivePane(name) {
    $$('.tab').forEach((el) => el.classList.remove('active'));
    const pane = document.querySelector(`.tab[data-pane="${name}"]`);
    if (pane) pane.classList.add('active');
  }

  function setActiveNav(route) {
    $$('.side .nav-item').forEach((el) => el.classList.remove('active'));
    // Library is the active nav for both 'library' and 'detail' views.
    let navRoute = route;
    if (route === 'skills' || route === 'detail') navRoute = 'skills';
    else if (route === 'activity') navRoute = 'activity';
    else if (route === 'settings') navRoute = 'settings';
    else if (route === 'admin') navRoute = 'admin';
    else if (route === 'market') navRoute = 'market';
    else navRoute = '';
    const target = document.querySelector(`.side .nav-item[data-route="${navRoute}"]`);
    if (target) target.classList.add('active');
  }

  function applyRoute() {
    const r = parseRoute();
    const dlg = document.getElementById('detail-dialog');
    if (dlg && dlg.open) dlg.close();

    switch (r.view) {
      case 'library':
        setActivePane('skills');
        setActiveNav('skills');
        loadSkills();
        break;
      case 'detail':
        setActivePane('detail');
        setActiveNav('skills');
        loadSkillDetail(r.name);
        break;
      case 'activity':
        setActivePane('activity');
        setActiveNav('activity');
        loadActivity();
        break;
      case 'settings':
        setActivePane('settings');
        setActiveNav('settings');
        loadSettings();
        break;
      case 'admin':
        // Non-admins bounce back to overview rather than seeing an empty
        // admin pane. The sidebar link is also hidden for them.
        if (!account.me || !account.me.is_admin) {
          location.hash = '#/';
          return;
        }
        setActivePane('admin');
        setActiveNav('admin');
        loadSettings();
        loadAdminUsers();
        break;
      case 'market':
        if (!account.me) {
          showAuthModal('login');
          location.hash = '#/';
          return;
        }
        setActivePane('market');
        setActiveNav('market');
        loadMarket();
        break;
      case 'overview':
      default:
        setActivePane('overview');
        setActiveNav('');
        refresh();
        break;
    }
    window.scrollTo({ top: 0, behavior: 'instant' in window ? 'instant' : 'auto' });
  }

  window.addEventListener('hashchange', applyRoute);

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

  // ------------------------------------------------------------------
  //  Activity tab — full event list + chart + filter
  // ------------------------------------------------------------------
  const activityState = {
    hours: '24',
    hitOnly: '',
    filter: '',
    limit: 100,
    offset: 0,
    total: 0,
  };

  async function loadActivity() {
    const qs = new URLSearchParams();
    if (activityState.hours) qs.set('hours', activityState.hours);
    qs.set('limit', activityState.limit);
    qs.set('offset', activityState.offset);
    if (activityState.hitOnly) qs.set('hit_only', '1');

    // chart
    try {
      const buckets = activityState.hours === '1' ? 12 : activityState.hours === '24' ? 48 : activityState.hours === '168' ? 56 : 60;
      const tq = new URLSearchParams();
      tq.set('hours', activityState.hours || '24');
      tq.set('buckets', buckets);
      const tres = await fetch(`/api/timeline?${tq.toString()}`);
      if (tres.ok) {
        const td = await tres.json();
        drawTimelineInto('#activity-chart', td);
      }
    } catch (_) {}

    // events
    try {
      const res = await fetch(`/api/events?${qs.toString()}`);
      if (!res.ok) return;
      const data = await res.json();
      activityState.total = data.total ?? 0;
      $('#activity-count').textContent = fmtInt(activityState.total);
      const list = $('#activity-list');
      list.innerHTML = '';
      const rows = (data.events || data.rows || []).filter((e) => {
        if (!activityState.filter) return true;
        const f = activityState.filter.toLowerCase();
        const skill = ((e.chosen && e.chosen[0]) || '').toLowerCase();
        const prompt = (e.user_prompt || '').toLowerCase();
        const model = (e.model || '').toLowerCase();
        return skill.includes(f) || prompt.includes(f) || model.includes(f);
      });
      if (rows.length === 0) {
        list.innerHTML = '<div class="stream-empty muted">这个区间没有事件</div>';
      } else {
        for (const e of rows) {
          const row = document.createElement('div');
          row.className = 'recent-row';
          row.dataset.id = e.id ?? '';
          const chosenArr = Array.isArray(e.chosen) ? e.chosen : [];
          const modeChar = (e.mode || '').toLowerCase().startsWith('e') ? 'e' : 'c';
          const modeText = (e.mode || '').toLowerCase();
          const okErr = e.status === 'ok' ? 'ok' : 'err';
          const okText = e.status === 'ok' ? 'ok' : escapeHTML(e.status || 'err');
          const promptShort = e.user_prompt ? e.user_prompt.slice(0, 140) : '';
          const skillChips = chosenArr.length
            ? chosenArr.map((s) => `<span class="skill-chip">${escapeHTML(s)}</span>`).join('')
            : '<span class="nm-empty">(no skill)</span>';
          const promptLine = promptShort
            ? `<div class="prompt-preview">${escapeHTML(promptShort)}</div>`
            : '';
          const modelText = e.model ? escapeHTML(e.model) : '—';
          row.innerHTML = `
            <div class="ts">${fmtTime(e.ts)}</div>
            <div class="nm">
              <div class="skill-chips">${skillChips}</div>
              ${promptLine}
            </div>
            <div class="mode ${modeChar}">${escapeHTML(modeText || '—')}</div>
            <div class="model">${modelText}</div>
            <div class="dur">${escapeHTML(fmtMsDur(e.latency_ms))}</div>
            <div class="tok">${fmtTok(e.prompt_tokens)} tok</div>
            <div class="st ${okErr}">${okText}</div>
          `;
          row.addEventListener('click', () => openDetail(e.id));
          list.appendChild(row);
        }
      }
      const start = activityState.offset + 1;
      const end = Math.min(activityState.offset + activityState.limit, activityState.total);
      $('#activity-page').textContent = `${start} - ${end} / ${fmtInt(activityState.total)}`;
    } catch (_) {}
  }

  // 通用 timeline 画图 helper — 兼容 {points:[]} / {buckets:[]} / {rows:[]} / array
  function drawTimelineInto(svgSel, td) {
    const svg = document.querySelector(svgSel);
    if (!svg) return;
    const arr = td.points || td.buckets || td.rows || (Array.isArray(td) ? td : []);
    if (!arr.length) return;
    const W = 800, H = 240;
    const max = Math.max(1, ...arr.map((b) => (b.total ?? b.count ?? 0)));
    const pts = arr.map((b, i) => {
      const x = (i / (arr.length - 1 || 1)) * W;
      const v = b.total ?? b.count ?? 0;
      const y = H - (v / max) * (H - 30) - 10;
      return [x, y];
    });
    const pathStroke = pts.map((p, i) => (i === 0 ? `M${p[0]},${p[1]}` : `L${p[0]},${p[1]}`)).join(' ');
    const pathFill = `${pathStroke} L${W},${H} L0,${H} Z`;
    svg.querySelector('.area-stroke').setAttribute('d', pathStroke);
    svg.querySelector('.area-fill').setAttribute('d', pathFill);

    // axis 三个标签写到 SVG 兄弟 div 里，不挤进 SVG，避免跟图表线重叠
    const wrap = svg.parentElement;
    const axStart = wrap && wrap.querySelector('.ax-start');
    const axMid = wrap && wrap.querySelector('.ax-mid');
    const axEnd = wrap && wrap.querySelector('.ax-end');
    if (axStart && axMid && axEnd) {
      const fmtBucket = (b) => {
        if (!b) return '';
        const d = new Date((b.ts_start ?? b.ts ?? b.t ?? 0) * 1000);
        return `${d.getMonth() + 1}-${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
      };
      axStart.textContent = fmtBucket(arr[0]);
      axMid.textContent = fmtBucket(arr[Math.floor(arr.length / 2)]);
      axEnd.textContent = fmtBucket(arr[arr.length - 1]);
    }
  }

  // ------------------------------------------------------------------
  //  Event detail dialog
  // ------------------------------------------------------------------
  async function openDetail(id) {
    if (id == null) return;
    const res = await fetch(`/api/event/${id}`);
    if (!res.ok) return;
    const e = await res.json();
    $('#detail-id').textContent = `#${e.id}`;
    const body = $('#detail-body');
    const chosenInline = e.chosen && e.chosen.length
      ? e.chosen.map((s) => `<a class="chip" href="#/skill/${encodeURIComponent(s)}">${escapeHTML(s)}</a>`).join('')
      : '<span class="chip-empty">空集</span>';
    const statusKlass = e.status === 'ok' ? '' : 'status-error';
    const injectedBadge = e.injected
      ? `<span class="chip">已注入</span>`
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
      <div class="prompt-block">${e.llm_input ? escapeHTML(e.llm_input) : '<span class="dim">(legacy row · schema v13 之后的事件才有)</span>'}</div>
      <div class="section-label">router LLM 原始返回</div>
      <div class="prompt-block">${e.llm_raw_response ? escapeHTML(e.llm_raw_response) : '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">hook 注入给 Claude Code 的内容</div>
      <div class="prompt-block">${e.hook_output ? escapeHTML(e.hook_output) : '<span class="dim">(本次没有注入)</span>'}</div>
    `;
    $('#detail-dialog').showModal();
    document.body.classList.add('dialog-open');
  }

  // dialog 关闭时摘掉 body.dialog-open，让 custom cursor 恢复
  document.getElementById('detail-dialog')
    ?.addEventListener('close', () => document.body.classList.remove('dialog-open'));

  // ------------------------------------------------------------------
  //  Library: skills list
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

    const body = $('#skill-rows');
    body.innerHTML = '';
    $('#skills-empty').hidden = rows.length !== 0;
    rows.forEach((s, idx) => {
      const div = document.createElement('div');
      div.className = 'row';
      // v15: tag the row with the canonical skill name so scope filter +
      // select-mode bulk operations can identify the row without scraping
      // the inner text. Also marks rows that are currently in the user's
      // library so the indicator column renders correctly.
      div.dataset.skill = s.name;
      const inLib = account.libraryNames.has(s.name);
      if (inLib) div.classList.add('in-library');
      if (account.bulkSelect.has(s.name)) div.classList.add('selected');
      const llm = s.llm_score == null
        ? `<div class="llm unknown">—</div>`
        : `<div class="llm">${s.llm_score}</div>`;
      const desc = s.description ? `<small>${escapeHTML(s.description.slice(0, 140))}</small>` : '';
      // Phase F: owner badge — public pool vs private (server.rs omits the
      // owner_user_id field for public rows via serde(skip_serializing_if)).
      const ownerBadge = s.owner_user_id
        ? `<span class="owner-badge owner-private" title="私有 owner=${escapeHTML(s.owner_user_id)}">私有</span>`
        : `<span class="owner-badge owner-public" title="公共池 skill">公共</span>`;
      div.innerHTML = `
        <div class="idx">${String(idx + 1).padStart(2, '0')}</div>
        <div class="nm">${escapeHTML(s.name)} ${ownerBadge}${desc}</div>
        <div class="used">${s.usage_count || 0}</div>
        <div class="last">—</div>
        ${llm}
        <div class="lib-cell">${inLib ? '<span class="lib-on">●</span>' : '<span>○</span>'}</div>
      `;
      div.addEventListener('click', (ev) => {
        // In select mode, clicking toggles bulk selection instead of
        // opening the skill detail page.
        const container = document.getElementById('skill-rows');
        if (container?.classList.contains('select-mode')) {
          ev.stopPropagation();
          ev.preventDefault();
          const name = div.dataset.skill;
          if (account.bulkSelect.has(name)) {
            account.bulkSelect.delete(name);
            div.classList.remove('selected');
          } else {
            account.bulkSelect.add(name);
            div.classList.add('selected');
          }
          // Re-evaluate bulk-button visibility — in scope=all the
          // "加入 / 移出" buttons depend on what's selected (in-library
          // vs not-in-library), so re-render the scope bar on every
          // selection change.
          renderScopeBar();
          return;
        }
        location.hash = `#/skill/${encodeURIComponent(s.name)}`;
      });
      body.appendChild(div);
    });
    // After populating the rows, apply the current scope filter so the
    // user's "my library / public / all" selection takes effect on every
    // (re)render, not just on scope-button click.
    if (typeof renderSkills === 'function') renderSkills();
  }

  async function loadSkills() {
    const res = await fetch('/api/skills');
    if (!res.ok) return;
    const data = await res.json();
    skillsState.cache = data.skills || [];
    $('#library-count').textContent = `${data.total} installed`;
    $('#lib-sub-installed-count').textContent = data.total;
    $('#hero-strip-skills').textContent = data.total;
    $('#hero-strip-skills-delta').textContent = `${data.enriched} enriched`;
    renderSkillsRows();
  }

  // ------------------------------------------------------------------
  //  Library: skill detail
  // ------------------------------------------------------------------
  async function loadSkillDetail(name) {
    detailState.name = name;
    $('#detail-name').textContent = name;
    $('#detail-desc').textContent = '';
    $('#detail-meta-path').textContent = '—';
    $('#detail-strip-used').textContent = '—';
    $('#detail-strip-llm').textContent = '—';
    $('#detail-strip-lat').innerHTML = '—<em class="ms-tail">ms</em>';
    $('#detail-strip-used-delta').textContent = '';
    $('#detail-strip-lat-delta').textContent = '';
    $('#detail-summary').textContent = '';
    $('#detail-files-helper').textContent = '—';
    $('#detail-events-helper').textContent = '—';
    $('#detail-events-body').innerHTML = '';
    $('#detail-events-empty').hidden = true;
    $('#detail-file-tree').innerHTML = '';
    $('#detail-file-body').textContent = '';
    $('#detail-file-path').textContent = '—';
    $('#detail-file-meta').textContent = '';

    const res = await fetch(`/api/skill/${encodeURIComponent(name)}`);
    if (!res.ok) {
      $('#detail-name').textContent = '加载失败';
      $('#detail-desc').textContent = `找不到 skill: ${name}`;
      return;
    }
    const d = await res.json();
    detailState.current = d;

    $('#detail-name').textContent = d.name;
    $('#detail-desc').textContent = d.description || '(no description)';
    $('#detail-meta-path').textContent = d.skill_md_path || '—';
    $('#detail-strip-used').textContent = d.usage_count;
    $('#detail-strip-llm').textContent = d.llm_score == null ? '—' : d.llm_score;

    // Average latency from the embedded events array (no per-skill latency
    // endpoint exists; aggregate client-side).
    const events = d.events || [];
    if (events.length > 0) {
      const lats = events.map((e) => e.latency_ms).filter((v) => v != null);
      if (lats.length > 0) {
        const avg = lats.reduce((a, b) => a + b, 0) / lats.length;
        $('#detail-strip-lat').innerHTML = `${fmtMs(avg)}<em class="ms-tail">ms</em>`;
        const sorted = lats.slice().sort((a, b) => a - b);
        const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? sorted[sorted.length - 1];
        $('#detail-strip-lat-delta').textContent = `p95 ${fmtMs(p95)}ms`;
      }
      $('#detail-strip-used-delta').textContent = `${events.length} 次有记录`;
    } else {
      $('#detail-strip-lat').innerHTML = '—<em class="ms-tail">ms</em>';
    }

    // AI summary section: hide entirely when empty.
    const summarySection = $('#detail-summary-section');
    if (d.summary) {
      $('#detail-summary').textContent = d.summary;
      summarySection.hidden = false;
    } else {
      $('#detail-summary').textContent = '(尚未富集 — 跑 `runai recommend enrich` 生成)';
      summarySection.hidden = false;
    }

    // Event history (grid-based rows, scrollable with edge blur)
    const tbody = $('#detail-events-body');
    tbody.innerHTML = '';
    $('#detail-events-helper').textContent = events.length
      ? (events.length >= (d.events_total ?? events.length)
          ? `${events.length} 条`
          : `${events.length} / 共 ${d.events_total}`)
      : '—';
    $('#detail-events-empty').hidden = events.length !== 0;
    for (const e of events) {
      const row = document.createElement('div');
      row.className = 'er-row';
      row.dataset.id = e.id ?? '';
      const okErr = e.status === 'ok' ? 'st-ok' : 'st-err';
      const okText = e.status === 'ok' ? 'ok' : escapeHTML(e.status || 'err');
      const modeText = (e.mode || '').toLowerCase();
      const modeChar = modeText.startsWith('e') ? 'e' : 'c';
      const promptShort = e.user_prompt ? e.user_prompt.slice(0, 80) : '';
      row.innerHTML = `
        <div class="er-ts">${fmtTs(e.ts)}</div>
        <div class="er-mode ${modeChar}">${escapeHTML(modeText || '—')}</div>
        <div class="er-dur">${fmtMs(e.latency_ms)}<em class="ms-tail">ms</em></div>
        <div class="er-tok">${fmtTok(e.prompt_tokens)} → ${fmtTok(e.completion_tokens)}</div>
        <div class="er-prompt" title="${escapeHTML(e.user_prompt || '')}">${escapeHTML(promptShort) || '<span class="muted">—</span>'}</div>
        <div class="er-st ${okErr}">${okText}</div>
      `;
      row.addEventListener('click', () => openDetail(e.id));
      tbody.appendChild(row);
    }

    await loadFileTree(name);
  }

  async function loadFileTree(name) {
    const tree = $('#detail-file-tree');
    const res = await fetch(`/api/skill/${encodeURIComponent(name)}/files`);
    if (!res.ok) {
      tree.innerHTML = '<div class="muted" style="padding:8px">无法读取 skill 目录</div>';
      $('#detail-file-body').textContent = '';
      return;
    }
    const data = await res.json();
    detailState.files = data;
    const entries = data.entries || [];
    $('#detail-files-helper').textContent =
      entries.length ? `${entries.length} 个 · ${data.skill_dir}` : '空目录';
    tree.innerHTML = '';
    if (entries.length === 0) {
      tree.innerHTML = '<div class="muted" style="padding:8px">空目录</div>';
      $('#detail-file-body').textContent = '';
      return;
    }
    for (const entry of entries) {
      const div = document.createElement('div');
      div.className = 'ftree-entry' + (entry.is_text ? '' : ' binary');
      div.dataset.path = entry.path;
      div.innerHTML = `
        <span class="ftree-name">${escapeHTML(entry.path)}</span>
        <span class="ftree-size">${fmtBytes(entry.size)}</span>
      `;
      div.addEventListener('click', () => selectFile(name, entry.path));
      tree.appendChild(div);
    }
    const preferred =
      entries.find((e) => e.path === 'SKILL.md') ||
      entries.find((e) => e.is_text) ||
      entries[0];
    if (preferred) selectFile(name, preferred.path);
  }

  async function selectFile(name, path) {
    detailState.activeFile = path;
    $$('#detail-file-tree .ftree-entry').forEach((el) => {
      el.classList.toggle('active', el.dataset.path === path);
    });
    $('#detail-file-path').textContent = path;
    $('#detail-file-body').textContent = '加载中...';
    const url = `/api/skill/${encodeURIComponent(name)}/file?path=${encodeURIComponent(path)}`;
    const res = await fetch(url);
    if (!res.ok) {
      $('#detail-file-body').textContent = '(读取失败)';
      $('#detail-file-meta').textContent = '';
      return;
    }
    const f = await res.json();
    $('#detail-file-meta').textContent =
      `${fmtBytes(f.size)}${f.truncated ? ' · truncated' : ''}${f.is_text ? '' : ' · 二进制'}`;
    if (f.is_text) {
      $('#detail-file-body').textContent = f.content || '(空文件)';
    } else {
      $('#detail-file-body').textContent = `(二进制文件 — ${fmtBytes(f.size)} — 不显示内容)`;
    }
  }

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

  // ------------------------------------------------------------------
  //  Custom dropdown wiring (mockup-style; no native <select>)
  // ------------------------------------------------------------------
  function initDropdown(dd, onChange) {
    const trigger = dd.querySelector('.dropdown-trigger');
    const menu = dd.querySelector('.dropdown-menu');
    const labelEl = trigger.querySelector('.dd-label');

    trigger.addEventListener('click', (e) => {
      e.stopPropagation();
      const wasOpen = dd.classList.contains('open');
      document.querySelectorAll('.dropdown.open').forEach((x) => {
        if (x !== dd) x.classList.remove('open');
      });
      dd.classList.toggle('open', !wasOpen);
    });

    menu.querySelectorAll('.dd-opt').forEach((opt) => {
      opt.addEventListener('click', (e) => {
        e.stopPropagation();
        menu.querySelectorAll('.dd-opt').forEach((x) => x.classList.remove('active'));
        opt.classList.add('active');
        if (labelEl) labelEl.textContent = opt.textContent.trim();
        dd.classList.remove('open');
        if (onChange) onChange(opt.dataset.value);
      });
    });
  }

  // ------------------------------------------------------------------
  //  Theme swatch picker
  // ------------------------------------------------------------------
  function initSwatches() {
    document.querySelectorAll('.swatch').forEach((s) => {
      s.addEventListener('click', () => {
        const t = s.getAttribute('data-theme');
        document.body.className = document.body.className
          .replace(/\btheme-\w+\b/g, '').trim();
        document.body.classList.add('theme-' + t);
        document.querySelectorAll('.swatch').forEach((x) => x.classList.remove('active'));
        s.classList.add('active');
        try { localStorage.setItem('runai.theme', t); } catch (_e) { /* ignore */ }
      });
    });
    // restore saved theme
    try {
      const saved = localStorage.getItem('runai.theme');
      if (saved && /^[a-z]+$/.test(saved)) {
        const s = document.querySelector(`.swatch[data-theme="${saved}"]`);
        if (s) s.click();
      }
    } catch (_e) { /* ignore */ }
  }

  // ------------------------------------------------------------------
  //  Custom cursor (SVG arrow + lagging ring + contextual label)
  // ------------------------------------------------------------------
  function initCustomCursor() {
    if (!window.matchMedia || !window.matchMedia('(pointer:fine)').matches) return;

    const glow  = document.querySelector('.cursor-glow');
    const arrow = document.querySelector('.cursor-arrow');
    const ring  = document.querySelector('.cursor-ring');
    const label = document.querySelector('.cursor-label');
    if (!arrow || !ring || !label) return;

    let mx = window.innerWidth / 2, my = window.innerHeight / 2;
    let rx = mx, ry = my;
    let lx = mx, ly = my;

    function frame() {
      arrow.style.transform = `translate(${mx - 2}px,${my - 2}px)`;
      rx += (mx - rx) * 0.18;
      ry += (my - ry) * 0.18;
      ring.style.transform = `translate(${rx - ring.offsetWidth / 2}px,${ry - ring.offsetHeight / 2}px)`;
      lx += (mx - lx) * 0.32;
      ly += (my - ly) * 0.32;
      label.style.transform = `translate(${lx + 18}px,${ly + 18}px)`;
      if (glow) {
        glow.style.setProperty('--mx', mx + 'px');
        glow.style.setProperty('--my', my + 'px');
      }
      requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);

    document.addEventListener('mousemove', (e) => {
      mx = e.clientX; my = e.clientY;
      if (arrow.classList.contains('hide')) {
        arrow.classList.remove('hide');
        ring.classList.remove('hide');
      }
    }, { passive: true });

    document.addEventListener('mouseleave', () => {
      arrow.classList.add('hide');
      ring.classList.add('hide');
      label.classList.remove('visible');
    });
    document.addEventListener('mouseenter', () => {
      arrow.classList.remove('hide');
      ring.classList.remove('hide');
    });

    function showLabel(text, accent) {
      label.textContent = text;
      label.classList.toggle('accent', !!accent);
      label.classList.remove('hide');
      label.classList.add('visible');
    }
    function hideLabel() { label.classList.remove('visible'); }

    let currentIntent = null;
    function applyIntent(name) {
      if (currentIntent) {
        arrow.classList.remove('intent-' + currentIntent);
        ring.classList.remove('intent-' + currentIntent);
      }
      if (name) {
        arrow.classList.add('intent-' + name);
        ring.classList.add('intent-' + name);
      }
      currentIntent = name;
    }

    // [selector, label-text, intent-class, label-accent-style]
    const bindings = [
      ['.skill-rows .row',           'open skill', 'deep',    true],
      ['.recent-row',                'view exec',  'deep',    false],
      ['.stream-item',               'view exec',  'deep',    false],
      ['.events-rows .er-row',       'view exec',  'deep',    false],
      ['.dd-opt',                    'select',     'primary', true],
      ['.dropdown-trigger',          'open',       'default', false],
      ['.lib-sub',                   'switch',     'default', false],
      ['.back-link',                 'back',       'default', false],
      ['.btn',                       'tap',        'default', false],
      ['.swatch',                    'preview',    'theme',   false],
      ['.nav-item',                  'navigate',   'default', false],
      ['.skill-rows .row .toggle',   'toggle',     'toggle',  false],
      ['.ftree-entry',               'open file',  'default', false],
      ['a.chip',                     'open',       'default', false],
      ['a:not(.back-link):not(.lib-sub):not(.nav-item):not(.chip)', 'open', 'default', false],
    ];

    // Re-bind every time DOM mutates (because we render rows dynamically).
    function bind() {
      bindings.forEach((pair) => {
        const [sel, text, intent, accent] = pair;
        document.querySelectorAll(sel).forEach((el) => {
          if (el.dataset.cursorBound === '1') return;
          el.dataset.cursorBound = '1';
          let hoverIntent = intent;
          if (el.classList.contains('off') || el.classList.contains('disabled') ||
              el.closest('[disabled]') || el.getAttribute('aria-disabled') === 'true') {
            hoverIntent = 'disabled';
          }
          el.addEventListener('mouseenter', () => {
            if (intent === 'theme') {
              const c = getComputedStyle(el).backgroundColor;
              arrow.style.setProperty('--swatch-c', c);
              ring.style.setProperty('--swatch-c', c);
            }
            applyIntent(hoverIntent);
            showLabel(text, accent);
          });
          el.addEventListener('mouseleave', () => {
            if (intent === 'theme') {
              arrow.style.removeProperty('--swatch-c');
              ring.style.removeProperty('--swatch-c');
            }
            applyIntent(null);
            hideLabel();
          });
        });
      });

      document.querySelectorAll('input,textarea,[contenteditable]').forEach((el) => {
        if (el.dataset.cursorTextBound === '1') return;
        el.dataset.cursorTextBound = '1';
        el.addEventListener('mouseenter', () => {
          document.body.classList.add('text-cursor');
          applyIntent(null);
          hideLabel();
        });
        el.addEventListener('mouseleave', () => {
          document.body.classList.remove('text-cursor');
        });
      });
    }

    bind();
    // Re-bind when DOM changes (new rows etc).
    const mo = new MutationObserver(() => bind());
    mo.observe(document.body, { childList: true, subtree: true });

    document.addEventListener('mousedown', () => { ring.classList.add('click'); });
    document.addEventListener('mouseup',   () => { ring.classList.remove('click'); });
  }

  // ------------------------------------------------------------------
  //  Wiring
  // ------------------------------------------------------------------
  function bindControls() {
    // Skill filter input
    const filterEl = $('#skill-filter');
    if (filterEl) {
      filterEl.addEventListener('input', (e) => {
        skillsState.filter = e.target.value;
        renderSkillsRows();
      });
    }

    // Sort dropdown
    const sortDd = $('#skill-sort-dd');
    if (sortDd) {
      initDropdown(sortDd, (val) => {
        skillsState.sort = val;
        renderSkillsRows();
      });
    }

    // Event detail dialog close
    const closeBtn = $('#detail-close');
    if (closeBtn) closeBtn.addEventListener('click', () => $('#detail-dialog').close());

    // Activity tab controls
    const af = $('#activity-filter');
    if (af) {
      af.addEventListener('input', (e) => {
        activityState.filter = e.target.value;
        loadActivity();
      });
    }
    const aw = $('#activity-window');
    if (aw) initDropdown(aw, (val) => { activityState.hours = val; activityState.offset = 0; loadActivity(); });
    const ah = $('#activity-hit');
    if (ah) initDropdown(ah, (val) => { activityState.hitOnly = val; activityState.offset = 0; loadActivity(); });
    const ap = $('#activity-prev');
    if (ap) ap.addEventListener('click', () => {
      activityState.offset = Math.max(0, activityState.offset - activityState.limit);
      loadActivity();
    });
    const an = $('#activity-next');
    if (an) an.addEventListener('click', () => {
      if (activityState.offset + activityState.limit < activityState.total) {
        activityState.offset += activityState.limit;
        loadActivity();
      }
    });

    // Global dropdown close
    document.addEventListener('click', () => {
      document.querySelectorAll('.dropdown.open').forEach((x) => x.classList.remove('open'));
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        document.querySelectorAll('.dropdown.open').forEach((x) => x.classList.remove('open'));
      }
    });

    // Keyboard shortcuts (skip when typing)
    document.addEventListener('keydown', (e) => {
      const tag = (e.target && e.target.tagName) || '';
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      if (e.key === 'o') { location.hash = '#/'; }
      else if (e.key === 'l') { location.hash = '#/skills'; }
    });
  }

  // ------------------------------------------------------------------
  //  Settings tab — recommend config + providers CRUD
  // ------------------------------------------------------------------
  let lastSettings = null;
  let providerEditingId = null;       // null = adding new; "<id>" = editing
  let provKindValue = 'openai-compat';
  let provKindDdInitialized = false;

  function setProviderKindDropdown(value) {
    provKindValue = value;
    const dd = document.getElementById('prov-kind-dd');
    if (!dd) return;
    const label = dd.querySelector('.dd-label');
    dd.querySelectorAll('.dd-opt').forEach((opt) => {
      const isActive = opt.dataset.value === value;
      opt.classList.toggle('active', isActive);
      if (isActive && label) label.textContent = opt.textContent.trim();
    });
  }

  function initProviderKindDropdown() {
    if (provKindDdInitialized) return;
    const dd = document.getElementById('prov-kind-dd');
    if (!dd) return;
    initDropdown(dd, (val) => { provKindValue = val; });
    provKindDdInitialized = true;
  }

  async function loadSettings() {
    try {
      const res = await fetch('/api/settings');
      if (!res.ok) return;
      const data = await res.json();
      lastSettings = data;
      renderSettings(data);
    } catch (_) {}
  }

  function renderSettings(data) {
    $('#set-enabled').checked = !!data.enabled;
    $('#set-read-claude-md').checked = !!data.read_claude_md;
    $('#set-skip-reminder').checked = !!data.skip_reminder_enabled;
    $('#set-skip-reminder-template').value = data.skip_reminder_template || '';
    $('#set-skip-reminder-template').disabled = !data.skip_reminder_enabled;
    const activeLabel = (data.providers || [])
      .find((p) => p.id === data.active_provider_id);
    $('#set-active-label').textContent = activeLabel
      ? `${activeLabel.label} · ${activeLabel.model}`
      : '(未选择)';
    renderProvidersList(data);
  }

  function renderProvidersList(data) {
    const wrap = $('#providers-list');
    wrap.innerHTML = '';
    if (!data.providers || data.providers.length === 0) {
      wrap.innerHTML = '<div class="muted provider-empty">还没添加运营商 — 点下方按钮加一个</div>';
      return;
    }
    for (const p of data.providers) {
      const row = document.createElement('div');
      row.className = 'provider-row' + (p.id === data.active_provider_id ? ' active' : '');
      row.innerHTML = `
        <div class="prov-pick">
          <label class="radio">
            <input type="radio" name="active-provider" value="${escapeHTML(p.id)}"
                   ${p.id === data.active_provider_id ? 'checked' : ''}>
            <span></span>
          </label>
        </div>
        <div class="prov-id-col">
          <div class="prov-label">${escapeHTML(p.label || p.id)}</div>
          <div class="prov-meta"><span class="kind">${escapeHTML(p.kind)}</span> · ${escapeHTML(p.model || '')}</div>
          <div class="prov-base">${escapeHTML(p.base_url || '')}</div>
        </div>
        <div class="prov-keystate">
          ${p.has_api_key ? '<span class="key-set">key 已配置</span>' : '<span class="key-empty">无 key</span>'}
        </div>
        <div class="prov-actions">
          <button type="button" class="btn prov-edit" data-id="${escapeHTML(p.id)}">编辑</button>
          <button type="button" class="btn prov-del" data-id="${escapeHTML(p.id)}">删除</button>
        </div>
      `;
      wrap.appendChild(row);
    }
    wrap.querySelectorAll('input[name="active-provider"]').forEach((el) => {
      el.addEventListener('change', () => activateProvider(el.value));
    });
    wrap.querySelectorAll('.prov-edit').forEach((b) => {
      b.addEventListener('click', () => openProviderForm(b.dataset.id));
    });
    wrap.querySelectorAll('.prov-del').forEach((b) => {
      b.addEventListener('click', () => deleteProvider(b.dataset.id));
    });
  }

  async function patchSettings(patch) {
    try {
      const res = await fetch('/api/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(patch),
      });
      if (!res.ok) return;
      const data = await res.json();
      lastSettings = data;
      renderSettings(data);
    } catch (_) {}
  }

  async function activateProvider(id) {
    try {
      const res = await fetch(`/api/providers/${encodeURIComponent(id)}/activate`, { method: 'POST' });
      if (!res.ok) return;
      const data = await res.json();
      lastSettings = data;
      renderSettings(data);
    } catch (_) {}
  }

  async function deleteProvider(id) {
    if (!confirm(`删除运营商 "${id}" ？此操作只清除 runai 配置，不影响远程账号。`)) return;
    try {
      const res = await fetch(`/api/providers/${encodeURIComponent(id)}`, { method: 'DELETE' });
      if (!res.ok) return;
      const data = await res.json();
      lastSettings = data;
      renderSettings(data);
    } catch (_) {}
  }

  function openProviderForm(id) {
    providerEditingId = id || null;
    initProviderKindDropdown();
    const form = $('#provider-form');
    const title = $('#provider-form-title');
    if (id && lastSettings) {
      const p = (lastSettings.providers || []).find((x) => x.id === id);
      if (!p) return;
      title.textContent = `编辑 ${p.label || p.id}`;
      $('#prov-id').value = p.id;
      $('#prov-id').disabled = true;
      $('#prov-label').value = p.label || '';
      setProviderKindDropdown(p.kind || 'openai-compat');
      $('#prov-model').value = p.model || '';
      $('#prov-base-url').value = p.base_url || '';
      $('#prov-api-key').value = '';
      $('#prov-api-key').placeholder = p.has_api_key
        ? 'sk-... (留空保留已存的 key)'
        : 'sk-...';
    } else {
      title.textContent = '添加运营商';
      $('#prov-id').value = '';
      $('#prov-id').disabled = false;
      $('#prov-label').value = '';
      setProviderKindDropdown('openai-compat');
      $('#prov-model').value = '';
      $('#prov-base-url').value = '';
      $('#prov-api-key').value = '';
      $('#prov-api-key').placeholder = 'sk-...';
    }
    form.hidden = false;
    form.scrollIntoView({ behavior: 'smooth', block: 'center' });
  }

  function closeProviderForm() {
    $('#provider-form').hidden = true;
    providerEditingId = null;
  }

  async function submitProviderForm() {
    const id = $('#prov-id').value.trim();
    if (!id) { alert('运营商 ID 必填'); return; }
    const body = {
      id,
      label: $('#prov-label').value.trim() || id,
      kind: provKindValue || 'openai-compat',
      model: $('#prov-model').value.trim(),
      base_url: $('#prov-base-url').value.trim(),
      api_key: $('#prov-api-key').value,    // empty = preserve existing
    };
    try {
      const res = await fetch('/api/providers', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        const err = await res.json().catch(() => ({}));
        alert(`保存失败：${err.error || res.status}`);
        return;
      }
      const data = await res.json();
      lastSettings = data;
      renderSettings(data);
      closeProviderForm();
    } catch (e) {
      alert(`保存失败：${e}`);
    }
  }

  function bindSettingsControls() {
    // v15 multi-user: the four hook-behavior toggles + textarea moved
    // from /api/settings (global, admin-only) to /api/prefs (per-user).
    // Wire them through savePrefs() instead of patchSettings().
    const wirePref = (id, key, kind) => {
      const el = document.getElementById(id);
      if (!el) return;
      const event = kind === 'textarea' ? 'blur' : 'change';
      el.addEventListener(event, () => {
        if (!account.me || !account.prefs) return;
        const v = kind === 'checkbox' ? el.checked : el.value;
        account.prefs[key] = v;
        savePrefs();
      });
    };
    wirePref('set-enabled', 'recommend_enabled', 'checkbox');
    wirePref('set-read-claude-md', 'read_claude_md', 'checkbox');
    wirePref('set-skip-reminder', 'skip_reminder_enabled', 'checkbox');
    wirePref('set-skip-reminder-template', 'skip_reminder_template', 'textarea');
    const addBtn = document.getElementById('provider-add-btn');
    if (addBtn) addBtn.addEventListener('click', () => openProviderForm(null));
    const saveBtn = document.getElementById('provider-save-btn');
    if (saveBtn) saveBtn.addEventListener('click', submitProviderForm);
    const cancelBtn = document.getElementById('provider-cancel-btn');
    if (cancelBtn) cancelBtn.addEventListener('click', closeProviderForm);
  }

  // ------------------------------------------------------------------
  //  v15 multi-user: account / library / per-user prefs
  // ------------------------------------------------------------------
  // In-memory mirror of /api/me + /api/skills/library, refreshed on
  // login, logout, mutate, and dashboard boot. Powers the account pill,
  // Settings 我的偏好 toggle, and the skills tab scope filter.
  const account = {
    me: null,             // { user_id, username, is_admin, library_size } or null
    libraryNames: new Set(), // names currently in user's library
    scope: 'all',         // 'all' | 'my' | 'public'
    bulkSelect: new Set(),
    prefs: null,          // UserPrefs from /api/prefs
  };

  // localStorage-backed api_key fallback. Cookies expire / get cleared
  // by browser hygiene tools; the bearer survives across browser
  // restarts so the dashboard stays signed in.
  const RUNAI_KEY_STORE = 'runai_api_key';
  function getStoredApiKey() {
    try { return localStorage.getItem(RUNAI_KEY_STORE) || ''; } catch (_) { return ''; }
  }
  function setStoredApiKey(key) {
    try {
      if (key) localStorage.setItem(RUNAI_KEY_STORE, key);
      else localStorage.removeItem(RUNAI_KEY_STORE);
    } catch (_) {}
  }

  async function api(method, path, body) {
    const headers = {};
    const key = getStoredApiKey();
    if (key) headers['Authorization'] = `Bearer ${key}`;
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    const opts = { method, credentials: 'same-origin', headers };
    if (body !== undefined) opts.body = JSON.stringify(body);
    const res = await fetch(path, opts);
    if (!res.ok) {
      let detail = '';
      try { detail = (await res.json()).error || ''; } catch (_) {}
      const err = new Error(detail || `HTTP ${res.status}`);
      err.status = res.status;
      throw err;
    }
    if (res.status === 204) return null;
    const ct = res.headers.get('content-type') || '';
    return ct.includes('json') ? res.json() : res.text();
  }

  async function refreshMe() {
    try {
      account.me = await api('GET', '/api/me');
    } catch (e) {
      account.me = null;
    }
    renderAccountPill();
    await refreshLibraryNames();
    await refreshPrefs();
    renderSettingsUser();
    renderScopeBar();
    // re-render rows so the in-library "●" indicator + selection state
    // reflect the freshly-loaded library set.
    if (typeof renderSkillsRows === 'function' && skillsState.cache.length) {
      renderSkillsRows();
    }
    renderSkills();
  }

  async function refreshLibraryNames() {
    account.libraryNames.clear();
    if (!account.me) return;
    try {
      const data = await api('GET', '/api/skills/library');
      for (const e of (data.items || [])) account.libraryNames.add(e.name);
    } catch (_) {}
  }

  async function refreshPrefs() {
    if (!account.me) { account.prefs = null; return; }
    try {
      account.prefs = await api('GET', '/api/prefs');
    } catch (_) { account.prefs = null; }
  }

  function renderAccountPill() {
    const loginBtn = $('#account-login-btn');
    const info = $('#account-info');
    const name = $('#account-name');
    if (!loginBtn || !info || !name) return;
    if (account.me) {
      loginBtn.classList.add('hide');
      info.classList.remove('hide');
      name.textContent = account.me.username + (account.me.is_admin ? ' (admin)' : '');
    } else {
      loginBtn.classList.remove('hide');
      info.classList.add('hide');
      name.textContent = '—';
    }
    // Toggle the admin nav-item + the "前往 Admin" jump button in Settings.
    const adminNav = document.querySelector('.nav-admin');
    if (adminNav) adminNav.classList.toggle('hide', !(account.me && account.me.is_admin));
    const jumpBtn = $('#settings-admin-jump');
    if (jumpBtn) {
      jumpBtn.style.display = (account.me && account.me.is_admin) ? '' : 'none';
    }
  }

  // ------------------------------------------------------------------
  //  v15 admin user-management table (Admin pane)
  // ------------------------------------------------------------------
  async function loadAdminUsers() {
    if (!account.me || !account.me.is_admin) return;
    const rowsEl = $('#admin-users-rows');
    const countEl = $('#admin-users-count');
    if (!rowsEl) return;
    try {
      const data = await api('GET', '/api/admin/users');
      if (countEl) countEl.textContent = data.total;
      rowsEl.innerHTML = '';
      for (const u of data.items) {
        const row = document.createElement('div');
        row.className = 'admin-users-row';
        const isSelf = account.me.user_id === u.user_id;
        const adminBadge = u.is_admin
          ? '<span class="badge admin">ADMIN</span>'
          : '<span class="badge user">USER</span>';
        const statusBadge = u.disabled
          ? '<span class="badge disabled">DISABLED</span>'
          : '<span class="badge active">ACTIVE</span>';
        const created = new Date(u.created_at * 1000)
          .toISOString()
          .replace('T', ' ')
          .slice(0, 16);
        const promoteBtn = u.is_admin
          ? `<button class="btn-small" data-act="demote" data-uid="${u.user_id}" ${isSelf ? 'disabled' : ''}>取消管理员</button>`
          : `<button class="btn-small" data-act="promote" data-uid="${u.user_id}">设为管理员</button>`;
        const toggleDisableBtn = u.disabled
          ? `<button class="btn-small" data-act="enable" data-uid="${u.user_id}">启用</button>`
          : `<button class="btn-small" data-act="disable" data-uid="${u.user_id}" ${isSelf ? 'disabled' : ''}>禁用</button>`;
        const deleteBtn = `<button class="btn-small btn-danger" data-act="delete" data-uid="${u.user_id}" data-uname="${escapeHTML(u.username)}" ${isSelf ? 'disabled' : ''}>删除</button>`;
        row.innerHTML = `
          <div class="uname ${isSelf ? 'self' : ''}">${escapeHTML(u.username)}${isSelf ? ' (你)' : ''}</div>
          <div>${adminBadge}</div>
          <div>${statusBadge}</div>
          <div>${u.library_size}</div>
          <div>${u.event_count}</div>
          <div>${created}</div>
          <div class="actions">${promoteBtn}${toggleDisableBtn}${deleteBtn}</div>
        `;
        rowsEl.appendChild(row);
      }
      rowsEl.querySelectorAll('button[data-act]').forEach((b) => {
        b.addEventListener('click', () => adminUserAction(b.dataset.act, b.dataset.uid, b.dataset.uname));
      });
    } catch (e) {
      rowsEl.innerHTML = `<div class="muted" style="padding:12px">加载用户失败：${escapeHTML(e.message)}</div>`;
    }
  }

  async function adminUserAction(action, userId, username) {
    if (!userId) return;
    try {
      if (action === 'promote' || action === 'demote') {
        await api('POST', `/api/admin/users/${encodeURIComponent(userId)}`, {
          is_admin: action === 'promote',
        });
      } else if (action === 'enable' || action === 'disable') {
        await api('POST', `/api/admin/users/${encodeURIComponent(userId)}`, {
          disabled: action === 'disable',
        });
      } else if (action === 'delete') {
        const ok = await showConfirm({
          title: '删除用户',
          body: `确定删除用户 ${username || userId}？该用户的账号 + 我的库订阅会被一并清除，已写入的 router_events 保留（user_id 不再可追溯到具体用户）。此操作不可撤销。`,
          ok: '永久删除',
          cancel: '取消',
          danger: true,
        });
        if (!ok) return;
        await api('DELETE', `/api/admin/users/${encodeURIComponent(userId)}`);
      } else {
        return;
      }
      await loadAdminUsers();
    } catch (e) {
      alert(`操作失败：${e.message}`);
    }
  }

  function renderSettingsUser() {
    const lbl = $('#settings-me-label');
    if (lbl) lbl.textContent = account.me ? account.me.username : '未登录';

    const loggedIn = !!account.me;
    const p = account.prefs || {};
    // Per-user prefs UI. All disabled until login resolves.
    const setSwitch = (id, key, fallback = false) => {
      const el = $(`#${id}`);
      if (!el) return;
      el.disabled = !loggedIn;
      el.checked = loggedIn ? !!(p[key] ?? fallback) : false;
    };
    const setText = (id, key, fallback = '') => {
      const el = $(`#${id}`);
      if (!el) return;
      el.disabled = !loggedIn;
      el.value = loggedIn ? (p[key] ?? fallback) : '';
    };
    setSwitch('set-allow-public-recommend', 'allow_public_recommend', false);
    setSwitch('set-enabled', 'recommend_enabled', true);
    setSwitch('set-read-claude-md', 'read_claude_md', true);
    setSwitch('set-skip-reminder', 'skip_reminder_enabled', false);
    setText('set-skip-reminder-template', 'skip_reminder_template', '');
  }

  function renderScopeBar() {
    const bar = $('#library-scope-bar');
    const selectGroup = $('#select-mode-group');
    const quick = $('#library-quick-actions');
    if (!bar) return;
    if (account.me) {
      if (selectGroup) selectGroup.classList.remove('hide');
      if (quick) quick.classList.remove('hide');
    } else {
      if (selectGroup) selectGroup.classList.add('hide');
      if (quick) quick.classList.add('hide');
    }

    // Count badges per scope
    const total = skillsState.cache.length;
    const myCount = account.libraryNames.size;
    const publicCount = Math.max(0, total - myCount);
    $('#scope-all-count') && ($('#scope-all-count').textContent = total);
    $('#scope-my-count') && ($('#scope-my-count').textContent = myCount);
    $('#scope-public-count') && ($('#scope-public-count').textContent = publicCount);

    document.querySelectorAll('.scope-btn').forEach((b) => {
      b.classList.toggle('active', b.dataset.scope === account.scope);
    });

    // Bulk-button visibility — only show the action that actually applies
    // to what's currently selected (avoids the "in 全部 scope you can hit
    // 移出我的库 even on skills that aren't in your library" surprise).
    //
    // Rules:
    //   - select mode off              → hide all bulk buttons
    //   - scope=my                     → only "移出我的库"
    //   - scope=public                 → only "加入我的库"
    //   - scope=all + selection empty  → show both (user hasn't chosen yet)
    //   - scope=all + all-in-library   → only "移出我的库"
    //   - scope=all + all-not-in-lib   → only "加入我的库"
    //   - scope=all + mixed selection  → show both, user picks intent
    const container = document.getElementById('skill-rows');
    const inSelectMode = container?.classList.contains('select-mode');
    let showAdd = false;
    let showRemove = false;
    if (inSelectMode) {
      if (account.scope === 'my') {
        showRemove = true;
      } else if (account.scope === 'public') {
        showAdd = true;
      } else {
        // scope=all — look at the current selection composition.
        if (account.bulkSelect.size === 0) {
          showAdd = true;
          showRemove = true;
        } else {
          let anyIn = false;
          let anyOut = false;
          for (const name of account.bulkSelect) {
            if (account.libraryNames.has(name)) anyIn = true;
            else anyOut = true;
            if (anyIn && anyOut) break;
          }
          showAdd = anyOut;
          showRemove = anyIn;
        }
      }
    }
    $('#bulk-add')?.classList.toggle('hide', !showAdd);
    $('#bulk-remove')?.classList.toggle('hide', !showRemove);
    $('#bulk-select-all')?.classList.toggle('hide', !inSelectMode);
    $('#bulk-clear-sel')?.classList.toggle('hide', !inSelectMode);
    $('#bulk-sel-count')?.classList.toggle('hide', !inSelectMode);
    updateBulkCount();
  }

  function toggleSelectMode() {
    if (!account.me) { showAuthModal('login'); return; }
    const container = document.getElementById('skill-rows');
    const btn = $('#select-mode-btn');
    if (!container || !btn) return;
    const on = !container.classList.contains('select-mode');
    container.classList.toggle('select-mode', on);
    btn.classList.toggle('on', on);
    btn.textContent = on ? '退出选中模式' : '进入选中模式';
    if (!on) {
      // Exiting select mode: clear selection so it doesn't linger when
      // user toggles back in expecting a clean slate.
      account.bulkSelect.clear();
      document.querySelectorAll('#skill-rows .row.selected').forEach((r) =>
        r.classList.remove('selected'),
      );
    }
    renderScopeBar();
  }

  function updateBulkCount() {
    const el = $('#bulk-sel-count');
    if (el) el.textContent = `已选 ${account.bulkSelect.size}`;
  }

  // Patch the existing renderSkills to apply scope filter + add an
  // "in library" toggle button per row. We do it via DOM hook on the
  // skill-rows container — non-invasive to renderSkills itself.
  function renderSkills() {
    const rows = document.getElementById('skill-rows');
    if (!rows) return;
    // v15 scope filter — name comes from row.dataset.skill (set by
    // renderSkillsRows). Fall back to ".nm" text scrape on legacy rows.
    for (const row of rows.children) {
      const name = row.dataset.skill
        || row.querySelector('.nm')?.firstChild?.textContent?.trim()
        || '';
      const inLib = account.libraryNames.has(name);
      let show = true;
      if (account.scope === 'my') show = inLib;
      else if (account.scope === 'public') show = !inLib;
      row.style.display = show ? '' : 'none';
    }
  }

  function showAuthModal(mode) {
    const modal = $('#auth-modal');
    const title = $('#auth-title');
    const sub = $('#auth-sub');
    const submit = $('#auth-submit');
    const toggle = $('#auth-toggle');
    const err = $('#auth-err');
    if (!modal) return;
    modal.classList.remove('hide');
    err.classList.add('hide');
    err.textContent = '';
    modal.dataset.mode = mode;
    if (mode === 'register') {
      title.textContent = '注册 runai';
      sub.textContent = '已有账号？切换到登录';
      submit.textContent = '注册';
      toggle.textContent = '已有账号 → 登录';
      $('#auth-password').autocomplete = 'new-password';
    } else {
      title.textContent = '登录 runai';
      sub.textContent = '没有账号？切换到注册';
      submit.textContent = '登录';
      toggle.textContent = '注册新账号';
      $('#auth-password').autocomplete = 'current-password';
    }
    setTimeout(() => $('#auth-username')?.focus(), 30);
  }

  function hideAuthModal() {
    $('#auth-modal')?.classList.add('hide');
  }

  // Generic in-page confirm dialog. Returns a Promise<boolean>. Reuses the
  // auth-modal chrome so the look-and-feel stays consistent across the app
  // instead of flashing the OS-native white window.confirm().
  function showConfirm({ title, body, ok = '确定', cancel = '取消', danger = true } = {}) {
    return new Promise((resolve) => {
      const modal = $('#confirm-modal');
      const okBtn = $('#confirm-ok');
      const cancelBtn = $('#confirm-cancel');
      if (!modal || !okBtn || !cancelBtn) { resolve(window.confirm(body)); return; }
      $('#confirm-title').textContent = title || '确认';
      $('#confirm-body').textContent = body || '';
      okBtn.textContent = ok;
      cancelBtn.textContent = cancel;
      cancelBtn.classList.remove('hide');
      okBtn.classList.toggle('confirm-danger', !!danger);
      modal.classList.remove('hide');
      const cleanup = (val) => {
        modal.classList.add('hide');
        okBtn.removeEventListener('click', onOk);
        cancelBtn.removeEventListener('click', onCancel);
        modal.removeEventListener('click', onBg);
        resolve(val);
      };
      const onOk = () => cleanup(true);
      const onCancel = () => cleanup(false);
      const onBg = (ev) => { if (ev.target === modal) cleanup(false); };
      okBtn.addEventListener('click', onOk);
      cancelBtn.addEventListener('click', onCancel);
      modal.addEventListener('click', onBg);
      setTimeout(() => okBtn.focus(), 30);
    });
  }

  // Info / success modal — same DOM as confirm, hides the cancel button so
  // it looks like a one-action acknowledge dialog. Supports an optional
  // pre-formatted body block (long list of names, etc).
  function showInfo({ title, body, items, ok = '确定', danger = false } = {}) {
    return new Promise((resolve) => {
      const modal = $('#confirm-modal');
      const okBtn = $('#confirm-ok');
      const cancelBtn = $('#confirm-cancel');
      if (!modal || !okBtn || !cancelBtn) { window.alert(body); resolve(); return; }
      $('#confirm-title').textContent = title || '提示';
      const bodyEl = $('#confirm-body');
      if (items && items.length) {
        // Render lines as <li> so long lists scroll inside the card.
        bodyEl.innerHTML = `<div>${escapeHTML(body || '')}</div>
          <ul class="confirm-items">${items.map((x) => `<li>${escapeHTML(x)}</li>`).join('')}</ul>`;
      } else {
        bodyEl.textContent = body || '';
      }
      okBtn.textContent = ok;
      okBtn.classList.toggle('confirm-danger', !!danger);
      cancelBtn.classList.add('hide');
      modal.classList.remove('hide');
      const cleanup = () => {
        modal.classList.add('hide');
        // Restore cancel for future confirm() calls
        cancelBtn.classList.remove('hide');
        bodyEl.textContent = '';
        okBtn.removeEventListener('click', onOk);
        modal.removeEventListener('click', onBg);
        document.removeEventListener('keydown', onKey);
        resolve();
      };
      const onOk = () => cleanup();
      const onBg = (ev) => { if (ev.target === modal) cleanup(); };
      const onKey = (ev) => { if (ev.key === 'Escape' || ev.key === 'Enter') cleanup(); };
      okBtn.addEventListener('click', onOk);
      modal.addEventListener('click', onBg);
      document.addEventListener('keydown', onKey);
      setTimeout(() => okBtn.focus(), 30);
    });
  }

  async function submitAuth(ev) {
    ev.preventDefault();
    const modal = $('#auth-modal');
    const mode = modal?.dataset.mode || 'login';
    const username = $('#auth-username').value.trim();
    const password = $('#auth-password').value;
    const err = $('#auth-err');
    err.classList.add('hide');
    try {
      const path = mode === 'register' ? '/users/register' : '/auth/login';
      const resp = await api('POST', path, { username, password });
      // Persist api_key locally so the dashboard survives cookie wipes
      // / browser restarts. Both /users/register and /auth/login return
      // an api_key in their JSON body.
      if (resp && resp.api_key) setStoredApiKey(resp.api_key);
      hideAuthModal();
      $('#auth-username').value = '';
      $('#auth-password').value = '';
      await refreshMe();
      // Force every pane that has cached "anonymous → 401 → empty"
      // state to re-fetch with the new cookie. Without these the
      // hero numbers stay "—" until the next 5s poll, the Activity
      // list stays empty until tab switch, and the skills cache
      // shows zero usage_count for the user.
      await reloadCurrentView();
    } catch (e) {
      err.textContent = e.message || (mode === 'register' ? '注册失败' : '登录失败');
      err.classList.remove('hide');
    }
  }

  /// Re-run whatever loaders the current route needs. Cheaper than a
  /// full page reload and keeps the user on the same hash / scroll pos.
  async function reloadCurrentView() {
    try { await loadSummary(); } catch (_) {}
    try { await loadTimeline?.(); } catch (_) {}
    try { await loadSkills(); } catch (_) {}
    // applyRoute re-binds the active pane's loader (loadActivity etc).
    applyRoute();
  }

  async function doLogout() {
    try { await api('POST', '/auth/logout'); } catch (_) {}
    setStoredApiKey('');
    account.me = null;
    account.libraryNames.clear();
    account.prefs = null;
    renderAccountPill();
    renderSettingsUser();
    renderScopeBar();
    renderSkills();
    // Clear the now-stale per-user data; subsequent loaders will hit
    // 401 and the dashboard will fall back to its empty state.
    await reloadCurrentView();
  }

  async function bulkAction(kind) {
    if (!account.me) { showAuthModal('login'); return; }
    if (kind === 'select-all') {
      const rows = document.getElementById('skill-rows');
      if (!rows) return;
      for (const row of rows.children) {
        if (row.style.display === 'none') continue;
        const name = row.dataset.skill;
        if (name) {
          account.bulkSelect.add(name);
          row.classList.add('selected');
        }
      }
      renderScopeBar();
      return;
    }
    if (kind === 'clear-sel') {
      account.bulkSelect.clear();
      document.querySelectorAll('#skill-rows .row.selected').forEach((r) =>
        r.classList.remove('selected'),
      );
      renderScopeBar();
      return;
    }
    if (kind !== 'add' && kind !== 'remove') return;
    if (account.bulkSelect.size === 0) {
      alert('请先选中要操作的 skill');
      return;
    }
    try {
      await api('POST', '/api/skills/library', {
        action: kind,
        names: Array.from(account.bulkSelect),
      });
      // Keep selection state so user can do "加入 → 移出" tweaking; clear
      // only after the in-library indicator catches up via a fresh
      // renderSkillsRows() call.
      await refreshLibraryNames();
      // Re-render with new in-library state, then re-apply selection set
      // so the user sees their choices still highlighted.
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
      renderSkills();
    } catch (e) {
      alert('批量操作失败: ' + e.message);
    }
  }

  async function quickFillTop50() {
    if (!account.me) { showAuthModal('login'); return; }
    try {
      await api('POST', '/api/skills/library/fill?top=50');
      await refreshLibraryNames();
      renderScopeBar();
      renderSkills();
    } catch (e) { alert(e.message); }
  }

  async function quickImportFromUsage() {
    if (!account.me) { showAuthModal('login'); return; }
    try {
      await api('POST', '/api/skills/library/import-from-usage');
      await refreshLibraryNames();
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
      renderSkills();
    } catch (e) { alert(e.message); }
  }

  async function quickClear() {
    if (!account.me) { showAuthModal('login'); return; }
    const ok = await showConfirm({
      title: '清空我的库',
      body: `当前 ${account.libraryNames.size} 个 skill 将被一次性从你的库里全部移除，操作不可撤销。公共池本身不会动。`,
      ok: '清空',
      cancel: '取消',
      danger: true,
    });
    if (!ok) return;
    try {
      await api('POST', '/api/skills/library/clear');
      await refreshLibraryNames();
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
      renderSkills();
    } catch (e) { alert(e.message); }
  }

  /// Single source of truth for "push account.prefs to the server".
  /// Returns a promise so callers can await + restore UI on failure.
  async function savePrefs() {
    if (!account.me || !account.prefs) return null;
    try {
      account.prefs = await api('POST', '/api/prefs', account.prefs);
      return account.prefs;
    } catch (e) {
      alert('保存偏好失败: ' + e.message);
      // Reload from server so UI reflects DB truth.
      await refreshPrefs();
      renderSettingsUser();
      return null;
    }
  }

  async function togglePrefAllowPublic() {
    if (!account.me || !account.prefs) return;
    account.prefs.allow_public_recommend = $('#set-allow-public-recommend').checked;
    await savePrefs();
  }

  function bindAccountUI() {
    $('#account-login-btn')?.addEventListener('click', () => showAuthModal('login'));
    $('#account-logout-btn')?.addEventListener('click', doLogout);
    $('#auth-close')?.addEventListener('click', hideAuthModal);
    $('#auth-form')?.addEventListener('submit', submitAuth);
    $('#auth-toggle')?.addEventListener('click', () => {
      const m = $('#auth-modal').dataset.mode === 'register' ? 'login' : 'register';
      showAuthModal(m);
    });

    document.querySelectorAll('.scope-btn').forEach((btn) => {
      btn.addEventListener('click', () => {
        account.scope = btn.dataset.scope;
        renderScopeBar();
        renderSkills();
      });
    });

    document.querySelectorAll('[data-bulk]').forEach((btn) => {
      btn.addEventListener('click', () => bulkAction(btn.dataset.bulk));
    });

    $('#lib-fill-50')?.addEventListener('click', quickFillTop50);
    $('#lib-import-usage')?.addEventListener('click', quickImportFromUsage);
    $('#lib-clear')?.addEventListener('click', quickClear);
    $('#set-allow-public-recommend')?.addEventListener('change', togglePrefAllowPublic);
    $('#select-mode-btn')?.addEventListener('click', toggleSelectMode);
  }

  // ------------------------------------------------------------------
  //  v15 Market (per-user install from public sources + arbitrary GitHub)
  // ------------------------------------------------------------------
  const marketState = {
    items: [],          // current page only (server-paginated)
    filter: '',
    sources: [],
    detail: null,
    refreshing: false,
    // tab = sort/filter mode sent to server (`sort` query) OR
    // client-side filter on the loaded page (`library`/`installed`).
    tab: 'all',
    // server-side pagination state
    offset: 0,
    limit: 50,
    total: 0,
    sourceFilter: '',
  };

  function marketSortParam() {
    // Server understands `all`/`trending`/`hot`. The `library` and
    // `installed` tabs reuse `all` server-side and filter client-side.
    switch (marketState.tab) {
      case 'trending': return 'trending';
      case 'hot':      return 'hot';
      default:         return 'all';
    }
  }

  async function loadMarket() {
    const grid = $('#market-grid');
    const empty = $('#market-empty');
    const count = $('#market-count');
    if (!grid) return;
    grid.innerHTML = '<div class="muted" style="padding:18px">加载中 …</div>';
    try {
      const params = new URLSearchParams({
        sort: marketSortParam(),
        offset: String(marketState.offset),
        limit: String(marketState.limit),
      });
      if (marketState.filter) params.set('q', marketState.filter);
      const data = await api('GET', `/api/market?${params}`);
      marketState.items = data.items || [];
      marketState.sources = data.sources || [];
      marketState.total = data.total || 0;
      marketState.offset = data.offset || 0;
      marketState.limit = data.limit || marketState.limit;
      renderSourceStatus();
      renderMarket();
      if (count) count.textContent = ` ${marketState.total.toLocaleString()} skill 可装`;
      // First-visit warm-up: no cache anywhere → spawn refresh in the
      // background so content shows up without a manual click.
      if (data.needs_refresh && !marketState.refreshing) {
        marketState.refreshing = true;
        renderSourceStatus();
        try { await api('POST', '/api/market/refresh'); } catch (_) {}
        marketState.refreshing = false;
        await loadMarket();
      }
    } catch (e) {
      grid.innerHTML = `<div class="muted" style="padding:18px">加载失败：${escapeHTML(e.message)}</div>`;
      if (empty) empty.hidden = true;
    }
  }

  function renderSourceStatus() {
    const wrap = $('#market-source-status');
    if (!wrap) return;
    // Hide the chip row entirely when the only enabled source is
    // skills.sh — no point showing a one-chip filter. As soon as the
    // user adds a custom GitHub repo via "+ GitHub" the chips re-appear.
    const userSources = marketState.sources.filter((s) => s.label !== 'skills.sh');
    if (userSources.length === 0 && !marketState.refreshing) {
      wrap.innerHTML = '';
      wrap.style.display = 'none';
      return;
    }
    wrap.style.display = '';
    const parts = marketState.sources.map((s) => {
      const empty = s.cached_count === 0;
      const active = marketState.sourceFilter === s.label;
      const cls = ['src-chip', empty ? 'empty' : '', active ? 'active' : '']
        .filter(Boolean)
        .join(' ');
      return `<button type="button" class="${cls}" data-src="${escapeHTML(s.label)}">
        ${escapeHTML(s.label)} <span class="src-n">${s.cached_count.toLocaleString()}</span>
      </button>`;
    });
    if (marketState.refreshing) {
      parts.push('<span class="src-chip refreshing">拉取 skills.sh 中 …</span>');
    }
    wrap.innerHTML = parts.join('');
    wrap.querySelectorAll('button[data-src]').forEach((btn) => {
      btn.addEventListener('click', () => {
        // Click an active chip again to clear the filter.
        marketState.sourceFilter = marketState.sourceFilter === btn.dataset.src
          ? ''
          : btn.dataset.src;
        marketState.offset = 0;
        loadMarket();
      });
    });
  }

  function formatInstalls(n) {
    if (!n) return '';
    if (n >= 1_000_000) return (n / 1_000_000).toFixed(1).replace(/\.0$/, '') + 'M';
    if (n >= 1_000)     return (n / 1_000).toFixed(1).replace(/\.0$/, '') + 'K';
    return String(n);
  }

  function renderSparkline(weekly) {
    if (!weekly || weekly.length < 2) {
      return '<span class="ml-trend-empty">—</span>';
    }
    const max = Math.max(...weekly, 1);
    const w = 96, h = 24;
    const step = w / (weekly.length - 1);
    const points = weekly
      .map((v, i) => `${(i * step).toFixed(1)},${(h - (v / max) * h).toFixed(1)}`)
      .join(' ');
    return `<svg class="ml-trend-svg" viewBox="0 0 ${w} ${h}" preserveAspectRatio="none">
      <polyline points="${points}" fill="none" stroke="currentColor" stroke-width="1.4"/>
    </svg>`;
  }

  function renderMarket() {
    const rows = $('#market-grid');
    const empty = $('#market-empty');
    const pagerEl = $('#market-pager');
    if (!rows) return;

    // Pages come server-side already filtered + sorted; client-side
    // narrowing only kicks in for the My-Library / Installed tabs which
    // reuse `all` sort but show a subset of the page.
    let visible = marketState.items.slice();
    if (marketState.tab === 'library') visible = visible.filter((s) => s.in_my_library);
    else if (marketState.tab === 'installed') visible = visible.filter((s) => s.installed);
    if (marketState.sourceFilter) {
      visible = visible.filter((s) => (s.source_label || '') === marketState.sourceFilter);
    }

    const setCount = (id, n) => {
      const el = document.getElementById(id);
      if (el) el.textContent = ` ${n.toLocaleString()}`;
    };
    // total count for the active sort comes back as `marketState.total`;
    // library/installed counts are estimated from the current page only.
    const libCount = marketState.items.filter((s) => s.in_my_library).length;
    const instCount = marketState.items.filter((s) => s.installed).length;
    setCount('mtab-count-all',       marketState.tab === 'all'       ? marketState.total : marketState.total);
    setCount('mtab-count-trending',  marketState.tab === 'trending'  ? marketState.total : 0);
    setCount('mtab-count-hot',       marketState.tab === 'hot'       ? marketState.total : 0);
    setCount('mtab-count-library',   libCount);
    setCount('mtab-count-installed', instCount);

    rows.innerHTML = '';
    if (visible.length === 0) {
      if (empty) empty.hidden = false;
      if (pagerEl) pagerEl.innerHTML = '';
      return;
    }
    if (empty) empty.hidden = true;

    const frag = document.createDocumentFragment();
    visible.forEach((s, idx) => {
      const row = document.createElement('div');
      row.className = 'ml-row' + (s.in_my_library ? ' in-library' : '');
      row.dataset.mktSkill = JSON.stringify(s);

      const rank = marketState.offset + idx + 1;
      const installLabel = s.in_my_library
        ? '已在库'
        : s.installed
          ? '加入我的库'
          : '安装到我的库';
      const officialBadge = s.is_official
        ? '<span class="ml-official-badge" title="Official">OFFICIAL</span>'
        : '';
      const installsText = marketState.tab === 'trending' ? s.trending_installs
        : marketState.tab === 'hot' ? s.hot_score
        : s.installs;
      const installsLabel = formatInstalls(installsText) ||
        '<span class="ml-trend-empty">—</span>';

      row.innerHTML = `
        <div class="ml-rank">${rank}</div>
        <div class="ml-skill">
          <div class="ml-skill-name">${escapeHTML(s.name)} ${officialBadge}</div>
          <div class="ml-skill-repo">${escapeHTML(s.source_repo || '')}</div>
        </div>
        <div class="ml-trend">${renderSparkline(s.weekly_installs)}</div>
        <div class="ml-installs">${installsLabel}</div>
        <div class="ml-action">
          <button type="button" class="btn-small ml-install-btn" data-mkt-install="${escapeHTML(s.name)}" ${s.in_my_library ? 'disabled' : ''}>
            ${installLabel}
          </button>
        </div>
      `;
      row.addEventListener('click', (ev) => {
        if (ev.target.closest('button')) return;
        openMarketDetail(s);
      });
      frag.appendChild(row);
    });
    rows.appendChild(frag);

    rows.querySelectorAll('button[data-mkt-install]').forEach((b) =>
      b.addEventListener('click', (ev) => {
        ev.stopPropagation();
        installFromMarket(b.dataset.mktInstall, b);
      }),
    );

    renderPager();
  }

  function renderPager() {
    const pagerEl = $('#market-pager');
    if (!pagerEl) return;
    const { offset, limit, total } = marketState;
    if (total <= limit) {
      pagerEl.innerHTML = '';
      return;
    }
    const page = Math.floor(offset / limit) + 1;
    const pages = Math.max(1, Math.ceil(total / limit));
    const hasPrev = offset > 0;
    const hasNext = offset + limit < total;
    pagerEl.innerHTML = `
      <button type="button" class="ml-pager-btn" id="market-pager-prev" ${hasPrev ? '' : 'disabled'}>← 上一页</button>
      <span class="ml-pager-info">第 ${page} / ${pages} 页 · ${(offset + 1).toLocaleString()}–${Math.min(offset + limit, total).toLocaleString()} / ${total.toLocaleString()}</span>
      <button type="button" class="ml-pager-btn" id="market-pager-next" ${hasNext ? '' : 'disabled'}>下一页 →</button>
    `;
    pagerEl.querySelector('#market-pager-prev')?.addEventListener('click', () => {
      marketState.offset = Math.max(0, marketState.offset - marketState.limit);
      loadMarket();
    });
    pagerEl.querySelector('#market-pager-next')?.addEventListener('click', () => {
      marketState.offset = marketState.offset + marketState.limit;
      loadMarket();
    });
  }

  async function openMarketDetail(skill) {
    const modal = $('#market-detail-modal');
    if (!modal) return;
    marketState.detail = skill;
    modal.classList.remove('hide');
    $('#market-detail-name').textContent = skill.name;
    $('#market-detail-source').textContent = `${skill.source_label} · ${skill.source_repo}`;
    const status = skill.in_my_library ? '已在我的库'
      : skill.installed ? '已安装在公共池'
      : '未安装';
    $('#market-detail-status').textContent = status;
    const repoUrl = `https://github.com/${skill.source_repo}/tree/${skill.branch || 'main'}/${skill.repo_path || ''}`;
    $('#market-detail-repo').href = repoUrl;
    $('#market-detail-repo').textContent = `${skill.source_repo}${skill.repo_path ? '/' + skill.repo_path : ''} · ${skill.branch || 'main'} →`;
    const mdEl = $('#market-detail-md');
    const filesEl = $('#market-detail-files');
    mdEl.textContent = 'SKILL.md 加载中 …';
    if (filesEl) filesEl.textContent = '加载中 …';
    const installBtn = $('#market-detail-install');
    installBtn.disabled = !!skill.in_my_library;
    installBtn.textContent = skill.in_my_library ? '已在库' : '安装到我的库';
    const params = new URLSearchParams({
      source_repo: skill.source_repo,
      branch: skill.branch || 'main',
      repo_path: skill.repo_path || '',
      skill_name: skill.name || '',
    });
    // Kick off the SKILL.md fetch and the file-list fetch in parallel so
    // the modal doesn't wait for two sequential round trips.
    const mdReq = api('GET', `/api/market/preview?${params}`)
      .then((data) => {
        if (data.skill_md) mdEl.textContent = data.skill_md;
        else mdEl.textContent = `[未能加载 SKILL.md${data.error ? ': ' + data.error : ''}]`;
      })
      .catch((e) => { mdEl.textContent = `[加载失败：${e.message}]`; });
    const filesReq = api('GET', `/api/market/preview-files?${params}`)
      .then((data) => {
        if (!filesEl) return;
        if (data.error || !data.entries || data.entries.length === 0) {
          filesEl.textContent = data.error ? `[文件列表加载失败：${data.error}]` : '仅 SKILL.md';
          return;
        }
        filesEl.innerHTML = data.entries
          .map((e) => {
            const tag = e.is_dir ? '<span class="md-file-dir">DIR</span>' : '<span class="md-file-file">FILE</span>';
            const sz = e.is_dir ? '' : `<span class="md-file-size">${(e.size || 0).toLocaleString()} B</span>`;
            return `<div class="md-file-row">${tag}<span class="md-file-name">${escapeHTML(e.path)}</span>${sz}</div>`;
          })
          .join('');
      })
      .catch((e) => { if (filesEl) filesEl.textContent = `[文件列表失败：${e.message}]`; });
    await Promise.all([mdReq, filesReq]);
  }

  function closeMarketDetail() {
    $('#market-detail-modal')?.classList.add('hide');
    marketState.detail = null;
  }

  async function installFromDetail() {
    if (!marketState.detail) return;
    const btn = $('#market-detail-install');
    if (!btn) return;
    btn.disabled = true;
    btn.textContent = '安装中 …';
    try {
      await api('POST', '/api/market/install', { name: marketState.detail.name });
      await refreshLibraryNames();
      await loadMarket();
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
      closeMarketDetail();
    } catch (e) {
      alert('安装失败：' + e.message);
      btn.disabled = false;
      btn.textContent = '重试';
    }
  }

  async function installFromMarket(name, btn) {
    if (!account.me) { showAuthModal('login'); return; }
    if (btn) { btn.disabled = true; btn.textContent = '安装中 …'; }
    try {
      // Send the source label so api_market_install routes through the
      // skills.sh dispatch even if the user has installed the same name
      // previously from a different source.
      await api('POST', '/api/market/install', { name, source: 'skills.sh' });
      // Optimistic in-place update — no full re-fetch. The leaderboard
      // doesn't need to re-sort just because one row's status changed.
      await refreshLibraryNames();
      for (const s of marketState.items) {
        if (s.name === name) {
          s.in_my_library = true;
          s.installed = true;
        }
      }
      renderMarket();
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
    } catch (e) {
      alert('安装失败：' + e.message);
      if (btn) { btn.disabled = false; btn.textContent = '重试'; }
    }
  }

  // GitHub parse + pick modal state.
  const ghImport = { skills: [], selected: new Set(), source: '', resp: null };

  function showGithubModal() {
    if (!account.me) { showAuthModal('login'); return; }
    const m = $('#github-modal');
    if (!m) return;
    m.classList.remove('hide');
    ghImport.skills = [];
    ghImport.selected.clear();
    ghImport.resp = null;
    $('#github-err')?.classList.add('hide');
    $('#github-busy')?.classList.add('hide');
    $('#github-result')?.classList.add('hide');
    $('#github-src').value = '';
    $('#github-form')?.classList.remove('hide');
    setTimeout(() => $('#github-src')?.focus(), 30);
  }
  function hideGithubModal() {
    $('#github-modal')?.classList.add('hide');
  }

  async function parseGithub(ev) {
    ev?.preventDefault?.();
    const src = $('#github-src').value.trim();
    const err = $('#github-err');
    const busy = $('#github-busy');
    const btn = $('#github-parse-btn');
    if (!src) return;
    err?.classList.add('hide');
    busy?.classList.remove('hide');
    if (busy) busy.textContent = '解析仓库结构 …';
    if (btn) btn.disabled = true;
    try {
      const resp = await api('POST', '/api/parse/github', { source: src });
      ghImport.skills = resp.skills || [];
      ghImport.resp = resp;
      ghImport.source = src;
      // Default-select every skill that's not already in this user's library.
      ghImport.selected.clear();
      for (const s of ghImport.skills) {
        if (!s.in_my_library) ghImport.selected.add(s.name);
      }
      renderGithubResult();
      $('#github-form')?.classList.add('hide');
      $('#github-result')?.classList.remove('hide');
    } catch (e) {
      if (err) {
        err.textContent = e.message || '解析失败';
        err.classList.remove('hide');
      }
    } finally {
      busy?.classList.add('hide');
      if (btn) btn.disabled = false;
    }
  }

  function renderGithubResult() {
    const list = $('#github-skill-list');
    const summary = $('#github-result-summary');
    const countEl = $('#github-install-count');
    if (!list) return;
    const total = ghImport.skills.length;
    const inLibrary = ghImport.skills.filter((s) => s.in_my_library).length;
    const newish = total - inLibrary;
    if (summary) {
      summary.textContent = `${ghImport.resp.owner}/${ghImport.resp.repo}@${ghImport.resp.branch} · ${total} 个 skill · ${inLibrary} 已在我的库 · ${newish} 可导入`;
    }
    list.innerHTML = '';
    if (total === 0) {
      list.innerHTML = '<div class="muted" style="padding:14px;text-align:center">该仓库没找到任何 skill</div>';
    }
    for (const s of ghImport.skills) {
      const row = document.createElement('label');
      row.className = 'github-skill-row' + (s.in_my_library ? ' disabled' : '');
      const checked = ghImport.selected.has(s.name) ? 'checked' : '';
      const statusBadge = s.in_my_library
        ? '<span class="gsr-status in-lib">已存在</span>'
        : s.already_installed
          ? '<span class="gsr-status installed">公共池已有</span>'
          : '<span class="gsr-status new">新</span>';
      row.innerHTML = `
        <input type="checkbox" data-name="${escapeHTML(s.name)}" ${checked} ${s.in_my_library ? 'disabled' : ''}>
        <span class="gsr-name">${escapeHTML(s.name)}</span>
        <span class="gsr-path" title="${escapeHTML(s.repo_path)}">${escapeHTML(s.repo_path)}</span>
        ${statusBadge}
      `;
      const box = row.querySelector('input');
      box.addEventListener('change', () => {
        if (box.checked) ghImport.selected.add(s.name);
        else ghImport.selected.delete(s.name);
        if (countEl) countEl.textContent = ghImport.selected.size;
      });
      list.appendChild(row);
    }
    if (countEl) countEl.textContent = ghImport.selected.size;
  }

  function githubSelectAllNew() {
    ghImport.selected.clear();
    for (const s of ghImport.skills) {
      if (!s.in_my_library) ghImport.selected.add(s.name);
    }
    renderGithubResult();
  }
  function githubClearSelection() {
    ghImport.selected.clear();
    renderGithubResult();
  }
  function githubBack() {
    $('#github-result')?.classList.add('hide');
    $('#github-form')?.classList.remove('hide');
    setTimeout(() => $('#github-src')?.focus(), 30);
  }

  async function installSelectedFromGithub() {
    if (ghImport.selected.size === 0) {
      await showInfo({ title: '提示', body: '请至少勾选一个 skill' });
      return;
    }
    const busy = $('#github-busy');
    const installBtn = $('#github-install-selected');
    busy?.classList.remove('hide');
    if (busy) busy.textContent = `下载 ${ghImport.selected.size} 个 skill …`;
    if (installBtn) installBtn.disabled = true;
    try {
      const resp = await api('POST', '/api/install/github', {
        source: ghImport.source,
        skills: Array.from(ghImport.selected),
      });
      hideGithubModal();
      await refreshLibraryNames();
      if (typeof renderSkillsRows === 'function') renderSkillsRows();
      renderScopeBar();
      renderSkills();
      // Trigger a market reload silently to keep its "installed" flags
      // accurate; users don't see Market anymore but cache might be used.
      try { await loadMarket(); } catch (_) {}
      await showInfo({
        title: '导入完成',
        body: `已导入 ${resp.installed.length} 个 skill 到我的库`,
        items: resp.installed,
      });
    } catch (e) {
      await showInfo({
        title: '导入失败',
        body: e.message || String(e),
        danger: true,
      });
    } finally {
      busy?.classList.add('hide');
      if (installBtn) installBtn.disabled = false;
    }
  }

  function bindMarketUI() {
    let mkSearchTimer = 0;
    $('#market-filter')?.addEventListener('input', (ev) => {
      marketState.filter = ev.target.value;
      // Mirror the typed query into the TRY IT NOW command so a user
      // who pastes `owner/repo` or `skill-name` sees the matching
      // command immediately. Empty → placeholder.
      const cmd = $('#market-tryit-cmd');
      if (cmd) {
        const v = (ev.target.value || '').trim();
        cmd.textContent = v
          ? `runai install ${v}`
          : 'runai install <owner/repo>';
      }
      // Debounce server fetch to 250ms so typing doesn't fire 5 reqs.
      clearTimeout(mkSearchTimer);
      mkSearchTimer = setTimeout(() => {
        marketState.offset = 0;
        loadMarket();
      }, 250);
    });
    $('#market-refresh-btn')?.addEventListener('click', async () => {
      const btn = $('#market-refresh-btn');
      if (btn) { btn.disabled = true; btn.textContent = '拉取中 …'; }
      marketState.refreshing = true;
      renderSourceStatus();
      try {
        await api('POST', '/api/market/refresh');
        await loadMarket();
      } catch (e) {
        alert('刷新失败：' + e.message);
      } finally {
        marketState.refreshing = false;
        renderSourceStatus();
        if (btn) { btn.disabled = false; btn.textContent = '刷新'; }
      }
    });
    $('#market-add-github-btn')?.addEventListener('click', showGithubModal);
    $('#lib-import-github')?.addEventListener('click', showGithubModal);

    // Leaderboard tab switcher. Switching sort hits the server again
    // (new sort key); switching to library/installed is a client-side
    // filter on the current page.
    document.querySelectorAll('.market-tab-btn').forEach((btn) => {
      btn.addEventListener('click', () => {
        const nextTab = btn.dataset.mtab || 'all';
        const sortChanged = (
          (marketState.tab !== nextTab) &&
          ['all', 'trending', 'hot'].includes(nextTab)
        );
        marketState.tab = nextTab;
        marketState.offset = 0;
        document.querySelectorAll('.market-tab-btn').forEach((b) =>
          b.classList.toggle('active', b === btn),
        );
        if (sortChanged || ['all', 'trending', 'hot'].includes(nextTab)) {
          loadMarket();
        } else {
          renderMarket();
        }
      });
    });

    // TRY IT NOW copy button — copies the current install template
    // (auto-fills the skill name when the search box has one match).
    $('#market-tryit-copy')?.addEventListener('click', async () => {
      const cmd = $('#market-tryit-cmd');
      if (!cmd) return;
      try {
        await navigator.clipboard.writeText(cmd.textContent.trim());
        const btn = $('#market-tryit-copy');
        if (btn) {
          const orig = btn.textContent;
          btn.textContent = 'copied';
          setTimeout(() => (btn.textContent = orig), 1200);
        }
      } catch (_) {
        // navigator.clipboard requires a secure context. Silent fail.
      }
    });

    // `/` keystroke focuses the search box — skills.sh parity.
    document.addEventListener('keydown', (ev) => {
      if (ev.key !== '/') return;
      // Only when Market tab is active and user is not already typing.
      const pane = document.querySelector('.tab[data-pane="market"]');
      if (!pane || !pane.classList.contains('active')) return;
      const tag = (ev.target.tagName || '').toUpperCase();
      if (tag === 'INPUT' || tag === 'TEXTAREA') return;
      ev.preventDefault();
      $('#market-filter')?.focus();
    });
    $('#github-cancel')?.addEventListener('click', hideGithubModal);
    $('#github-form')?.addEventListener('submit', parseGithub);
    $('#github-parse-btn')?.addEventListener('click', parseGithub);
    $('#github-select-all')?.addEventListener('click', githubSelectAllNew);
    $('#github-clear')?.addEventListener('click', githubClearSelection);
    $('#github-back')?.addEventListener('click', githubBack);
    $('#github-install-selected')?.addEventListener('click', installSelectedFromGithub);
    $('#github-modal')?.addEventListener('click', (ev) => {
      if (ev.target.id === 'github-modal') hideGithubModal();
    });
    $('#market-detail-close')?.addEventListener('click', closeMarketDetail);
    $('#market-detail-dismiss')?.addEventListener('click', closeMarketDetail);
    $('#market-detail-install')?.addEventListener('click', installFromDetail);
    $('#market-detail-modal')?.addEventListener('click', (ev) => {
      if (ev.target.id === 'market-detail-modal') closeMarketDetail();
    });
  }

  // ------------------------------------------------------------------
  //  Boot
  // ------------------------------------------------------------------
  initSwatches();
  bindControls();
  initCustomCursor();
  bindSettingsControls();
  bindAccountUI();
  bindMarketUI();
  refreshMe();
  applyRoute();
  startPolling();
})();
