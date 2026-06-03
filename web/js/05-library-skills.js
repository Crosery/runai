  // ------------------------------------------------------------------
  //  Library: skills list
  // ------------------------------------------------------------------
  function renderSkillsRows() {
    let rows = skillsState.cache.slice();
    const f = skillsState.filter.toLowerCase().trim();
    if (f) {
      rows = rows.filter((s) =>
        s.name.toLowerCase().includes(f) ||
        (s.description || '').toLowerCase().includes(f) ||
        (s.summary || '').toLowerCase().includes(f)
      );
    }
    const sort = skillsState.sort;
    rows.sort((a, b) => {
      const sa = a.llm_score == null ? -1 : a.llm_score;
      const sb = b.llm_score == null ? -1 : b.llm_score;
      switch (sort) {
        case 'score-asc':  return sa - sb || a.name.localeCompare(b.name);
        case 'used-desc':  return (b.usage_count - a.usage_count) || sb - sa;
        case 'name':       return a.name.localeCompare(b.name);
        case 'unenriched': return ((a.summary ? 1 : -1) - (b.summary ? 1 : -1)) || sb - sa;
        case 'score-desc':
        default:           return sb - sa || a.name.localeCompare(b.name);
      }
    });

    const body = $('#skill-rows');
    body.innerHTML = '';
    $('#skills-empty').hidden = rows.length !== 0;
    rows.forEach((s, idx) => {
      const div = document.createElement('div');
      div.className = 'row';
      // v15: tag the row with the canonical skill name so scope filter +
      // select-mode bulk operations can identify the row without scraping
      // the inner text. Also marks rows that are currently in the user's
      // library so the indicator column renders correctly.
      div.dataset.skill = s.name;
      const inLib = account.libraryNames.has(s.name);
      if (inLib) div.classList.add('in-library');
      if (account.bulkSelect.has(s.name)) div.classList.add('selected');
      const llm = s.llm_score == null
        ? `<div class="llm unknown">—</div>`
        : `<div class="llm">${s.llm_score}</div>`;
      const desc = s.description ? `<small>${escapeHTML(s.description.slice(0, 140))}</small>` : '';
      // Phase F: owner badge — public pool vs private (server.rs omits the
      // owner_user_id field for public rows via serde(skip_serializing_if)).
      const ownerBadge = s.owner_user_id
        ? `<span class="owner-badge owner-private" title="私有 owner=${escapeHTML(s.owner_user_id)}">私有</span>`
        : `<span class="owner-badge owner-public" title="公共池 skill">公共</span>`;
      div.innerHTML = `
        <div class="idx">${String(idx + 1).padStart(2, '0')}</div>
        <div class="nm">${escapeHTML(s.name)} ${ownerBadge}${desc}</div>
        <div class="used">${s.usage_count || 0}</div>
        <div class="last">—</div>
        ${llm}
        <div class="lib-cell">${inLib ? '<span class="lib-on">●</span>' : '<span>○</span>'}</div>
      `;
      div.addEventListener('click', (ev) => {
        // In select mode, clicking toggles bulk selection instead of
        // opening the skill detail page.
        const container = document.getElementById('skill-rows');
        if (container?.classList.contains('select-mode')) {
          ev.stopPropagation();
          ev.preventDefault();
          const name = div.dataset.skill;
          if (account.bulkSelect.has(name)) {
            account.bulkSelect.delete(name);
            div.classList.remove('selected');
          } else {
            account.bulkSelect.add(name);
            div.classList.add('selected');
          }
          // Re-evaluate bulk-button visibility — in scope=all the
          // "加入 / 移出" buttons depend on what's selected (in-library
          // vs not-in-library), so re-render the scope bar on every
          // selection change.
          renderScopeBar();
          return;
        }
        location.hash = `#/skill/${encodeURIComponent(s.name)}`;
      });
      body.appendChild(div);
    });
    // After populating the rows, apply the current scope filter so the
    // user's "my library / public / all" selection takes effect on every
    // (re)render, not just on scope-button click.
    if (typeof renderSkills === 'function') renderSkills();
  }

  async function loadSkills() {
    const res = await fetch('/api/skills');
    if (!res.ok) return;
    const data = await res.json();
    skillsState.cache = data.skills || [];
    $('#library-count').textContent = `${data.total} installed`;
    $('#lib-sub-installed-count').textContent = data.total;
    $('#hero-strip-skills').textContent = data.total;
    $('#hero-strip-skills-delta').textContent = `${data.enriched} enriched`;
    renderSkillsRows();
  }

