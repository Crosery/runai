  // ------------------------------------------------------------------
  //  Custom dropdown wiring (mockup-style; no native <select>)
  // ------------------------------------------------------------------
  function initDropdown(dd, onChange) {
    const trigger = dd.querySelector('.dropdown-trigger');
    const menu = dd.querySelector('.dropdown-menu');
    const labelEl = trigger.querySelector('.dd-label');

    trigger.addEventListener('click', (e) => {
      e.stopPropagation();
      const wasOpen = dd.classList.contains('open');
      document.querySelectorAll('.dropdown.open').forEach((x) => {
        if (x !== dd) x.classList.remove('open');
      });
      dd.classList.toggle('open', !wasOpen);
    });

    menu.querySelectorAll('.dd-opt').forEach((opt) => {
      opt.addEventListener('click', (e) => {
        e.stopPropagation();
        menu.querySelectorAll('.dd-opt').forEach((x) => x.classList.remove('active'));
        opt.classList.add('active');
        if (labelEl) labelEl.textContent = opt.textContent.trim();
        dd.classList.remove('open');
        if (onChange) onChange(opt.dataset.value);
      });
    });
  }

  // ------------------------------------------------------------------
  //  Theme swatch picker
  // ------------------------------------------------------------------
  function initSwatches() {
    document.querySelectorAll('.swatch').forEach((s) => {
      s.addEventListener('click', () => {
        const t = s.getAttribute('data-theme');
        document.body.className = document.body.className
          .replace(/\btheme-\w+\b/g, '').trim();
        document.body.classList.add('theme-' + t);
        document.querySelectorAll('.swatch').forEach((x) => x.classList.remove('active'));
        s.classList.add('active');
        try { localStorage.setItem('runai.theme', t); } catch (_e) { /* ignore */ }
      });
    });
    // restore saved theme
    try {
      const saved = localStorage.getItem('runai.theme');
      if (saved && /^[a-z]+$/.test(saved)) {
        const s = document.querySelector(`.swatch[data-theme="${saved}"]`);
        if (s) s.click();
      }
    } catch (_e) { /* ignore */ }
  }

  // ------------------------------------------------------------------
  //  Custom cursor (SVG arrow + lagging ring + contextual label)
  // ------------------------------------------------------------------
  function initCustomCursor() {
    if (!window.matchMedia || !window.matchMedia('(pointer:fine)').matches) return;

    const glow  = document.querySelector('.cursor-glow');
    const arrow = document.querySelector('.cursor-arrow');
    const ring  = document.querySelector('.cursor-ring');
    const label = document.querySelector('.cursor-label');
    if (!arrow || !ring || !label) return;

    let mx = window.innerWidth / 2, my = window.innerHeight / 2;
    let rx = mx, ry = my;
    let lx = mx, ly = my;

    function frame() {
      arrow.style.transform = `translate(${mx - 2}px,${my - 2}px)`;
      rx += (mx - rx) * 0.18;
      ry += (my - ry) * 0.18;
      ring.style.transform = `translate(${rx - ring.offsetWidth / 2}px,${ry - ring.offsetHeight / 2}px)`;
      lx += (mx - lx) * 0.32;
      ly += (my - ly) * 0.32;
      label.style.transform = `translate(${lx + 18}px,${ly + 18}px)`;
      if (glow) {
        glow.style.setProperty('--mx', mx + 'px');
        glow.style.setProperty('--my', my + 'px');
      }
      requestAnimationFrame(frame);
    }
    requestAnimationFrame(frame);

    document.addEventListener('mousemove', (e) => {
      mx = e.clientX; my = e.clientY;
      if (arrow.classList.contains('hide')) {
        arrow.classList.remove('hide');
        ring.classList.remove('hide');
      }
    }, { passive: true });

    document.addEventListener('mouseleave', () => {
      arrow.classList.add('hide');
      ring.classList.add('hide');
      label.classList.remove('visible');
    });
    document.addEventListener('mouseenter', () => {
      arrow.classList.remove('hide');
      ring.classList.remove('hide');
    });

    function showLabel(text, accent) {
      label.textContent = text;
      label.classList.toggle('accent', !!accent);
      label.classList.remove('hide');
      label.classList.add('visible');
    }
    function hideLabel() { label.classList.remove('visible'); }

    let currentIntent = null;
    function applyIntent(name) {
      if (currentIntent) {
        arrow.classList.remove('intent-' + currentIntent);
        ring.classList.remove('intent-' + currentIntent);
      }
      if (name) {
        arrow.classList.add('intent-' + name);
        ring.classList.add('intent-' + name);
      }
      currentIntent = name;
    }

    // [selector, label-text, intent-class, label-accent-style]
    const bindings = [
      ['.skill-rows .row',           'open skill', 'deep',    true],
      ['.recent-row',                'view exec',  'deep',    false],
      ['.stream-item',               'view exec',  'deep',    false],
      ['.events-rows .er-row',       'view exec',  'deep',    false],
      ['.dd-opt',                    'select',     'primary', true],
      ['.dropdown-trigger',          'open',       'default', false],
      ['.lib-sub',                   'switch',     'default', false],
      ['.back-link',                 'back',       'default', false],
      ['.btn',                       'tap',        'default', false],
      ['.swatch',                    'preview',    'theme',   false],
      ['.nav-item',                  'navigate',   'default', false],
      ['.skill-rows .row .toggle',   'toggle',     'toggle',  false],
      ['.ftree-entry',               'open file',  'default', false],
      ['a.chip',                     'open',       'default', false],
      ['a:not(.back-link):not(.lib-sub):not(.nav-item):not(.chip)', 'open', 'default', false],
    ];

    // Re-bind every time DOM mutates (because we render rows dynamically).
    function bind() {
      bindings.forEach((pair) => {
        const [sel, text, intent, accent] = pair;
        document.querySelectorAll(sel).forEach((el) => {
          if (el.dataset.cursorBound === '1') return;
          el.dataset.cursorBound = '1';
          let hoverIntent = intent;
          if (el.classList.contains('off') || el.classList.contains('disabled') ||
              el.closest('[disabled]') || el.getAttribute('aria-disabled') === 'true') {
            hoverIntent = 'disabled';
          }
          el.addEventListener('mouseenter', () => {
            if (intent === 'theme') {
              const c = getComputedStyle(el).backgroundColor;
              arrow.style.setProperty('--swatch-c', c);
              ring.style.setProperty('--swatch-c', c);
            }
            applyIntent(hoverIntent);
            showLabel(text, accent);
          });
          el.addEventListener('mouseleave', () => {
            if (intent === 'theme') {
              arrow.style.removeProperty('--swatch-c');
              ring.style.removeProperty('--swatch-c');
            }
            applyIntent(null);
            hideLabel();
          });
        });
      });

      document.querySelectorAll('input,textarea,[contenteditable]').forEach((el) => {
        if (el.dataset.cursorTextBound === '1') return;
        el.dataset.cursorTextBound = '1';
        el.addEventListener('mouseenter', () => {
          document.body.classList.add('text-cursor');
          applyIntent(null);
          hideLabel();
        });
        el.addEventListener('mouseleave', () => {
          document.body.classList.remove('text-cursor');
        });
      });
    }

    bind();
    // Re-bind when DOM changes (new rows etc).
    const mo = new MutationObserver(() => bind());
    mo.observe(document.body, { childList: true, subtree: true });

    document.addEventListener('mousedown', () => { ring.classList.add('click'); });
    document.addEventListener('mouseup',   () => { ring.classList.remove('click'); });
  }

