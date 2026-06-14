  // ------------------------------------------------------------------
  //  PLANNING §1.7 — Setup tab (命令清单 + 一键复制)
  // ------------------------------------------------------------------
  // The dashboard avoids external JS deps (single-binary contract), so
  // this is a minimal Markdown → HTML converter scoped to the subset of
  // syntax used in web/setup.md: H1/H2/H3, paragraphs, fenced code blocks
  // (```), inline code (`x`), bold (**x**), unordered (`- `) + ordered
  // (`1. `) lists, pipe tables, horizontal rules (`---`).
  //
  // All text is run through escapeHTML so the markdown source can never
  // inject script tags via the rendered output, even though the source
  // is server-controlled today.

  function setupMdToHtml(md) {
    const lines = md.split('\n');
    const out = [];
    let inCode = false;
    let listMode = null; // 'ul' | 'ol' | null
    let inTable = false;
    let tableHeaderDone = false;

    const flushList = () => {
      if (listMode === 'ul') out.push('</ul>');
      if (listMode === 'ol') out.push('</ol>');
      listMode = null;
    };
    const flushTable = () => {
      if (inTable) {
        out.push('</tbody></table>');
        inTable = false;
        tableHeaderDone = false;
      }
    };
    const flushAll = () => {
      flushList();
      flushTable();
    };

    const inlineMd = (s) => {
      let v = escapeHTML(s);
      // Bold first so inline code wrapping a bold range is safe (rare).
      v = v.replace(/\*\*([^*]+?)\*\*/g, '<strong>$1</strong>');
      v = v.replace(/`([^`]+?)`/g, '<code>$1</code>');
      return v;
    };

    for (const raw of lines) {
      const line = raw.replace(/\r$/, '');

      // Fenced code block toggle. `text` is dropped (no syntax highlight).
      if (/^```/.test(line)) {
        if (inCode) {
          out.push('</code></pre>');
          inCode = false;
        } else {
          flushAll();
          out.push('<pre><code>');
          inCode = true;
        }
        continue;
      }
      if (inCode) {
        out.push(escapeHTML(line));
        continue;
      }

      let m;
      if ((m = line.match(/^# (.+)$/))) {
        flushAll();
        out.push(`<h1>${inlineMd(m[1])}</h1>`);
        continue;
      }
      if ((m = line.match(/^## (.+)$/))) {
        flushAll();
        out.push(`<h2>${inlineMd(m[1])}</h2>`);
        continue;
      }
      if ((m = line.match(/^### (.+)$/))) {
        flushAll();
        out.push(`<h3>${inlineMd(m[1])}</h3>`);
        continue;
      }

      // Tables (pipe syntax).
      if (line.startsWith('|')) {
        const cells = line
          .slice(1)
          .replace(/\|\s*$/, '')
          .split('|')
          .map((c) => c.trim());
        // Separator row (e.g. `|---|---|`) — toggles body mode.
        if (cells.every((c) => /^:?-{2,}:?$/.test(c))) {
          tableHeaderDone = true;
          out.push('</thead><tbody>');
          continue;
        }
        if (!inTable) {
          flushList();
          out.push('<table><thead>');
          inTable = true;
          tableHeaderDone = false;
        }
        const tag = tableHeaderDone ? 'td' : 'th';
        const row = cells.map((c) => `<${tag}>${inlineMd(c)}</${tag}>`).join('');
        out.push(`<tr>${row}</tr>`);
        continue;
      } else if (inTable) {
        flushTable();
      }

      if ((m = line.match(/^(\d+)\.\s+(.+)$/))) {
        if (listMode !== 'ol') {
          flushList();
          out.push('<ol>');
          listMode = 'ol';
        }
        out.push(`<li>${inlineMd(m[2])}</li>`);
        continue;
      }
      if ((m = line.match(/^[-*]\s+(.+)$/))) {
        if (listMode !== 'ul') {
          flushList();
          out.push('<ul>');
          listMode = 'ul';
        }
        out.push(`<li>${inlineMd(m[1])}</li>`);
        continue;
      }
      if (listMode) flushList();

      if (/^---+$/.test(line.trim())) {
        out.push('<hr>');
        continue;
      }
      if (line.trim() === '') continue;
      out.push(`<p>${inlineMd(line)}</p>`);
    }

    if (inCode) out.push('</code></pre>');
    flushAll();
    return out.join('\n');
  }

  // Minimal bash/shell syntax highlighter. The dashboard ships zero
  // external JS deps (single-binary contract), so a 50-line tokenizer is
  // the right size — full Prism / highlight.js would be 60+ KB for one
  // tab. Tokens recognized per line:
  //   - comment    : starts with `#` (rest of line, italic gray)
  //   - env var    : `FOO=bar` form at command position (red)
  //   - string     : 'single' / "double" quoted (green)
  //   - placeholder: <name-like> tokens (orange) — runai docs use these
  //   - long flag  : `--name` (blue)
  //   - short flag : `-fsSL` after whitespace (blue)
  //   - keyword    : one of the well-known shell heads (purple, bold)
  // The output of `code.innerText` is still the raw text (spans are
  // inline), so the copy button keeps grabbing the verbatim command.
  const BASH_KEYWORDS = new Set([
    'curl', 'runai', 'runai-client', 'bash', 'cargo', 'kill', 'nohup',
    'launchctl', 'systemctl', 'sudo', 'export', 'set', 'cd', 'ls',
    'cp', 'mv', 'rm', 'mkdir', 'echo', 'cat',
  ]);

  function highlightBashLine(line) {
    // Whole-line comment.
    if (/^\s*#/.test(line)) {
      return '<span class="hl-c">' + escapeHTML(line) + '</span>';
    }
    let head = line;
    let tail = '';
    // Trailing comment (` # text`) — split off before tokenizing.
    const cIdx = line.search(/(?:^|\s)#/);
    if (cIdx > 0 && line[cIdx] !== '#') {
      head = line.slice(0, cIdx + 1);
      tail = '<span class="hl-c">' + escapeHTML(line.slice(cIdx + 1)) + '</span>';
    }
    const re =
      /([A-Z_][A-Z0-9_]*)=|(--[a-zA-Z][a-zA-Z0-9-]*)|(?:^|\s)(-[a-zA-Z]+)(?=\s|$)|('[^']*')|("[^"]*")|(<[a-zA-Z_][a-zA-Z0-9_-]*>)|\b([a-z][a-zA-Z0-9_-]*)\b/g;
    let out = '';
    let last = 0;
    let m;
    while ((m = re.exec(head)) !== null) {
      out += escapeHTML(head.slice(last, m.index));
      if (m[1]) {
        out += '<span class="hl-v">' + escapeHTML(m[1]) + '</span>=';
      } else if (m[2]) {
        out += '<span class="hl-f">' + escapeHTML(m[2]) + '</span>';
      } else if (m[3]) {
        // Re-emit the leading whitespace from the lookahead match.
        const lead = head.slice(m.index, m.index + (m[0].length - m[3].length));
        out += escapeHTML(lead) + '<span class="hl-f">' + escapeHTML(m[3]) + '</span>';
      } else if (m[4]) {
        out += '<span class="hl-s">' + escapeHTML(m[4]) + '</span>';
      } else if (m[5]) {
        out += '<span class="hl-s">' + escapeHTML(m[5]) + '</span>';
      } else if (m[6]) {
        out += '<span class="hl-p">' + escapeHTML(m[6]) + '</span>';
      } else if (m[7]) {
        if (BASH_KEYWORDS.has(m[7])) {
          out += '<span class="hl-k">' + escapeHTML(m[7]) + '</span>';
        } else {
          out += escapeHTML(m[7]);
        }
      }
      last = re.lastIndex;
    }
    out += escapeHTML(head.slice(last));
    return out + tail;
  }

  function highlightBash(text) {
    return text.split('\n').map(highlightBashLine).join('\n');
  }

  function highlightCodeBlocks(container) {
    container.querySelectorAll('pre code').forEach((code) => {
      code.innerHTML = highlightBash(code.textContent);
    });
  }

  // OS detection — coarse but reliable for picking install command
  // syntax. Windows -> PowerShell, everything else -> POSIX shell
  // (Mac / Linux / *BSD / WSL). The detection happens client-side AFTER
  // fetch because the same /setup.md body serves every client; we strip
  // the non-matching OS block before rendering.
  function detectClientOS() {
    const ua = (navigator.userAgent || '').toLowerCase();
    const plat = ((navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || '').toLowerCase();
    if (ua.includes('windows') || plat.includes('win')) return 'windows';
    return 'posix';
  }

  // Strip every <!-- runai:<aud>-start --> ... <!-- ...-end --> block
  // where <aud> is in dropList, plus every leftover keep-side marker
  // comment line so rendered markdown never shows raw HTML comments.
  // Mirror of the server-side strip_setup_marker_section in app.rs.
  function stripOsMarkerBlocks(md, dropList) {
    const dropStarts = new Set(dropList.map((a) => `<!-- runai:${a}-start -->`));
    const dropEnds = new Set(dropList.map((a) => `<!-- runai:${a}-end -->`));
    const lines = md.split('\n');
    const out = [];
    let inDrop = false;
    let activeEnd = null;
    for (const line of lines) {
      const t = line.trim();
      if (!inDrop && dropStarts.has(t)) {
        inDrop = true;
        // Find matching end marker by replacing -start- with -end-
        activeEnd = t.replace('-start -->', '-end -->');
        continue;
      }
      if (inDrop) {
        if (t === activeEnd || dropEnds.has(t)) {
          inDrop = false;
          activeEnd = null;
        }
        continue;
      }
      // Strip any surviving OS marker line (the keep-side has them too).
      if (/^<!-- runai:os-[a-z]+-only-(start|end) -->$/.test(t)) continue;
      out.push(line);
    }
    return out.join('\n');
  }

  // No cache: the server tailors /setup.md per user (admin vs user vs
  // anonymous) AND substitutes {SERVER_URL} per request, so a stale copy
  // after login/logout would show the wrong commands. Re-fetching on
  // every nav to #/setup is one tiny markdown body — not worth caching.
  async function loadSetup() {
    const el = $('#setup-content');
    if (!el) return;
    el.innerHTML = '<p class="muted">加载中 …</p>';
    try {
      const res = await fetch('/setup.md', { credentials: 'same-origin' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      let md = await res.text();
      const os = detectClientOS();
      const drop = os === 'windows' ? ['os-posix-only'] : ['os-windows-only'];
      md = stripOsMarkerBlocks(md, drop);
      el.innerHTML = setupMdToHtml(md);
      highlightCodeBlocks(el);
      attachCopyButtons(el);
    } catch (e) {
      el.innerHTML = `<p class="muted">Setup 加载失败:${escapeHTML(e.message)}</p>`;
    }
  }

  // SVG icons — kept inline so they ship with the bundle (no extra
  // network round-trip). 14x14 viewBox lines up with the .copy-btn slot.
  const COPY_ICON_SVG =
    '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<rect x="5" y="5" width="9" height="9" rx="1.5"/>' +
    '<path d="M11 5V2.5A1 1 0 0 0 10 1.5H2.5A1 1 0 0 0 1.5 2.5V10A1 1 0 0 0 2.5 11H5"/>' +
    '</svg>';
  const CHECK_ICON_SVG =
    '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<path d="M3 8L7 12L13 4"/>' +
    '</svg>';

  // Inject a copy button into every `<pre><code>` block so users can grab
  // the command without selecting text. Uses the async clipboard API;
  // falls back to `document.execCommand('copy')` on the rare browser that
  // still lacks it (older Safari in non-secure contexts). The button icon
  // flips to a green check for 1.5s before reverting so the click
  // registers visibly without a modal toast.
  function attachCopyButtons(container) {
    container.querySelectorAll('pre').forEach((pre) => {
      if (pre.querySelector('.copy-btn')) return;
      const code = pre.querySelector('code');
      if (!code) return;
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'copy-btn';
      btn.title = '复制';
      btn.setAttribute('aria-label', '复制命令');
      btn.innerHTML = COPY_ICON_SVG;
      btn.addEventListener('click', async (ev) => {
        ev.stopPropagation();
        const text = code.innerText;
        try {
          if (navigator.clipboard && window.isSecureContext) {
            await navigator.clipboard.writeText(text);
          } else {
            const ta = document.createElement('textarea');
            ta.value = text;
            ta.style.position = 'fixed';
            ta.style.left = '-9999px';
            document.body.appendChild(ta);
            ta.select();
            document.execCommand('copy');
            document.body.removeChild(ta);
          }
          btn.innerHTML = CHECK_ICON_SVG;
          btn.classList.add('copied');
          btn.title = '已复制';
          setTimeout(() => {
            btn.innerHTML = COPY_ICON_SVG;
            btn.classList.remove('copied');
            btn.title = '复制';
          }, 1500);
        } catch (e) {
          btn.title = '复制失败';
        }
      });
      pre.appendChild(btn);
    });
  }
