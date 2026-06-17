(() => {
  // ==================================================================
  //  runai dashboard — editorial / Linear-style production frontend
  //  Two views via hash router: Overview (#/) and Library (#/skills),
  //  plus a Skill Detail view (#/skill/<name>) shown inside Library.
  //
  //  Backend contract:
  //    GET /api/summary?hours=N
  //    GET /api/timeline?hours=N
  //    GET /api/events?hours=N&limit=N&offset=N
  //    GET /api/event/{id}
  //    GET /api/skills
  //    GET /api/skill/{name}
  //    GET /api/skill/{name}/files
  //    GET /api/skill/{name}/file?path=X
  // ==================================================================

  const state = {
    hours: '24',
    offset: 0,
    limit: 8,
  };
  const POLL_INTERVAL_MS = 5000;
  let pollTimer = null;
  let inFlight = false;

  const skillsState = { filter: '', sort: 'score-desc', enrichFilter: 'all', cache: [] };
  const detailState = { name: '', current: null, files: null, activeFile: null };

  const $ = (sel) => document.querySelector(sel);
  const $$ = (sel) => Array.from(document.querySelectorAll(sel));

  // ------------------------------------------------------------------
  //  Formatters
  // ------------------------------------------------------------------
  const pad = (n) => String(n).padStart(2, '0');
  const fmtTime = (ts) => {
    const d = new Date(ts * 1000);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtTs = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getMonth() + 1}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
  };
  const fmtTsFull = (ts) => {
    const d = new Date(ts * 1000);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const fmtAgo = (ts) => {
    if (!ts) return '—';
    const now = Math.floor(Date.now() / 1000);
    const s = Math.max(0, now - ts);
    if (s < 60) return `${s}s ago`;
    if (s < 3600) return `${Math.floor(s / 60)}m ago`;
    if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
    return `${Math.floor(s / 86400)}d ago`;
  };
  const fmtInt = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMs  = (n) => (n == null ? '—' : Math.round(n).toLocaleString());
  const fmtMsDur = (n) => {
    if (n == null) return '—';
    if (n >= 10000) return `${(n / 1000).toFixed(1)}s`;
    if (n >= 1000)  return `${(n / 1000).toFixed(2)}s`;
    return `${Math.round(n)}ms`;
  };
  const fmtTok = (n) => {
    if (n == null) return '—';
    if (n >= 10000) return `${(n / 1000).toFixed(1)}k`;
    return Math.round(n).toLocaleString();
  };
  const fmtPctRaw = (r) => (r == null ? null : (r * 100).toFixed(1));
  const escapeHTML = (s) =>
    String(s).replace(/[&<>"']/g, (c) => ({
      '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
    })[c]);
  const fmtBytes = (n) => {
    if (n == null) return '—';
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(2)} MB`;
  };

  function todayLabel() {
    return 'lifetime · 全部历史累计';
  }

