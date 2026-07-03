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
      // /users/register still returns an api_key; /auth/login no longer
      // does (issue #35 — a dashboard login mints a session cookie only
      // and never rotates the hook's key). setStoredApiKey is a no-op
      // stub; this stays for the register path's shape only.
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
      if (mode === 'register') {
        err.textContent = e.message || '注册失败';
      } else if (e.status === 401) {
        // Server replies with a uniform `invalid_credentials` for both
        // "user doesn't exist" and "wrong password" (anti-enumeration,
        // PLANNING §2.3 item 5) — keep that same non-distinguishing
        // behavior here instead of surfacing the raw e.message.
        err.textContent = '用户名或密码错误，请重试；忘记密码请联系管理员重置';
      } else {
        err.textContent = e.message || '登录失败';
      }
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

  // E1: rotate the api_key server-side (invalidates every existing copy —
  // other browsers, proxies, the stored ~/.runai-identity) then clear this
  // browser. Unlike plain logout this truly ends all sessions.
  async function doLogoutEverywhere() {
    const ok = await showConfirm({
      title: '全端退出',
      body: '将轮换你的 api_key：所有已登录设备、已安装的 hook 客户端会立即失效，需要重新登录 / 重装客户端。继续？',
      ok: '全端退出',
      cancel: '取消',
      danger: true,
    });
    if (!ok) return;
    try { await api('POST', '/api/me/logout-everywhere'); } catch (_) {}
    setStoredApiKey('');
    account.me = null;
    account.libraryNames.clear();
    account.prefs = null;
    renderAccountPill();
    renderSettingsUser();
    renderScopeBar();
    renderSkills();
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
    if (kind === 'trash') {
      // PLANNING §1.6 Model B C7c: admin-only batch trash public-pool
      // skills via POST /api/admin/skills/trash. The button only appears
      // when admin + select-mode + selection exists (renderScopeBar
      // gate), so we just confirm + fire here.
      if (!account.me?.is_admin) return;
      const names = Array.from(account.bulkSelect);
      if (names.length === 0) {
        alert('请先选中要操作的 skill');
        return;
      }
      const ok = await showConfirm({
        title: '移到垃圾桶',
        body: `把选中的 ${names.length} 个公共 skill 移到垃圾桶。不是 hard delete,可在垃圾桶视图 restore。继续?`,
        ok: '移到垃圾桶',
        cancel: '取消',
        danger: true,
      });
      if (!ok) return;
      try {
        const res = await api('POST', '/api/admin/skills/trash', { names });
        account.bulkSelect.clear();
        if (typeof loadSkills === 'function') await loadSkills();
        renderScopeBar();
        renderSkills();
        if (res.failed && res.failed.length > 0) {
          alert(`已移到垃圾桶 ${res.trashed} 个,${res.failed.length} 个失败:\n` + res.failed.join('\n'));
        }
      } catch (e) {
        alert('批量删除失败: ' + e.message);
      }
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
    $('#account-logout-all-btn')?.addEventListener('click', doLogoutEverywhere);
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

