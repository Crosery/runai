  // ------------------------------------------------------------------
  //  v15 Market (per-user install from public sources + arbitrary GitHub)
  // ------------------------------------------------------------------
  const marketState = {
    items: [],          // accumulated across infinite-scroll pages
    filter: '',
    sources: [],
    detail: null,
    refreshing: false,
    // tab = sort/filter mode sent to server (`sort` query) OR
    // client-side filter on the loaded page (`library`/`installed`).
    tab: 'all',
    // infinite-scroll pagination state
    offset: 0,          // rows already loaded (next page starts here)
    limit: 50,
    total: 0,
    sourceFilter: '',
    loading: false,
    done: false,
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

  // Fetch one page from the market API. `append=false` is a fresh load (sort /
  // filter / source change): reset to the top and replace the grid. `append=true`
  // streams the next page in and APPENDS its cards — never clearing the grid, so
  // the scroll position stays put (infinite scroll, not jump-to-top paging).
  async function loadMarket(append) {
    const grid = $('#market-grid');
    const empty = $('#market-empty');
    const count = $('#market-count');
    if (!grid) return;
    if (marketState.loading) return;
    if (append && marketState.done) return;
    marketState.loading = true;
    if (!append) {
      marketState.offset = 0;
      marketState.items = [];
      marketState.done = false;
      grid.innerHTML = '<div class="muted" style="padding:18px">加载中 …</div>';
    }
    updateMarketMore();
    try {
      const params = new URLSearchParams({
        sort: marketSortParam(),
        offset: String(marketState.offset),
        limit: String(marketState.limit),
      });
      if (marketState.filter) params.set('q', marketState.filter);
      const data = await api('GET', `/api/market?${params}`);
      const newItems = data.items || [];
      marketState.sources = data.sources || [];
      marketState.total = data.total || 0;
      marketState.limit = data.limit || marketState.limit;
      const baseIndex = marketState.items.length;
      marketState.items = marketState.items.concat(newItems);
      marketState.offset = marketState.items.length;
      marketState.done = newItems.length === 0 || marketState.offset >= marketState.total;
      if (!append) grid.innerHTML = '';
      appendMarketBatch(newItems, baseIndex);
      renderSourceStatus();
      updateMarketCounts();
      ensureMarketSentinel();
      updateMarketMore();
      if (count) count.textContent = ` ${marketState.total.toLocaleString()} skill 可装`;
      if (empty) empty.hidden = grid.querySelector('.ml-row') != null;
      // First-visit warm-up: no cache anywhere → spawn refresh in the
      // background so content shows up without a manual click.
      if (!append && data.needs_refresh && !marketState.refreshing) {
        marketState.refreshing = true;
        renderSourceStatus();
        try { await api('POST', '/api/market/refresh'); } catch (_) {}
        marketState.refreshing = false;
        marketState.loading = false;
        await loadMarket(false);
        return;
      }
    } catch (e) {
      if (!append) {
        grid.innerHTML = `<div class="muted" style="padding:18px">加载失败：${escapeHTML(e.message)}</div>`;
        if (empty) empty.hidden = true;
      }
    }
    marketState.loading = false;
    updateMarketMore();
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

  // Client-side narrowing for the My-Library / Installed / source-chip filters
  // (these reuse the `all` sort server-side and show a subset of each page).
  function marketItemVisible(s) {
    if (marketState.tab === 'library' && !s.in_my_library) return false;
    if (marketState.tab === 'installed' && !s.installed) return false;
    if (marketState.sourceFilter && (s.source_label || '') !== marketState.sourceFilter) return false;
    return true;
  }

  function marketRowEl(s, rank) {
    const row = document.createElement('div');
    row.className = 'ml-row' + (s.in_my_library ? ' in-library' : '');
    row.dataset.mktSkill = JSON.stringify(s);
    const installLabel = s.in_my_library ? '已在库' : s.installed ? '加入我的库' : '安装到我的库';
    const officialBadge = s.is_official
      ? '<span class="ml-official-badge" title="Official">OFFICIAL</span>'
      : '';
    const installsText = marketState.tab === 'trending' ? s.trending_installs
      : marketState.tab === 'hot' ? s.hot_score
      : s.installs;
    const installsLabel = formatInstalls(installsText) || '<span class="ml-trend-empty">—</span>';
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
    const btn = row.querySelector('button[data-mkt-install]');
    if (btn) {
      btn.addEventListener('click', (ev) => {
        ev.stopPropagation();
        installFromMarket(btn.dataset.mktInstall, btn);
      });
    }
    return row;
  }

  // Append the visible subset of one freshly-fetched page to the grid (no clear).
  function appendMarketBatch(items, baseIndex) {
    const grid = $('#market-grid');
    if (!grid) return;
    const frag = document.createDocumentFragment();
    items.forEach((s, i) => {
      if (!marketItemVisible(s)) return;
      frag.appendChild(marketRowEl(s, baseIndex + i + 1));
    });
    grid.appendChild(frag);
  }

  // Full re-render from the accumulated items (no fetch) — used when an install
  // flips an item's library state and the grid must reflect it in place.
  function renderMarket() {
    const grid = $('#market-grid');
    const empty = $('#market-empty');
    if (!grid) return;
    grid.innerHTML = '';
    appendMarketBatch(marketState.items, 0);
    updateMarketCounts();
    if (empty) empty.hidden = grid.querySelector('.ml-row') != null;
    ensureMarketSentinel();
    updateMarketMore();
  }

  function updateMarketCounts() {
    const setCount = (id, n) => {
      const el = document.getElementById(id);
      if (el) el.textContent = ` ${n.toLocaleString()}`;
    };
    const libCount = marketState.items.filter((s) => s.in_my_library).length;
    const instCount = marketState.items.filter((s) => s.installed).length;
    setCount('mtab-count-all', marketState.total);
    setCount('mtab-count-trending', marketState.tab === 'trending' ? marketState.total : 0);
    setCount('mtab-count-hot', marketState.tab === 'hot' ? marketState.total : 0);
    setCount('mtab-count-library', libCount);
    setCount('mtab-count-installed', instCount);
  }

  // Infinite-scroll sentinel: a "load more" row at the bottom of the list that
  // auto-fetches the next page when it scrolls into view (also clickable).
  let marketObserver = null;
  function ensureMarketSentinel() {
    const pagerEl = $('#market-pager');
    if (!pagerEl) return;
    let more = $('#market-more');
    if (!more) {
      more = document.createElement('button');
      more.type = 'button';
      more.id = 'market-more';
      more.className = 'load-more';
      more.addEventListener('click', () => loadMarket(true));
      pagerEl.innerHTML = '';
      pagerEl.appendChild(more);
    }
    if (!marketObserver) {
      marketObserver = new IntersectionObserver((entries) => {
        if (entries.some((en) => en.isIntersecting)) loadMarket(true);
      }, { rootMargin: '400px' });
      marketObserver.observe(more);
    }
  }

  function updateMarketMore() {
    const more = $('#market-more');
    if (!more) return;
    if (marketState.done) {
      more.textContent = marketState.total > 0
        ? `已全部加载 · 共 ${marketState.total.toLocaleString()} 个`
        : '';
      more.classList.add('done');
    } else if (marketState.loading) {
      more.textContent = '加载中 …';
      more.classList.remove('done');
    } else {
      more.textContent = `加载更多 · 已显示 ${marketState.offset.toLocaleString()} / ${marketState.total.toLocaleString()}`;
      more.classList.remove('done');
    }
  }

