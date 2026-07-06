  // ------------------------------------------------------------------
  //  Settings tab — recommend config + providers CRUD
  // ------------------------------------------------------------------
  let lastSettings = null;

  // §1.3 prompt injection toggles — one row per toggleable prompt name
  // (`TOGGLEABLE_PROMPT_NAMES` in src/core/prompts/mod.rs). We hard-code
  // the descriptors here because there is no server endpoint that lists
  // them yet; adding a new toggleable prompt = adding a row here + a
  // const + a TOGGLEABLE_PROMPT_NAMES entry on the server side.
  const PROMPT_INJECTION_TOGGLES = [
    {
      key: 'recommend_history_prefix',
      title: '历史 transcript 注入',
      hint: '把最近若干条 user/assistant 对话喂给路由 LLM，用作"我刚问过什么"的上下文',
    },
    {
      key: 'recommend_already_routed',
      title: '本 session 已激活技能列表',
      hint: '告诉路由器本次会话内已经命中过哪些 skill，避免重复推荐',
    },
    {
      key: 'recommend_cwd_prefix',
      title: '当前 cwd 路径注入',
      hint: '把 Claude Code 的工作目录路径写进 prompt，帮路由器猜场景',
    },
    {
      key: 'recommend_project_context',
      title: 'CLAUDE.md 项目上下文',
      hint: '读 cwd 的 CLAUDE.md（及其 @ 引用）作为长上下文；与"读取 CLAUDE.md"开关共同生效',
    },
  ];

  function renderPromptInjectionFlags() {
    const wrap = document.getElementById('prefs-flags-list');
    if (!wrap) return;
    const loggedIn = !!(typeof account !== 'undefined' && account && account.me);
    const flags = (loggedIn && account.prefs && account.prefs.prompt_injection_flags) || {};
    if (!loggedIn) {
      wrap.innerHTML = '<div class="muted prefs-flags-empty">登录后即可调整每个提示词的注入开关。</div>';
      return;
    }
    wrap.innerHTML = PROMPT_INJECTION_TOGGLES.map((t) => {
      // Default true when missing — matches UserPrefs::prompt_injection_enabled.
      const on = flags[t.key] === undefined ? true : !!flags[t.key];
      return `
        <div class="setting-row" data-pref-flag="${escapeHTML(t.key)}">
          <div class="setting-meta">
            <div class="setting-title">${escapeHTML(t.title)} <code class="pref-flag-key">${escapeHTML(t.key)}</code></div>
            <div class="setting-hint">${escapeHTML(t.hint)}</div>
          </div>
          <label class="switch">
            <input type="checkbox" data-pref-flag-input="${escapeHTML(t.key)}" ${on ? 'checked' : ''}>
            <span></span>
          </label>
        </div>
      `;
    }).join('');
    wrap.querySelectorAll('input[data-pref-flag-input]').forEach((el) => {
      el.addEventListener('change', () => onPromptInjectionFlagToggle(el));
    });
  }

  async function onPromptInjectionFlagToggle(el) {
    if (!account.me || !account.prefs) return;
    const key = el.dataset.prefFlagInput;
    if (!key) return;
    if (!account.prefs.prompt_injection_flags) {
      account.prefs.prompt_injection_flags = {};
    }
    account.prefs.prompt_injection_flags[key] = !!el.checked;
    const saved = await savePrefs();
    if (!saved) {
      // savePrefs already re-rendered the user section; sync flags too so
      // the DOM matches DB truth on failure.
      renderPromptInjectionFlags();
    }
  }
  let providerEditingId = null;       // null = adding new; "<id>" = editing
  let provKindValue = 'openai-compat';
  let provKindDdInitialized = false;
  const providerTestState = new Map();
  const providerBusyIds = new Set();

  function setProviderStatus(message, kind = 'muted') {
    const el = document.getElementById('provider-status');
    if (!el) return;
    el.textContent = message || '';
    el.className = `provider-status ${kind}`;
    el.hidden = !message;
  }

  function setProviderRowBusy(id, busy) {
    if (busy) providerBusyIds.add(id);
    else providerBusyIds.delete(id);
    document.querySelectorAll('button[data-id]').forEach((b) => {
      if (b.dataset.id !== id) return;
      if (b.classList.contains('prov-test') || b.classList.contains('prov-edit') || b.classList.contains('prov-del')) {
        b.disabled = busy;
      }
    });
    document.querySelectorAll('input[name="active-provider"]').forEach((radio) => {
      if (radio.value === id) radio.disabled = busy;
    });
  }

  function setProviderKindDropdown(value) {
    provKindValue = value;
    const dd = document.getElementById('prov-kind-dd');
    if (!dd) return;
    const label = dd.querySelector('.dd-label');
    dd.querySelectorAll('.dd-opt').forEach((opt) => {
      const isActive = opt.dataset.value === value;
      opt.classList.toggle('active', isActive);
      if (isActive && label) label.textContent = opt.textContent.trim();
    });
  }

  function initProviderKindDropdown() {
    if (provKindDdInitialized) return;
    const dd = document.getElementById('prov-kind-dd');
    if (!dd) return;
    initDropdown(dd, (val) => { provKindValue = val; });
    provKindDdInitialized = true;
  }

  async function loadSettings() {
    try {
      const data = await api('GET', '/api/settings');
      lastSettings = data;
      renderSettings(data);
    } catch (e) {
      setProviderStatus(`加载设置失败：${e.message}`, 'error');
    }
  }

  function renderSettings(data) {
    const globalEnabled = $('#set-global-enabled');
    if (globalEnabled) globalEnabled.checked = !!data.enabled;
    const activeLabel = (data.providers || [])
      .find((p) => p.id === data.active_provider_id);
    $('#set-active-label').textContent = activeLabel
      ? `${activeLabel.label} · ${activeLabel.model}`
      : '未选择';
    const globalHint = $('#set-global-enabled-hint');
    if (globalHint) {
      globalHint.textContent = data.enabled
        ? '全局已开启；当前账号还需在 Settings 里开启自己的 recommend'
        : '全局已关闭；所有账号的 recommend 都不会请求模型';
    }
    renderProvidersList(data);
  }

  function renderProvidersList(data) {
    const wrap = $('#providers-list');
    wrap.innerHTML = '';
    if (!data.providers || data.providers.length === 0) {
      wrap.innerHTML = '<div class="muted provider-empty">还没添加运营商 — 点下方按钮加一个</div>';
      return;
    }
    for (const p of data.providers) {
      const row = document.createElement('div');
      row.className = 'provider-row' + (p.id === data.active_provider_id ? ' active' : '');
      const lastTest = providerTestState.get(p.id);
      let testHtml = '<span class="prov-test-state muted">未测试</span>';
      if (lastTest) {
        const klass = lastTest.ok ? 'ok' : 'error';
        testHtml = `<span class="prov-test-state ${klass}">${escapeHTML(lastTest.label)}</span>`;
      }
      row.innerHTML = `
        <div class="prov-pick">
          <label class="radio" title="设为 active provider">
            <input type="radio" name="active-provider" value="${escapeHTML(p.id)}"
                   ${p.id === data.active_provider_id ? 'checked' : ''}>
            <span></span>
          </label>
        </div>
        <div class="prov-id-col">
          <div class="prov-label">${escapeHTML(p.label || p.id)}</div>
          <div class="prov-meta"><span class="kind">${escapeHTML(p.kind)}</span> · ${escapeHTML(p.model || '')}</div>
          <div class="prov-base">${escapeHTML(p.base_url || '')}</div>
          <div class="prov-test-line">${testHtml}</div>
        </div>
        <div class="prov-keystate">
          ${p.kind === 'claude-cli' ? '<span class="key-set">无需 key</span>' : (p.has_api_key ? '<span class="key-set">key 已配置</span>' : '<span class="key-empty">无 key</span>')}
        </div>
        <div class="prov-actions">
          <button type="button" class="btn prov-test" data-id="${escapeHTML(p.id)}">测试</button>
          <button type="button" class="btn prov-edit" data-id="${escapeHTML(p.id)}">编辑</button>
          <button type="button" class="btn prov-del" data-id="${escapeHTML(p.id)}">删除</button>
        </div>
      `;
      wrap.appendChild(row);
      if (providerBusyIds.has(p.id)) setProviderRowBusy(p.id, true);
    }
    wrap.querySelectorAll('input[name="active-provider"]').forEach((el) => {
      el.addEventListener('change', () => activateProvider(el.value));
    });
    wrap.querySelectorAll('.prov-test').forEach((b) => {
      b.addEventListener('click', () => testProvider(b.dataset.id));
    });
    wrap.querySelectorAll('.prov-edit').forEach((b) => {
      b.addEventListener('click', () => openProviderForm(b.dataset.id));
    });
    wrap.querySelectorAll('.prov-del').forEach((b) => {
      b.addEventListener('click', () => deleteProvider(b.dataset.id));
    });
  }

  async function patchSettings(patch) {
    try {
      const data = await api('POST', '/api/settings', patch);
      lastSettings = data;
      renderSettings(data);
      setProviderStatus('全局设置已保存', 'ok');
    } catch (e) {
      setProviderStatus(`保存全局设置失败：${e.message}`, 'error');
    }
  }

  async function activateProvider(id) {
    if (!id) return;
    setProviderRowBusy(id, true);
    setProviderStatus(`正在切换 active provider：${id}`, 'muted');
    try {
      const data = await api('POST', `/api/providers/${encodeURIComponent(id)}/activate`);
      lastSettings = data;
      renderSettings(data);
      setProviderStatus(`已切换 active provider：${id}`, 'ok');
    } catch (e) {
      setProviderStatus(`切换失败：${e.message}`, 'error');
      renderSettings(lastSettings || { providers: [] });
    }
  }

  async function testProvider(id) {
    if (!id) return;
    setProviderRowBusy(id, true);
    providerTestState.set(id, { ok: true, label: '测试中 …' });
    renderSettings(lastSettings || { providers: [] });
    setProviderStatus(`正在请求模型：${id}`, 'muted');
    try {
      const data = await api('POST', `/api/providers/${encodeURIComponent(id)}/test`);
      if (data.ok) {
        const reply = data.reply ? ` · ${data.reply.slice(0, 80)}` : '';
        providerTestState.set(id, { ok: true, label: `可请求 · ${fmtMsDur(data.latency_ms)}${reply}` });
        setProviderStatus(`模型请求成功：${data.model} · ${fmtMsDur(data.latency_ms)}`, 'ok');
      } else {
        const msg = data.error || 'unknown error';
        providerTestState.set(id, { ok: false, label: `不可用 · ${msg.slice(0, 140)}` });
        setProviderStatus(`模型请求失败：${msg}`, 'error');
      }
    } catch (e) {
      providerTestState.set(id, { ok: false, label: `测试接口失败 · ${e.message}` });
      setProviderStatus(`测试接口失败：${e.message}`, 'error');
    } finally {
      setProviderRowBusy(id, false);
      renderSettings(lastSettings || { providers: [] });
    }
  }

  async function deleteProvider(id) {
    const ok = await showConfirm({
      title: '删除运营商',
      body: `删除运营商 ${id}？此操作只清除 runai 配置，不影响远程账号。`,
      ok: '删除',
      cancel: '取消',
      danger: true,
    });
    if (!ok) return;
    setProviderRowBusy(id, true);
    try {
      const data = await api('DELETE', `/api/providers/${encodeURIComponent(id)}`);
      providerTestState.delete(id);
      lastSettings = data;
      renderSettings(data);
      setProviderStatus(`已删除 provider：${id}`, 'ok');
    } catch (e) {
      setProviderStatus(`删除失败：${e.message}`, 'error');
      setProviderRowBusy(id, false);
    }
  }

  function openProviderForm(id) {
    providerEditingId = id || null;
    initProviderKindDropdown();
    const form = $('#provider-form');
    const title = $('#provider-form-title');
    if (id && lastSettings) {
      const p = (lastSettings.providers || []).find((x) => x.id === id);
      if (!p) return;
      title.textContent = `编辑 ${p.label || p.id}`;
      $('#prov-id').value = p.id;
      $('#prov-id').disabled = true;
      $('#prov-label').value = p.label || '';
      setProviderKindDropdown(p.kind || 'openai-compat');
      $('#prov-model').value = p.model || '';
      $('#prov-base-url').value = p.base_url || '';
      $('#prov-api-key').value = '';
      $('#prov-api-key').placeholder = p.has_api_key
        ? 'sk-... (留空保留已存的 key)'
        : 'sk-...';
    } else {
      title.textContent = '添加运营商';
      $('#prov-id').value = '';
      $('#prov-id').disabled = false;
      $('#prov-label').value = '';
      setProviderKindDropdown('openai-compat');
      $('#prov-model').value = '';
      $('#prov-base-url').value = '';
      $('#prov-api-key').value = '';
      $('#prov-api-key').placeholder = 'sk-...';
    }
    form.hidden = false;
    form.scrollIntoView({ behavior: 'smooth', block: 'center' });
  }

  function closeProviderForm() {
    $('#provider-form').hidden = true;
    providerEditingId = null;
  }

  async function submitProviderForm() {
    const id = $('#prov-id').value.trim();
    if (!id) { alert('运营商 ID 必填'); return; }
    const body = {
      id,
      label: $('#prov-label').value.trim() || id,
      kind: provKindValue || 'openai-compat',
      model: $('#prov-model').value.trim(),
      base_url: $('#prov-base-url').value.trim(),
      api_key: $('#prov-api-key').value,    // empty = preserve existing
    };
    try {
      const data = await api('POST', '/api/providers', body);
      providerTestState.delete(id);
      lastSettings = data;
      renderSettings(data);
      closeProviderForm();
      setProviderStatus(`Provider 已保存：${id}`, 'ok');
    } catch (e) {
      setProviderStatus(`保存失败：${e.message || e}`, 'error');
    }
  }

  function bindSettingsControls() {
    // v15 multi-user: the four hook-behavior toggles + textarea moved
    // from /api/settings (global, admin-only) to /api/prefs (per-user).
    // Wire them through savePrefs() instead of patchSettings().
    const wirePref = (id, key, kind) => {
      const el = document.getElementById(id);
      if (!el) return;
      const event = kind === 'textarea' ? 'blur' : 'change';
      el.addEventListener(event, () => {
        if (!account.me || !account.prefs) return;
        let v;
        if (kind === 'checkbox') {
          v = el.checked;
        } else if (kind === 'number') {
          v = Number.parseInt(el.value, 10);
          if (!Number.isFinite(v)) return;
        } else {
          v = el.value;
        }
        account.prefs[key] = v;
        savePrefs();
      });
    };
    wirePref('set-enabled', 'recommend_enabled', 'checkbox');
    wirePref('set-read-claude-md', 'read_claude_md', 'checkbox');
    wirePref('set-intent-memory-enabled', 'intent_memory_enabled', 'checkbox');
    wirePref('set-intent-memory-limit', 'intent_memory_limit', 'number');
    wirePref('set-bm25-candidate-limit', 'bm25_candidate_limit', 'number');
    wirePref('set-skip-reminder', 'skip_reminder_enabled', 'checkbox');
    wirePref('set-skip-reminder-template', 'skip_reminder_template', 'textarea');
    const globalEnabled = document.getElementById('set-global-enabled');
    if (globalEnabled) {
      globalEnabled.addEventListener('change', () => patchSettings({ enabled: globalEnabled.checked }));
    }
    const addBtn = document.getElementById('provider-add-btn');
    if (addBtn) addBtn.addEventListener('click', () => openProviderForm(null));
    const saveBtn = document.getElementById('provider-save-btn');
    if (saveBtn) saveBtn.addEventListener('click', submitProviderForm);
    const cancelBtn = document.getElementById('provider-cancel-btn');
    if (cancelBtn) cancelBtn.addEventListener('click', closeProviderForm);
  }

