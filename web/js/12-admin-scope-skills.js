  // ------------------------------------------------------------------
  //  v15 admin user-management table (Admin pane)
  // ------------------------------------------------------------------
  async function loadAdminUsers() {
    if (!account.me || !account.me.is_admin) return;
    // PLANNING §1.1: owner mode is single-user self-serve — the user-
    // management table is hidden by CSS (13-owner-mode.css) and the
    // /api/admin/users data has nowhere to render. Skip the request so
    // we don't spam the server log with admin queries the UI ignores.
    if (account.me.mode === 'owner') return;
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
          body: `确定删除用户 ${username || userId}？将级联清理：该用户的私有 skill 移入垃圾桶（管理员可恢复，恢复后落公共池）、其物理目录与社区上传一并删除、我的库订阅清空、调用记录匿名化（router_events.user_id 置空，保留用于统计但不再可追溯）。账号本身不可恢复。`,
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
    // §1.3 prompt injection toggles — render in lockstep with the rest of
    // the per-user prefs section so login / logout flips them too.
    if (typeof renderPromptInjectionFlags === 'function') renderPromptInjectionFlags();
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

    // Count badges per scope — ownership-based (v0.11 model):
    //   公共库 = owner_user_id absent (公共池 ~/.runai/skills)
    //   私有库 = owner_user_id present (~/.runai/users/<uid>/skills)
    //   我的库 = 私有库 ∪ 已订阅公共 (private always counts as mine)
    const cache = skillsState.cache;
    const total = cache.length;
    const publicCount = cache.filter((s) => !s.owner_user_id).length;
    const privateCount = total - publicCount;
    const myCount = cache.filter(
      (s) => s.owner_user_id || account.libraryNames.has(s.name),
    ).length;
    $('#scope-all-count') && ($('#scope-all-count').textContent = total);
    $('#scope-my-count') && ($('#scope-my-count').textContent = myCount);
    $('#scope-public-count') && ($('#scope-public-count').textContent = publicCount);
    $('#scope-private-count') && ($('#scope-private-count').textContent = privateCount);

    document.querySelectorAll('.scope-btn').forEach((b) => {
      b.classList.toggle('active', b.dataset.scope === account.scope);
    });

    // Bulk-button visibility — only show the action that actually applies
    // to what's currently selected (avoids the "in 全部 scope you can hit
    // 移出我的库 even on skills that aren't in your library" surprise).
    //
    // Rules:
    //   - select mode off              → hide all bulk buttons
    //   - scope=private                → hide both (private is always "mine",
    //                                    public-library subscription N/A)
    //   - scope=my                     → only "移出我的库"
    //   - scope=public/all + sel empty → show both (user hasn't chosen yet)
    //   - scope=public/all + all-in    → only "移出我的库"
    //   - scope=public/all + all-out   → only "加入我的库"
    //   - scope=public/all + mixed     → show both, user picks intent
    const container = document.getElementById('skill-rows');
    const inSelectMode = container?.classList.contains('select-mode');
    let showAdd = false;
    let showRemove = false;
    if (inSelectMode) {
      if (account.scope === 'private') {
        // 私有库 rows are always "mine" — no public-library add/remove applies.
        showAdd = false;
        showRemove = false;
      } else if (account.scope === 'my') {
        showRemove = true;
      } else {
        // scope=all or 公共库 — both add/remove can apply (a public skill may
        // or may not be subscribed). Look at the current selection composition.
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
    // PLANNING §1.6 Model B C7c: admin-only batch trash. Visible only
    // when admin is in select-mode AND has at least one row selected.
    // CSS belt-and-braces hides it from non-admin / owner mode regardless.
    const showTrash = !!(
      account.me?.is_admin && inSelectMode && account.bulkSelect.size > 0
    );
    $('#bulk-trash')?.classList.toggle('hide', !showTrash);
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
      // Ownership-based scope (v0.11). `dataset.owner` is set by
      // renderSkillsRows: non-empty = private skill, empty = public pool.
      const isPrivate = !!row.dataset.owner;
      const inMy = isPrivate || account.libraryNames.has(name);
      let show = true;
      if (account.scope === 'my') show = inMy;
      else if (account.scope === 'public') show = !isPrivate;
      else if (account.scope === 'private') show = isPrivate;
      row.style.display = show ? '' : 'none';
    }
  }

  // ------------------------------------------------------------------
  //  PLANNING §1.6 Model B C6b — admin userlib sub-tab (browse only)
  // ------------------------------------------------------------------
  function fmtRelativeTime(tsSec) {
    if (!tsSec) return '—';
    const diff = (Date.now() / 1000) - tsSec;
    if (diff < 60) return '刚刚';
    if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
    if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
    if (diff < 2592000) return `${Math.floor(diff / 86400)} 天前`;
    const d = new Date(tsSec * 1000);
    return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  }

  async function loadAdminUserlib() {
    if (!account.me || !account.me.is_admin) return;
    const listEl = $('#userlib-list');
    const statusEl = $('#userlib-status');
    const countEl = $('#lib-sub-userlib-count');
    if (!listEl) return;
    statusEl.textContent = '加载中 …';
    listEl.innerHTML = '';
    try {
      const data = await api('GET', '/api/admin/userlib');
      if (countEl) countEl.textContent = data.total;
      statusEl.textContent = `共 ${data.total} 个非 admin 用户 · 排序: 最近活跃降`;
      const head = document.createElement('div');
      head.className = 'userlib-row head';
      head.innerHTML = '<div>用户名</div><div>私有</div><div>导入公共</div><div>上次活跃</div><div></div>';
      listEl.appendChild(head);
      for (const u of data.items) {
        const row = document.createElement('div');
        row.className = 'userlib-row data' + (u.disabled ? ' disabled' : '');
        row.dataset.uid = u.user_id;
        row.innerHTML = `
          <div class="uname">${escapeHTML(u.username)}${u.disabled ? '<span class="badge-disabled">DISABLED</span>' : ''}</div>
          <div class="num">${u.private_count}</div>
          <div class="num">${u.imported_count}</div>
          <div class="when">${escapeHTML(fmtRelativeTime(u.last_active_ts))}</div>
          <div class="open-arrow">›</div>
        `;
        row.addEventListener('click', () => openUserlibDetail(u.user_id));
        listEl.appendChild(row);
      }
      if (data.items.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'muted userlib-empty';
        empty.textContent = '还没有非 admin 用户注册';
        listEl.appendChild(empty);
      }
    } catch (e) {
      statusEl.textContent = `加载失败:${e.message}`;
    }
  }

  async function openUserlibDetail(userId) {
    const detail = $('#userlib-detail');
    const list = $('#userlib-list');
    const status = $('#userlib-status');
    if (!detail) return;
    list.classList.add('hide');
    status.classList.add('hide');
    detail.classList.remove('hide');
    detail.setAttribute('aria-hidden', 'false');
    $('#userlib-detail-name').textContent = '加载中 …';
    $('#userlib-detail-meta').textContent = '';
    try {
      const d = await api('GET', `/api/admin/userlib/${encodeURIComponent(userId)}`);
      $('#userlib-detail-name').textContent = d.username + (d.disabled ? ' (已禁用)' : '');
      $('#userlib-detail-meta').textContent = `${d.recent_events_count} 次调用 · 上次活跃 ${fmtRelativeTime(d.last_active_ts)}`;
      renderUserlibSection('private', d.private, userId);
      renderUserlibSection('imported', d.imported, userId);
    } catch (e) {
      $('#userlib-detail-name').textContent = '加载失败';
      $('#userlib-detail-meta').textContent = e.message;
    }
  }

  function renderUserlibSection(kind, items, userId) {
    const rowsEl = $(`#userlib-detail-${kind}`);
    const emptyEl = $(`#userlib-detail-${kind}-empty`);
    const countEl = $(`#userlib-detail-${kind}-count`);
    if (!rowsEl) return;
    rowsEl.innerHTML = '';
    if (countEl) countEl.textContent = items.length;
    if (items.length === 0) {
      if (emptyEl) emptyEl.hidden = false;
      return;
    }
    if (emptyEl) emptyEl.hidden = true;
    for (const s of items) {
      const row = document.createElement('div');
      row.className = 'userlib-skill-row clickable';
      row.innerHTML = `
        <div class="nm">${escapeHTML(s.name)}</div>
        <div class="desc" title="${escapeHTML(s.description || '')}">${escapeHTML(s.description || '—')}</div>
        <div class="used">${s.usage_count} 次</div>
      `;
      // Click → full skill detail page, same as 已安装. Carry ?owner=<uid>
      // so the (admin-gated) server resolves THIS user's skill — private
      // skills shadow public per owner, and same-named privates across users
      // would otherwise be ambiguous. Imported (public) rows resolve fine
      // with the owner param too (public falls through when no private match).
      row.addEventListener('click', () => {
        location.hash = `#/skill/${encodeURIComponent(s.name)}?owner=${encodeURIComponent(userId)}`;
      });
      rowsEl.appendChild(row);
    }
  }

  function closeUserlibDetail() {
    $('#userlib-detail')?.classList.add('hide');
    $('#userlib-list')?.classList.remove('hide');
    $('#userlib-status')?.classList.remove('hide');
  }

  // Sub-tab switcher between 「已安装」 and 「用户库」 panels.
  // Toggles ad-hoc instead of using a `.lib-pane` wrapper because the
  // installed view is the original markup that has no wrapper — adding
  // one would shift every related selector. The element list is the
  // authoritative set of "installed view" chrome that has to flip.
  const INSTALLED_VIEW_SELECTORS = [
    '#library-scope-bar',
    '.library-filter',
    '.skill-rows-head',
    '#skill-rows',
    '#skills-empty',
  ];

  function switchLibrarySubTab(sub) {
    document.querySelectorAll('.lib-sub').forEach((a) => {
      a.classList.toggle('active', a.dataset.sub === sub);
    });
    const userlibPanel = $('#userlib-panel');
    if (sub === 'userlib') {
      INSTALLED_VIEW_SELECTORS.forEach((s) =>
        document.querySelectorAll(s).forEach((e) => e.classList.add('hide')),
      );
      userlibPanel?.classList.remove('hide');
      userlibPanel?.setAttribute('aria-hidden', 'false');
      closeUserlibDetail();
      loadAdminUserlib();
    } else {
      INSTALLED_VIEW_SELECTORS.forEach((s) =>
        document.querySelectorAll(s).forEach((e) => e.classList.remove('hide')),
      );
      userlibPanel?.classList.add('hide');
      userlibPanel?.setAttribute('aria-hidden', 'true');
    }
  }

  function bindLibrarySubTabs() {
    document.querySelectorAll('.lib-sub[data-sub]').forEach((a) => {
      a.addEventListener('click', (ev) => {
        if (a.dataset.sub === 'userlib') {
          ev.preventDefault();
          switchLibrarySubTab('userlib');
        } else if (a.dataset.sub === 'installed') {
          switchLibrarySubTab('installed');
        }
      });
    });
    $('#userlib-back')?.addEventListener('click', closeUserlibDetail);
  }

