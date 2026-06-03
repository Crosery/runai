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

