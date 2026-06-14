  // ------------------------------------------------------------------
  //  PLANNING §1.4 C9g — admin reviews pending publish-requests
  // ------------------------------------------------------------------
  async function loadAdminPublishRequests() {
    if (!account.me || !account.me.is_admin) return;
    const rowsEl = $('#admin-publish-rows');
    const countEl = $('#admin-publish-count');
    const emptyEl = $('#admin-publish-empty');
    if (!rowsEl) return;
    try {
      const data = await api('GET', '/api/admin/publish-requests');
      if (countEl) countEl.textContent = data.total;
      rowsEl.innerHTML = '';
      if (!data.items || data.items.length === 0) {
        if (emptyEl) emptyEl.hidden = false;
        return;
      }
      if (emptyEl) emptyEl.hidden = true;
      for (const r of data.items) {
        const row = document.createElement('div');
        row.className = 'admin-publish-row';
        const when = r.uploaded_at
          ? new Date(r.uploaded_at * 1000).toISOString().slice(0, 16).replace('T', ' ')
          : '—';
        row.innerHTML = `
          <div class="uname">${escapeHTML(r.uploader_username || r.owner_user_id || '—')}</div>
          <div class="nm">${escapeHTML(r.name)}</div>
          <div class="desc" title="${escapeHTML(r.description || '')}">${escapeHTML(r.description || '—')}</div>
          <div class="when">${escapeHTML(when)}</div>
          <div class="actions">
            <button class="btn-small" data-act="approve" data-rid="${escapeHTML(r.resource_id)}" data-name="${escapeHTML(r.name)}">批准</button>
            <button class="btn-small btn-danger" data-act="reject" data-rid="${escapeHTML(r.resource_id)}" data-name="${escapeHTML(r.name)}">拒绝</button>
          </div>
        `;
        rowsEl.appendChild(row);
      }
      rowsEl.querySelectorAll('button[data-act]').forEach((b) => {
        b.addEventListener('click', () => publishAction(b.dataset.act, b.dataset.rid, b.dataset.name));
      });
    } catch (e) {
      rowsEl.innerHTML = `<div class="muted" style="padding:12px">加载失败:${escapeHTML(e.message)}</div>`;
    }
  }

  async function publishAction(action, resourceId, skillName) {
    if (!resourceId) return;
    try {
      if (action === 'approve') {
        const ok = await showConfirm({
          title: '批准发布',
          body: `把 ${skillName} 复制到社区池(uploader 的私有副本仍保留)。继续?`,
          ok: '批准',
          cancel: '取消',
          danger: false,
        });
        if (!ok) return;
        await api('POST', `/api/admin/publish-requests/${encodeURIComponent(resourceId)}/approve`);
      } else if (action === 'reject') {
        const reason = window.prompt(`拒绝 ${skillName} 的原因(会显示给上传者):`, '');
        if (reason === null) return;
        if (!reason.trim()) {
          alert('拒绝理由不能为空');
          return;
        }
        await api('POST', `/api/admin/publish-requests/${encodeURIComponent(resourceId)}/reject`, {
          reason: reason.trim(),
        });
      } else {
        return;
      }
      await loadAdminPublishRequests();
    } catch (e) {
      alert(`操作失败:${e.message}`);
    }
  }
