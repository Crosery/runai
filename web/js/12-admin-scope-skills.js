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

