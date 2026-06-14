  // ------------------------------------------------------------------
  //  v15 multi-user: account / library / per-user prefs
  // ------------------------------------------------------------------
  // In-memory mirror of /api/me + /api/skills/library, refreshed on
  // login, logout, mutate, and dashboard boot. Powers the account pill,
  // Settings 我的偏好 toggle, and the skills tab scope filter.
  const account = {
    me: null,             // { user_id, username, is_admin, library_size } or null
    libraryNames: new Set(), // names currently in user's library
    scope: 'all',         // 'all' | 'my' | 'public'
    bulkSelect: new Set(),
    prefs: null,          // UserPrefs from /api/prefs
  };

  // localStorage-backed api_key fallback. Cookies expire / get cleared
  // by browser hygiene tools; the bearer survives across browser
  // restarts so the dashboard stays signed in.
  const RUNAI_KEY_STORE = 'runai_api_key';
  function getStoredApiKey() {
    try { return localStorage.getItem(RUNAI_KEY_STORE) || ''; } catch (_) { return ''; }
  }
  function setStoredApiKey(key) {
    try {
      if (key) localStorage.setItem(RUNAI_KEY_STORE, key);
      else localStorage.removeItem(RUNAI_KEY_STORE);
    } catch (_) {}
  }

  async function api(method, path, body) {
    const headers = {};
    const key = getStoredApiKey();
    if (key) headers['Authorization'] = `Bearer ${key}`;
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    const opts = { method, credentials: 'same-origin', headers };
    if (body !== undefined) opts.body = JSON.stringify(body);
    const res = await fetch(path, opts);
    if (!res.ok) {
      let detail = '';
      try { detail = (await res.json()).error || ''; } catch (_) {}
      const err = new Error(detail || `HTTP ${res.status}`);
      err.status = res.status;
      throw err;
    }
    if (res.status === 204) return null;
    const ct = res.headers.get('content-type') || '';
    return ct.includes('json') ? res.json() : res.text();
  }

  async function refreshMe() {
    try {
      account.me = await api('GET', '/api/me');
    } catch (e) {
      account.me = null;
    }
    // PLANNING §1.1: keep the `mode-owner` body class in sync with the
    // runtime mode the server reports via /api/me. serve_index injects
    // it server-side on first paint; this re-applies it after every SPA
    // refreshMe so a swap to/from owner mode (e.g. after a server
    // restart with a different --mode flag) updates the visible chrome
    // without a hard reload.
    if (account.me && account.me.mode === 'owner') {
      document.body.classList.add('mode-owner');
    } else {
      document.body.classList.remove('mode-owner');
    }
    // PLANNING §1.6 model B: sync `is-admin` body class so CSS can hide
    // chrome that's meaningless to an admin (the per-user scope segment
    // 全部 / 我的库 / 仅公共 collapses to one number for admin — the
    // public pool — so the segmentation is just visual noise).
    if (account.me && account.me.is_admin) {
      document.body.classList.add('is-admin');
    } else {
      document.body.classList.remove('is-admin');
    }
    renderAccountPill();
    await refreshLibraryNames();
    await refreshPrefs();
    renderSettingsUser();
    renderScopeBar();
    // PLANNING §1.6 Model B: prime the admin userlib sub-tab count so it
    // reads "用户库 N" before the admin first clicks the tab. Skip in
    // owner mode (synthetic owner is admin but the sub-tab is hidden by
    // CSS) so we don't fire a wasted /api/admin/userlib request.
    if (account.me && account.me.is_admin && account.me.mode !== 'owner') {
      if (typeof loadAdminUserlib === 'function') loadAdminUserlib();
    }
    // re-render rows so the in-library "●" indicator + selection state
    // reflect the freshly-loaded library set.
    if (typeof renderSkillsRows === 'function' && skillsState.cache.length) {
      renderSkillsRows();
    }
    renderSkills();
  }

  async function refreshLibraryNames() {
    account.libraryNames.clear();
    if (!account.me) return;
    try {
      const data = await api('GET', '/api/skills/library');
      for (const e of (data.items || [])) account.libraryNames.add(e.name);
    } catch (_) {}
  }

  async function refreshPrefs() {
    if (!account.me) { account.prefs = null; return; }
    try {
      account.prefs = await api('GET', '/api/prefs');
    } catch (_) { account.prefs = null; }
  }

  function renderAccountPill() {
    const loginBtn = $('#account-login-btn');
    const info = $('#account-info');
    const name = $('#account-name');
    if (!loginBtn || !info || !name) return;
    if (account.me) {
      loginBtn.classList.add('hide');
      info.classList.remove('hide');
      name.textContent = account.me.username + (account.me.is_admin ? ' (admin)' : '');
    } else {
      loginBtn.classList.remove('hide');
      info.classList.add('hide');
      name.textContent = '—';
    }
    // Toggle the admin nav-item + the "前往 Admin" jump button in Settings.
    const adminNav = document.querySelector('.nav-admin');
    if (adminNav) adminNav.classList.toggle('hide', !(account.me && account.me.is_admin));
    const jumpBtn = $('#settings-admin-jump');
    if (jumpBtn) {
      jumpBtn.style.display = (account.me && account.me.is_admin) ? '' : 'none';
    }
  }

