  // ------------------------------------------------------------------
  //  PLANNING §1.7 — Help tab user manual
  // ------------------------------------------------------------------
  // The dashboard avoids external JS deps (single-binary contract), so
  // this is a minimal Markdown → HTML converter scoped to the subset of
  // syntax used in web/help.md: H1/H2/H3, paragraphs, fenced code blocks
  // (```), inline code (`x`), bold (**x**), unordered (`- `) + ordered
  // (`1. `) lists, pipe tables, horizontal rules (`---`).
  //
  // All text is run through escapeHTML so the markdown source can never
  // inject script tags via the rendered output, even though the source
  // is server-controlled today.

  function helpMdToHtml(md) {
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

  let helpLoaded = false;
  async function loadHelp() {
    if (helpLoaded) return;
    const el = $('#help-content');
    if (!el) return;
    try {
      const res = await fetch('/help.md', { credentials: 'same-origin' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const md = await res.text();
      el.innerHTML = helpMdToHtml(md);
      helpLoaded = true;
    } catch (e) {
      el.innerHTML = `<p class="muted">手册加载失败:${escapeHTML(e.message)}</p>`;
    }
  }
