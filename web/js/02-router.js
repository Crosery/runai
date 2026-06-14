  // ------------------------------------------------------------------
  //  Router (hash-based)
  // ------------------------------------------------------------------
  function parseRoute() {
    const h = location.hash.replace(/^#\/?/, '');
    if (!h) return { view: 'overview' };
    if (h === 'skills') return { view: 'library' };
    if (h === 'activity') return { view: 'activity' };
    if (h === 'settings') return { view: 'settings' };
    if (h === 'admin') return { view: 'admin' };
    if (h === 'market') return { view: 'market' };
    if (h === 'help') return { view: 'help' };
    const m = h.match(/^skill\/(.+)$/);
    if (m) return { view: 'detail', name: decodeURIComponent(m[1]) };
    return { view: 'overview' };
  }

  function setActivePane(name) {
    $$('.tab').forEach((el) => el.classList.remove('active'));
    const pane = document.querySelector(`.tab[data-pane="${name}"]`);
    if (pane) pane.classList.add('active');
  }

  function setActiveNav(route) {
    $$('.side .nav-item').forEach((el) => el.classList.remove('active'));
    // Library is the active nav for both 'library' and 'detail' views.
    let navRoute = route;
    if (route === 'skills' || route === 'detail') navRoute = 'skills';
    else if (route === 'activity') navRoute = 'activity';
    else if (route === 'settings') navRoute = 'settings';
    else if (route === 'admin') navRoute = 'admin';
    else if (route === 'market') navRoute = 'market';
    else if (route === 'help') navRoute = 'help';
    else navRoute = '';
    const target = document.querySelector(`.side .nav-item[data-route="${navRoute}"]`);
    if (target) target.classList.add('active');
  }

  function applyRoute() {
    const r = parseRoute();
    const dlg = document.getElementById('detail-dialog');
    if (dlg && dlg.open) dlg.close();

    switch (r.view) {
      case 'library':
        setActivePane('skills');
        setActiveNav('skills');
        loadSkills();
        break;
      case 'detail':
        setActivePane('detail');
        setActiveNav('skills');
        loadSkillDetail(r.name);
        break;
      case 'activity':
        setActivePane('activity');
        setActiveNav('activity');
        loadActivity();
        break;
      case 'settings':
        setActivePane('settings');
        setActiveNav('settings');
        loadSettings();
        break;
      case 'admin':
        // Non-admins bounce back to overview rather than seeing an empty
        // admin pane. The sidebar link is also hidden for them.
        if (!account.me || !account.me.is_admin) {
          location.hash = '#/';
          return;
        }
        setActivePane('admin');
        setActiveNav('admin');
        loadSettings();
        loadAdminUsers();
        break;
      case 'market':
        if (!account.me) {
          showAuthModal('login');
          location.hash = '#/';
          return;
        }
        setActivePane('market');
        setActiveNav('market');
        loadMarket();
        break;
      case 'help':
        // PLANNING §1.7: Help tab is open to all viewers (logged-in or
        // not) — owner mode, team-mode admin, team-mode user, anonymous
        // pre-login all need to read it to know how to install / upload.
        setActivePane('help');
        setActiveNav('help');
        loadHelp();
        break;
      case 'overview':
      default:
        setActivePane('overview');
        setActiveNav('');
        refresh();
        break;
    }
    window.scrollTo({ top: 0, behavior: 'instant' in window ? 'instant' : 'auto' });
  }

  window.addEventListener('hashchange', applyRoute);

