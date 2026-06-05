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
    if (aw) initDropdown(aw, (val) => { activityState.hours = val; activityState.offset = 0; loadActivity(); });
    const ah = $('#activity-hit');
    if (ah) initDropdown(ah, (val) => { activityState.hitOnly = val; activityState.offset = 0; loadActivity(); });
    // Pager: reload the page in place WITHOUT the window jumping to the top.
    // Clearing the fixed-height #activity-list briefly collapses the page
    // height, which clamps window scroll to 0; capture scrollY before the
    // reload and restore it after the list refills.
    const pageActivity = (mutateOffset) => {
      if (!mutateOffset()) return;
      const y = window.scrollY;
      const keep = () => window.scrollTo({ top: y });
      loadActivity().finally(() => { keep(); requestAnimationFrame(keep); });
    };
    const ap = $('#activity-prev');
    if (ap) ap.addEventListener('click', () => pageActivity(() => {
      if (activityState.offset <= 0) return false;
      activityState.offset = Math.max(0, activityState.offset - activityState.limit);
      return true;
    }));
    const an = $('#activity-next');
    if (an) an.addEventListener('click', () => pageActivity(() => {
      if (activityState.offset + activityState.limit >= activityState.total) return false;
      activityState.offset += activityState.limit;
      return true;
    }));

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

