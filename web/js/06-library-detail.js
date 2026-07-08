  // ------------------------------------------------------------------
  //  Library: skill detail
  // ------------------------------------------------------------------
  async function loadSkillDetail(name, owner) {
    detailState.name = name;
    // owner is set only when arriving from the admin "用户库" drill-in; it
    // pins detail/file resolution to that user's private pool (admin-gated
    // server-side). Empty/absent → normal own-scope resolution.
    detailState.owner = owner || null;
    const ownerQ = owner ? `?owner=${encodeURIComponent(owner)}` : '';
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

    const res = await fetch(`/api/skill/${encodeURIComponent(name)}${ownerQ}`);
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

    // 反馈画像：五轴雷达（本 skill vs 全库均值）+ 反馈统计 + 最近反馈列表。
    // radar/radar_avg 服务端总是返回一个值（零数据时是各轴自己的"中性"约定），
    // 所以这个 section 不像 AI 摘要那样需要按空值隐藏。
    renderSkillRadar('detail-radar', d.radar, d.radar_avg);
    renderFeedbackPanel(d);
    // Baseline for the live-poll short-circuit below: the next 5s tick
    // only touches the DOM if the feedback-relevant fields actually moved.
    lastLiveDetailSnapshot = liveDetailSnapshotKey(d);

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

    await loadFileTree(name, owner);
  }

  async function loadFileTree(name, owner) {
    const tree = $('#detail-file-tree');
    const ownerQ = owner ? `?owner=${encodeURIComponent(owner)}` : '';
    const res = await fetch(`/api/skill/${encodeURIComponent(name)}/files${ownerQ}`);
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
      div.addEventListener('click', () => selectFile(name, entry.path, owner));
      tree.appendChild(div);
    }
    const preferred =
      entries.find((e) => e.path === 'SKILL.md') ||
      entries.find((e) => e.is_text) ||
      entries[0];
    if (preferred) selectFile(name, preferred.path, owner);
  }

  async function selectFile(name, path, owner) {
    detailState.activeFile = path;
    $$('#detail-file-tree .ftree-entry').forEach((el) => {
      el.classList.toggle('active', el.dataset.path === path);
    });
    $('#detail-file-path').textContent = path;
    $('#detail-file-body').textContent = '加载中...';
    const ownerQ = owner ? `&owner=${encodeURIComponent(owner)}` : '';
    const url = `/api/skill/${encodeURIComponent(name)}/file?path=${encodeURIComponent(path)}${ownerQ}`;
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
  //  Library: skill detail — lightweight 5s poll refresh.
  //
  //  loadSkillDetail() above is the FULL paint: it blanks every field back
  //  to placeholders, re-fetches, rebuilds the event table, and re-fetches
  //  the file tree + the currently-selected file's content. That's correct
  //  for navigating into a skill (or switching skills) but was also what
  //  the 5s poll timer called on every tick — so a user just sitting on a
  //  skill detail page had the whole page (file viewer included) blank out
  //  and repaint every 5 seconds, refetching file content nothing had
  //  changed. That's the "网页很卡" root cause on this view, not server
  //  latency (/api/skill/{name} is ~1ms warm).
  //
  //  refreshSkillDetailLive() is what the poll timer calls instead: it
  //  only re-fetches /api/skill/{name} and, if the feedback-relevant
  //  fields actually changed since the last snapshot, updates the radar
  //  (via updateSkillRadarLive's in-place polygon move, see
  //  20-skill-radar.js) and the feedback stats/recent-list/enrich badge.
  //  It never touches the file tree, the event table, or the top strip —
  //  those don't change from feedback votes elsewhere and rebuilding them
  //  cost a scroll-position reset + extra fetches for no reason.
  // ------------------------------------------------------------------
  let lastLiveDetailSnapshot = null;

  function liveDetailSnapshotKey(d) {
    return JSON.stringify({
      radar: d.radar || null,
      radar_avg: d.radar_avg || null,
      feedback_stats: d.feedback_stats || null,
      feedback_recent: d.feedback_recent || null,
      enrich_status: d.enrich_status || null,
    });
  }

  async function refreshSkillDetailLive(name) {
    if (!name || detailState.name !== name) return;
    const owner = detailState.owner;
    const ownerQ = owner ? `?owner=${encodeURIComponent(owner)}` : '';
    let d;
    try {
      const res = await fetch(`/api/skill/${encodeURIComponent(name)}${ownerQ}`);
      if (!res.ok) return;
      d = await res.json();
    } catch (_e) {
      return;
    }
    // The route may have moved on (or switched skill) while this fetch was
    // in flight — discard a stale response rather than paint it over
    // whatever the user has navigated to since.
    if (detailState.name !== name) return;

    const snapshotKey = liveDetailSnapshotKey(d);
    if (snapshotKey === lastLiveDetailSnapshot) return;
    lastLiveDetailSnapshot = snapshotKey;

    detailState.current = d;
    updateSkillRadarLive('detail-radar', d.radar, d.radar_avg);
    renderFeedbackPanel(d);
  }

