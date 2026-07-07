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
    const authBadge = e.authenticated
      ? `<span class="chip">认证用户</span>`
      : `<span class="chip-empty">匿名无 Bearer</span>`;
    const bm25CandidateChips = Array.isArray(e.bm25_candidates) && e.bm25_candidates.length
      ? e.bm25_candidates.map((s) => `<span class="skill-chip">${escapeHTML(s)}</span>`).join('')
      : '<span class="chip-empty">(legacy row / empty)</span>';
    const intentStatus = e.intent_status
      ? `<span class="chip">${escapeHTML(e.intent_status)}</span>${e.intent_error_msg ? ` <span class="muted">— ${escapeHTML(e.intent_error_msg)}</span>` : ''}`
      : '<span class="chip-empty">legacy row</span>';
    body.innerHTML = `
      <dl class="detail-grid">
        <dt>时间</dt><dd class="mono">${fmtTsFull(e.ts)}</dd>
        <dt>状态</dt><dd class="${statusKlass}"><span class="status-dot"></span>${escapeHTML(e.status)}${e.error_msg ? ` <span class="muted">— ${escapeHTML(e.error_msg)}</span>` : ''}</dd>
        <dt>注入</dt><dd>${injectedBadge}</dd>
        <dt>模型</dt><dd><span class="mono">${escapeHTML(e.provider)} · ${escapeHTML(e.model)}</span></dd>
        <dt>模式</dt><dd class="mono">${escapeHTML(e.mode)}</dd>
        <dt>session</dt><dd class="mono muted">${escapeHTML(e.session_id || '(none)')}</dd>
        <dt>身份</dt><dd>${authBadge}</dd>
        <dt>BM25</dt><dd>${e.bm25_kept} / ${e.candidate_count} 候选</dd>
        <dt>token</dt><dd>prompt <span class="mono">${fmtInt(e.prompt_tokens)}</span> · completion <span class="mono">${fmtInt(e.completion_tokens)}</span> · total <span class="mono">${fmtInt(e.total_tokens)}</span></dd>
        <dt>延迟</dt><dd><span class="mono">${fmtMs(e.latency_ms)} ms</span></dd>
        <dt>cwd</dt><dd class="mono muted">${escapeHTML(e.cwd || '(none)')}</dd>
      </dl>
      <div class="section-label">chosen skills (点击进详情)</div>
      <div>${chosenInline}</div>
      <div class="section-label">user prompt (hook 收到的原文)</div>
      <div class="prompt-block">${escapeHTML(e.user_prompt) || '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">第一波：意图识别状态</div>
      <div>${intentStatus}</div>
      <div class="section-label">第一波：意图识别 LLM 输入</div>
      <div class="prompt-block">${e.intent_llm_input ? escapeHTML(e.intent_llm_input) : '<span class="dim">(legacy row · schema v25 之后的事件才有)</span>'}</div>
      <div class="section-label">第一波：压缩后的 BM25 检索意图</div>
      <div class="prompt-block">${e.intent_llm_output ? escapeHTML(e.intent_llm_output) : '<span class="dim">(legacy row)</span>'}</div>
      <div class="section-label">第一波：BM25 候选</div>
      <div class="skill-chips">${bm25CandidateChips}</div>
      <div class="section-label">第二波：router LLM 实际收到的完整输入</div>
      <div class="prompt-block">${e.llm_input ? escapeHTML(e.llm_input) : '<span class="dim">(legacy row · schema v13 之后的事件才有)</span>'}</div>
      <div class="section-label">第二波：router LLM 原始返回</div>
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
