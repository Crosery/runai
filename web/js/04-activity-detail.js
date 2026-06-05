  // ------------------------------------------------------------------
  //  Activity tab — full event list + chart + filter
  // ------------------------------------------------------------------
  const activityState = {
    hours: '24',
    hitOnly: '',
    filter: '',
    limit: 100,
    offset: 0,      // how many rows already loaded (next page starts here)
    total: 0,
    loading: false,
    done: false,
  };

  // Build + append one batch of event rows to the list (no clearing — this is
  // what keeps infinite scroll from resetting the scroll position). Returns the
  // number of rows actually appended after the client-side text filter.
  function appendActivityRows(events) {
    const list = $('#activity-list');
    const f = activityState.filter.toLowerCase();
    const rows = events.filter((e) => {
      if (!activityState.filter) return true;
      const skill = ((e.chosen && e.chosen[0]) || '').toLowerCase();
      const prompt = (e.user_prompt || '').toLowerCase();
      const model = (e.model || '').toLowerCase();
      return skill.includes(f) || prompt.includes(f) || model.includes(f);
    });
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
    return rows.length;
  }

  // Fetch one page of events. fresh=true resets to the top (filter/window
  // change); otherwise it APPENDS the next page (infinite scroll), never
  // clearing what's on screen, so the scroll position is preserved.
  async function loadActivityEvents(fresh) {
    if (activityState.loading) return;
    if (!fresh && activityState.done) return;
    activityState.loading = true;
    if (fresh) {
      activityState.offset = 0;
      activityState.done = false;
      $('#activity-list').innerHTML = '';
    }
    updateActivityMore();
    const qs = new URLSearchParams();
    if (activityState.hours) qs.set('hours', activityState.hours);
    qs.set('limit', activityState.limit);
    qs.set('offset', activityState.offset);
    if (activityState.hitOnly) qs.set('hit_only', '1');
    try {
      const res = await fetch(`/api/events?${qs.toString()}`);
      if (res.ok) {
        const data = await res.json();
        activityState.total = data.total ?? 0;
        $('#activity-count').textContent = fmtInt(activityState.total);
        const evs = data.events || data.rows || [];
        appendActivityRows(evs);
        activityState.offset += evs.length;
        activityState.done = evs.length === 0 || activityState.offset >= activityState.total;
        const list = $('#activity-list');
        if (fresh && list.children.length === 0) {
          list.innerHTML = '<div class="stream-empty muted">这个区间没有事件</div>';
        }
      }
    } catch (_) {}
    activityState.loading = false;
    updateActivityMore();
  }

  // The infinite-scroll sentinel: a row at the bottom of the list. It auto-loads
  // the next page when it scrolls into view, and is also clickable as a fallback.
  let activityObserver = null;
  function ensureActivitySentinel() {
    let more = $('#activity-more');
    if (!more) {
      const host = $('#activity-pager') || $('#activity-list')?.parentElement;
      if (!host) return;
      // retire the old prev/next pager — infinite scroll replaces it
      $('#activity-prev')?.setAttribute('hidden', '');
      $('#activity-next')?.setAttribute('hidden', '');
      $('#activity-page')?.setAttribute('hidden', '');
      more = document.createElement('button');
      more.type = 'button';
      more.id = 'activity-more';
      more.className = 'load-more';
      more.addEventListener('click', () => loadActivityEvents(false));
      host.appendChild(more);
    }
    if (!activityObserver) {
      activityObserver = new IntersectionObserver((entries) => {
        if (entries.some((en) => en.isIntersecting)) loadActivityEvents(false);
      }, { rootMargin: '400px' });
      activityObserver.observe(more);
    }
  }

  function updateActivityMore() {
    const more = $('#activity-more');
    if (!more) return;
    if (activityState.done) {
      more.textContent = activityState.total > 0
        ? `已全部加载 · 共 ${fmtInt(activityState.total)} 条`
        : '';
      more.classList.add('done');
    } else if (activityState.loading) {
      more.textContent = '加载中 …';
      more.classList.remove('done');
    } else {
      more.textContent = `加载更多 · 已显示 ${fmtInt(activityState.offset)} / ${fmtInt(activityState.total)}`;
      more.classList.remove('done');
    }
  }

  function loadActivityChart() {
    const buckets = activityState.hours === '1' ? 12 : activityState.hours === '24' ? 48 : activityState.hours === '168' ? 56 : 60;
    const tq = new URLSearchParams();
    tq.set('hours', activityState.hours || '24');
    tq.set('buckets', buckets);
    fetch(`/api/timeline?${tq.toString()}`)
      .then((r) => (r.ok ? r.json() : null))
      .then((td) => { if (td) drawTimelineInto('#activity-chart', td); })
      .catch(() => {});
  }

  // Public entry: fresh reload of the Activity tab (open / filter / window
  // change). Redraws the chart and resets the event list to the first page;
  // subsequent pages stream in via the infinite-scroll sentinel.
  async function loadActivity() {
    ensureActivitySentinel();
    loadActivityChart();
    await loadActivityEvents(true);
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

