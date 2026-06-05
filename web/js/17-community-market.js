  // ------------------------------------------------------------------
  //  §1.4 Community market — team-mode user-uploaded skill pool
  //
  //  Renders the "社区" sub-tab of the Market view. Talks exclusively to
  //  /api/community/* and reuses the same in-memory `account` from
  //  11-account-library.js for login state.
  //
  //  Owner-mode servers 404 every /api/community/* route — that's the
  //  contract enforced by server::community::require_team_mode. The
  //  pane stays usable in owner mode (it'll just show "—" counts and
  //  "服务端不可用" messages), so we don't gate the source tab itself.
  // ------------------------------------------------------------------
  const communityState = {
    items: [],
    sort: 'installs',     // 'installs' | 'created' | 'name'
    offset: 0,
    limit: 50,
    total: 0,
    activeSource: 'skillshub', // 'skillshub' | 'community'
    detail: null,         // currently-open detail row { uploader_uid, name, ... }
  };

  function setMarketSource(src) {
    communityState.activeSource = src;
    const hub = document.getElementById('market-skillshub-pane');
    const com = document.getElementById('market-community-pane');
    if (hub) hub.classList.toggle('hide', src !== 'skillshub');
    if (com) com.classList.toggle('hide', src !== 'community');
    document.querySelectorAll('.market-source-tab-btn').forEach((b) => {
      b.classList.toggle('active', b.dataset.mtsource === src);
    });
    if (src === 'community') loadCommunityMarket();
  }

  async function loadCommunityMarket() {
    const rows = document.getElementById('community-rows');
    const empty = document.getElementById('community-empty');
    const count = document.getElementById('community-count');
    if (!rows) return;
    rows.innerHTML = '<div class="muted" style="padding:18px">加载中 …</div>';
    if (empty) empty.hidden = true;
    try {
      const params = new URLSearchParams({
        sort: communityState.sort,
        offset: String(communityState.offset),
        limit: String(communityState.limit),
      });
      const data = await api('GET', `/api/community/list?${params}`);
      communityState.items = data.items || [];
      communityState.total = data.total || 0;
      communityState.offset = data.offset || 0;
      communityState.limit = data.limit || communityState.limit;
      renderCommunityMarket();
      if (count) count.textContent = ` ${communityState.total.toLocaleString()} skill`;
    } catch (e) {
      // 404 → owner-mode server with no community route. Render an
      // explanatory empty state instead of a noisy error stripe.
      if (e.status === 404) {
        rows.innerHTML = '';
        if (empty) {
          empty.hidden = false;
          empty.textContent = '该 server 运行在 owner 模式 — 社区池在 team 模式下才可用。';
        }
        if (count) count.textContent = '';
      } else if (e.status === 401) {
        rows.innerHTML = `<div class="muted" style="padding:18px">登录后即可浏览社区池。</div>`;
        if (count) count.textContent = '';
      } else {
        rows.innerHTML = `<div class="muted" style="padding:18px">加载失败：${escapeHTML(e.message)}</div>`;
      }
    }
  }

  function renderCommunityMarket() {
    const rows = document.getElementById('community-rows');
    const empty = document.getElementById('community-empty');
    if (!rows) return;
    rows.innerHTML = '';
    if (communityState.items.length === 0) {
      if (empty) {
        empty.hidden = false;
        empty.textContent = '社区池空 — 点上方按钮上传第一个 skill。';
      }
      renderCommunityPager();
      return;
    }
    if (empty) empty.hidden = true;
    const me = (typeof account !== 'undefined' && account && account.me) || null;
    const frag = document.createDocumentFragment();
    communityState.items.forEach((s, idx) => {
      const rank = communityState.offset + idx + 1;
      const row = document.createElement('div');
      row.className = 'cm-row';
      row.dataset.uploader = s.uploader_uid;
      row.dataset.name = s.name;
      const mine = me && me.user_id === s.uploader_uid;
      const installLabel = '安装到我的库';
      row.innerHTML = `
        <div class="cm-rank">${rank}</div>
        <div class="cm-skill">
          <div class="cm-skill-name">${escapeHTML(s.name)}</div>
          <div class="cm-skill-uploader">uploader · ${escapeHTML(s.uploader_uid.slice(0, 12))}${mine ? ' <em style="color:var(--accent)">(我)</em>' : ''}</div>
        </div>
        <div class="cm-version-text">${escapeHTML(s.version)}</div>
        <div class="cm-installs-text">${Number(s.installs_total || 0).toLocaleString()}</div>
        <div class="cm-action">
          <button type="button" class="btn-small cm-install-btn" data-act="install">${installLabel}</button>
        </div>
      `;
      row.addEventListener('click', (ev) => {
        if (ev.target.closest('button')) return;
        openCommunityDetail(s);
      });
      frag.appendChild(row);
    });
    rows.appendChild(frag);
    rows.querySelectorAll('button[data-act="install"]').forEach((b) => {
      b.addEventListener('click', (ev) => {
        ev.stopPropagation();
        const r = b.closest('.cm-row');
        installFromCommunity(r.dataset.uploader, r.dataset.name, b);
      });
    });
    renderCommunityPager();
  }

  function renderCommunityPager() {
    const pager = document.getElementById('community-pager');
    if (!pager) return;
    const { offset, limit, total } = communityState;
    if (total <= limit) {
      pager.innerHTML = '';
      return;
    }
    const page = Math.floor(offset / limit) + 1;
    const pages = Math.max(1, Math.ceil(total / limit));
    const hasPrev = offset > 0;
    const hasNext = offset + limit < total;
    pager.innerHTML = `
      <button type="button" class="cm-pager-btn" id="community-pager-prev" ${hasPrev ? '' : 'disabled'}>← 上一页</button>
      <span>第 ${page} / ${pages} 页 · ${(offset + 1).toLocaleString()}–${Math.min(offset + limit, total).toLocaleString()} / ${total.toLocaleString()}</span>
      <button type="button" class="cm-pager-btn" id="community-pager-next" ${hasNext ? '' : 'disabled'}>下一页 →</button>
    `;
    const pageCM = (newOffset) => {
      communityState.offset = newOffset;
      const y = window.scrollY;
      loadCommunityMarket().finally(() => window.scrollTo({ top: y }));
    };
    pager.querySelector('#community-pager-prev')?.addEventListener('click',
      () => pageCM(Math.max(0, offset - limit)));
    pager.querySelector('#community-pager-next')?.addEventListener('click',
      () => pageCM(offset + limit));
  }

  async function openCommunityDetail(row) {
    communityState.detail = row;
    const modal = document.getElementById('community-detail-modal');
    if (!modal) return;
    modal.classList.remove('hide');
    document.getElementById('community-detail-name').textContent = row.name;
    document.getElementById('community-detail-uploader').textContent =
      `uploader · ${row.uploader_uid}`;
    document.getElementById('community-detail-version').textContent =
      `version ${row.version} · ${Number(row.installs_total || 0).toLocaleString()} installs`;
    const md = document.getElementById('community-detail-md');
    if (md) md.textContent = 'SKILL.md 加载中 …';
    // Show delete button if current user is uploader or admin.
    const me = (typeof account !== 'undefined' && account && account.me) || null;
    const delBtn = document.getElementById('community-detail-delete');
    if (delBtn) {
      const canDelete = me && (me.user_id === row.uploader_uid || me.is_admin);
      delBtn.classList.toggle('hide', !canDelete);
    }
    try {
      const data = await api('GET',
        `/api/community/skill/${encodeURIComponent(row.uploader_uid)}/${encodeURIComponent(row.name)}`);
      if (md) md.textContent = data.readme || '(no SKILL.md)';
    } catch (e) {
      if (md) md.textContent = `加载失败：${e.message}`;
    }
  }

  function closeCommunityDetail() {
    const modal = document.getElementById('community-detail-modal');
    if (modal) modal.classList.add('hide');
    communityState.detail = null;
  }

  async function installFromCommunity(uid, name, btn) {
    if (btn) { btn.disabled = true; btn.textContent = '安装中 …'; }
    try {
      const data = await api('POST',
        `/api/community/install/${encodeURIComponent(uid)}/${encodeURIComponent(name)}`);
      if (btn) btn.textContent = '已装到私有池';
      // Refresh library in main app so the skills tab reflects the new
      // entry without a manual reload.
      if (typeof refreshLibraryNames === 'function') await refreshLibraryNames();
      if (typeof renderSkills === 'function') renderSkills();
      // Bump the row's install count locally for instant feedback.
      const r = communityState.items.find((x) => x.uploader_uid === uid && x.name === name);
      if (r) r.installs_total = (r.installs_total || 0) + 1;
    } catch (e) {
      if (btn) { btn.disabled = false; btn.textContent = '安装到我的库'; }
      alert('安装失败：' + e.message);
    }
  }

  async function deleteCommunitySkill() {
    const row = communityState.detail;
    if (!row) return;
    if (!confirm(`删除 ${row.name}？这会从社区池物理删除（已装到他人库的副本不会动）。`)) return;
    try {
      await api('DELETE',
        `/api/community/skill/${encodeURIComponent(row.uploader_uid)}/${encodeURIComponent(row.name)}`);
      closeCommunityDetail();
      await loadCommunityMarket();
    } catch (e) {
      alert('删除失败：' + e.message);
    }
  }

  // Dashboard intentionally has no upload UI. Uploads go through:
  //   - `runai community upload --path <dir> --name <skill>` (local CLI, agent-friendly)
  //   - runai TUI Community tab: scans `~/.claude/skills/` + cwd `.claude/skills/`,
  //     pick + upload interactively
  //   - remote bash `runai-client upload` (clients without runai binary)
  // The POST /api/community/upload endpoint is unchanged; only the browser UI is gone.

  function bindCommunityUI() {
    document.querySelectorAll('.market-source-tab-btn').forEach((btn) => {
      btn.addEventListener('click', () => setMarketSource(btn.dataset.mtsource || 'skillshub'));
    });
    document.querySelectorAll('.cm-sort-btn').forEach((btn) => {
      btn.addEventListener('click', () => {
        const next = btn.dataset.cmsort || 'installs';
        if (communityState.sort === next) return;
        communityState.sort = next;
        communityState.offset = 0;
        document.querySelectorAll('.cm-sort-btn').forEach((b) =>
          b.classList.toggle('active', b === btn));
        loadCommunityMarket();
      });
    });
    document.getElementById('community-refresh-btn')?.addEventListener('click',
      () => loadCommunityMarket());
    document.getElementById('community-detail-close')?.addEventListener('click',
      closeCommunityDetail);
    document.getElementById('community-detail-dismiss')?.addEventListener('click',
      closeCommunityDetail);
    document.getElementById('community-detail-delete')?.addEventListener('click',
      deleteCommunitySkill);
    document.getElementById('community-detail-install')?.addEventListener('click', () => {
      const r = communityState.detail;
      if (r) installFromCommunity(r.uploader_uid, r.name, null);
    });
  }
