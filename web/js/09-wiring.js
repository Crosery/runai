  // ------------------------------------------------------------------
  //  Wiring
  // ------------------------------------------------------------------
  function bindControls() {
    // Skill filter input
    const filterEl = $('#skill-filter');
    if (filterEl) {
      filterEl.addEventListener('input', (e) => {
        skillsState.filter = e.target.value;
        renderSkillsRows();
      });
    }

    // Sort dropdown
    const sortDd = $('#skill-sort-dd');
    if (sortDd) {
      initDropdown(sortDd, (val) => {
        skillsState.sort = val;
        renderSkillsRows();
      });
    }

    // Event detail dialog close
    const closeBtn = $('#detail-close');
    if (closeBtn) closeBtn.addEventListener('click', () => $('#detail-dialog').close());

    // Activity tab controls
    const af = $('#activity-filter');
    if (af) {
      af.addEventListener('input', (e) => {
        activityState.filter = e.target.value;
        loadActivity();
      });
    }
    const aw = $('#activity-window');
    if (aw) initDropdown(aw, (val) => { activityState.hours = val; loadActivity(); });
    const ah = $('#activity-hit');
    if (ah) initDropdown(ah, (val) => { activityState.hitOnly = val; loadActivity(); });
    // prev/next pager retired — the Activity list now streams via infinite
    // scroll (see ensureActivitySentinel / loadActivityEvents in 04-*.js).

    // Global dropdown close
    document.addEventListener('click', () => {
      document.querySelectorAll('.dropdown.open').forEach((x) => x.classList.remove('open'));
    });
    document.addEventListener('keydown', (e) => {
      if (e.key === 'Escape') {
        document.querySelectorAll('.dropdown.open').forEach((x) => x.classList.remove('open'));
      }
    });

    // Keyboard shortcuts (skip when typing)
    document.addEventListener('keydown', (e) => {
      const tag = (e.target && e.target.tagName) || '';
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      if (e.key === 'o') { location.hash = '#/'; }
      else if (e.key === 'l') { location.hash = '#/skills'; }
    });
  }

