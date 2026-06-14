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
      const md = await res.text();
      el.innerHTML = setupMdToHtml(md);
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
