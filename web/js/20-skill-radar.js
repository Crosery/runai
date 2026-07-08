  // ------------------------------------------------------------------
  //  Skill feedback radar — five-axis SVG chart (skill-feedback-radar,
  //  PLANNING §1 phase 4 dashboard front-end). Reads /api/skill/{name}'s
  //  `radar` / `radar_avg` (0-10 per axis, see src/server/skills.rs
  //  RadarJson) and animates from the previously-displayed values (or 0 on
  //  first paint) into the new target via requestAnimationFrame. Pure
  //  DOM/SVG — no chart library, matching the rest of web/ (no CDN, no
  //  build step).
  // ------------------------------------------------------------------
  const RADAR_AXES = [
    { key: 'adoption', label: '采纳' },
    { key: 'precision', label: '精准' },
    { key: 'rating', label: '口碑' },
    { key: 'quality', label: '质量' },
    { key: 'heat', label: '热度' },
  ];
  const RADAR_MAX = 10;
  const RADAR_ZERO = RADAR_AXES.reduce((a, ax) => ((a[ax.key] = 0), a), {});
  // containerId -> { raf, skill: {...}, avg: {...} } — lets a data refresh
  // interpolate FROM the currently-displayed values instead of snapping,
  // and lets a first paint animate outward from the center.
  const radarAnimState = new Map();

  function radarAngle(i) {
    return (i / RADAR_AXES.length) * Math.PI * 2;
  }
  function radarPoint(cx, cy, r, angle) {
    return [cx + r * Math.sin(angle), cy - r * Math.cos(angle)];
  }
  function radarPolygonPoints(cx, cy, maxR, values) {
    return RADAR_AXES.map((ax, i) => {
      const v = Math.max(0, Math.min(RADAR_MAX, values[ax.key] ?? 0));
      return radarPoint(cx, cy, (v / RADAR_MAX) * maxR, radarAngle(i));
    });
  }
  function radarPtsAttr(pts) {
    return pts.map((p) => `${p[0].toFixed(1)},${p[1].toFixed(1)}`).join(' ');
  }
  function radarEaseOutCubic(t) {
    return 1 - Math.pow(1 - t, 3);
  }

  function renderSkillRadar(containerId, radar, radarAvg) {
    const host = document.getElementById(containerId);
    if (!host) return;
    const target = radar || {};
    const targetAvg = radarAvg || {};

    const prev = radarAnimState.get(containerId);
    if (prev && prev.raf) cancelAnimationFrame(prev.raf);
    const fromSkill = prev ? prev.skill : RADAR_ZERO;
    const fromAvg = prev ? prev.avg : RADAR_ZERO;

    const W = 320, H = 300, cx = 150, cy = 142, maxR = 100, rings = 4;

    let gridSvg = '';
    for (let ring = 1; ring <= rings; ring++) {
      const r = (ring / rings) * maxR;
      const pts = RADAR_AXES.map((_, i) => radarPoint(cx, cy, r, radarAngle(i)));
      gridSvg += `<polygon class="radar-grid-ring" points="${radarPtsAttr(pts)}"></polygon>`;
    }
    let axisSvg = '';
    let labelSvg = '';
    RADAR_AXES.forEach((ax, i) => {
      const angle = radarAngle(i);
      const [ex, ey] = radarPoint(cx, cy, maxR, angle);
      axisSvg += `<line class="radar-axis-line" x1="${cx}" y1="${cy}" x2="${ex.toFixed(1)}" y2="${ey.toFixed(1)}"></line>`;
      const [lx, ly] = radarPoint(cx, cy, maxR + 24, angle);
      const anchor = Math.abs(ex - cx) < 4 ? 'middle' : ex > cx ? 'start' : 'end';
      labelSvg += `<text class="radar-axis-label" x="${lx.toFixed(1)}" y="${ly.toFixed(1)}" text-anchor="${anchor}">${escapeHTML(ax.label)}</text>`;
    });

    host.innerHTML = `
      <svg class="radar-svg" viewBox="0 0 ${W} ${H}" preserveAspectRatio="xMidYMid meet" role="img" aria-label="skill feedback radar">
        <g class="radar-grid">${gridSvg}</g>
        <g class="radar-axes">${axisSvg}</g>
        <polygon class="radar-poly radar-poly-avg"></polygon>
        <polygon class="radar-poly radar-poly-skill"></polygon>
        <g class="radar-values"></g>
        <g class="radar-labels">${labelSvg}</g>
      </svg>
      <div class="radar-legend">
        <span class="radar-legend-item"><i class="radar-swatch radar-swatch-skill"></i>本 skill</span>
        <span class="radar-legend-item"><i class="radar-swatch radar-swatch-avg"></i>全库均值</span>
      </div>`;

    const polySkill = host.querySelector('.radar-poly-skill');
    const polyAvg = host.querySelector('.radar-poly-avg');
    const valuesGroup = host.querySelector('.radar-values');
    if (!polySkill || !polyAvg || !valuesGroup) return;

    const DURATION = 420;
    const t0 = performance.now();

    function frame(now) {
      const t = Math.min(1, (now - t0) / DURATION);
      const eased = radarEaseOutCubic(t);
      const curSkill = {};
      const curAvg = {};
      RADAR_AXES.forEach((ax) => {
        const fv = fromSkill[ax.key] ?? 0;
        const tv = target[ax.key] ?? 0;
        curSkill[ax.key] = fv + (tv - fv) * eased;
        const fav = fromAvg[ax.key] ?? 0;
        const tav = targetAvg[ax.key] ?? 0;
        curAvg[ax.key] = fav + (tav - fav) * eased;
      });
      polySkill.setAttribute('points', radarPtsAttr(radarPolygonPoints(cx, cy, maxR, curSkill)));
      polyAvg.setAttribute('points', radarPtsAttr(radarPolygonPoints(cx, cy, maxR, curAvg)));
      valuesGroup.innerHTML = RADAR_AXES.map((ax, i) => {
        const v = curSkill[ax.key] ?? 0;
        const r = Math.max((v / RADAR_MAX) * maxR, 14);
        const [vx, vy] = radarPoint(cx, cy, r, radarAngle(i));
        return `<text class="radar-value-label" x="${vx.toFixed(1)}" y="${vy.toFixed(1)}">${v.toFixed(1)}</text>`;
      }).join('');

      if (t < 1) {
        const raf = requestAnimationFrame(frame);
        radarAnimState.set(containerId, { raf, skill: curSkill, avg: curAvg });
      } else {
        radarAnimState.set(containerId, { raf: null, skill: target, avg: targetAvg });
      }
    }
    const raf = requestAnimationFrame(frame);
    radarAnimState.set(containerId, { raf, skill: fromSkill, avg: fromAvg });
  }

  // ------------------------------------------------------------------
  //  Skill detail feedback panel: stats strip + recent votes + 好评/差评
  //  buttons. `d` is the full /api/skill/{name} response
  //  (SkillDetailResponse — feedback_stats / feedback_recent).
  // ------------------------------------------------------------------
  function setTextIfPresent(sel, text) {
    const el = $(sel);
    if (el) el.textContent = text;
  }

  function renderFeedbackPanel(d) {
    const stats = d.feedback_stats || {};
    setTextIfPresent('#detail-fb-pos', fmtInt(stats.pos ?? 0));
    setTextIfPresent('#detail-fb-neg', fmtInt(stats.neg ?? 0));
    setTextIfPresent('#detail-fb-chosen', fmtInt(stats.chosen_sessions ?? 0));
    setTextIfPresent('#detail-fb-adopted', fmtInt(stats.adopted_sessions ?? 0));

    const recentHost = $('#detail-fb-recent');
    if (recentHost) {
      const rows = d.feedback_recent || [];
      recentHost.innerHTML = rows.length
        ? rows.map((f) => {
            const mark = f.verdict > 0 ? 'good' : 'bad';
            const who = f.user_id ? escapeHTML(f.user_id) : '';
            const note = f.note ? `<span class="radar-recent-note">${escapeHTML(f.note)}</span>` : '';
            return `<div class="radar-recent-row radar-recent-${mark}">
              <span class="radar-recent-verdict">${f.verdict > 0 ? '好评' : '差评'}</span>
              <span class="radar-recent-ago muted">${fmtAgo(f.ts)}</span>
              ${who ? `<span class="radar-recent-user mono">${who}</span>` : ''}
              ${note}
            </div>`;
          }).join('')
        : '<div class="muted radar-recent-empty">还没有人给这个 skill 反馈过</div>';
    }
    const msg = $('#detail-fb-msg');
    if (msg) msg.textContent = '';
  }

  // 好评/差评 verdict-only 投票（POST /feedback，无 note → 服务端快路径，不
  // 触发 LLM 重新富集）。成功后重新拉一次 /api/skill/{name} 让雷达/统计跟着
  // 刷新；#detail-fb-good/#detail-fb-bad 是 index.html 里的静态节点，
  // loadSkillDetail 的 innerHTML 重写不会替换它们，所以只在这里绑一次即可。
  async function submitDetailVote(verdict) {
    const name = detailState.name;
    if (!name) return;
    const goodBtn = $('#detail-fb-good');
    const badBtn = $('#detail-fb-bad');
    const msg = $('#detail-fb-msg');
    if (goodBtn) goodBtn.disabled = true;
    if (badBtn) badBtn.disabled = true;
    try {
      await api('POST', '/feedback', { skill: name, verdict });
      await loadSkillDetail(name, detailState.owner);
      const msg2 = $('#detail-fb-msg');
      if (msg2) msg2.textContent = verdict > 0 ? '已记录好评，感谢反馈' : '已记录差评，感谢反馈';
    } catch (err) {
      if (msg) msg.textContent = err && err.status === 401 ? '需要登录才能反馈' : '反馈失败，请稍后重试';
    } finally {
      const g = $('#detail-fb-good');
      if (g) g.disabled = false;
      const b = $('#detail-fb-bad');
      if (b) b.disabled = false;
    }
  }

  function bindFeedbackRadarUI() {
    const goodBtn = document.getElementById('detail-fb-good');
    if (goodBtn) goodBtn.addEventListener('click', () => submitDetailVote(1));
    const badBtn = document.getElementById('detail-fb-bad');
    if (badBtn) badBtn.addEventListener('click', () => submitDetailVote(-1));
  }
