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
    // Reload the page in place without the window snapping back to the top
    // (clearing the fixed-height rows briefly collapses page height → scroll
    // clamps to 0). Capture scrollY and restore it after the grid refills.
    const pageMarket = (newOffset) => {
      marketState.offset = newOffset;
      const y = window.scrollY;
      const keep = () => window.scrollTo({ top: y });
      loadMarket().finally(() => { keep(); requestAnimationFrame(keep); });
    };
    pagerEl.querySelector('#market-pager-prev')?.addEventListener('click', () =>
      pageMarket(Math.max(0, marketState.offset - marketState.limit)));
    pagerEl.querySelector('#market-pager-next')?.addEventListener('click', () =>
      pageMarket(marketState.offset + marketState.limit));
  }

