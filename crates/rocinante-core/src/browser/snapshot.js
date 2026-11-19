function () {
  // The page as text for the model: where it is, problems worth fixing, and
  // its headings, text, images and numbered interactive elements in order.
  // Numbers stay valid until the next snapshot (window.__rcnRefs).
  const MAX_LINES = 180;
  const clean = (s, n) => {
    const t = (s || '').replace(/\s+/g, ' ').trim();
    return t.length > n ? t.slice(0, n - 1) + '…' : t;
  };
  const shown = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width < 1 || r.height < 1) return false;
    const s = getComputedStyle(el);
    return s.visibility !== 'hidden' && s.display !== 'none' && Number(s.opacity) !== 0;
  };
  const describe = (el) => {
    let d = el.tagName.toLowerCase();
    if (el.id) d += '#' + el.id;
    const cls = typeof el.className === 'string' ? el.className.trim().split(/\s+/)[0] : '';
    if (cls) d += '.' + cls;
    return '<' + d + '>';
  };
  const label = (el) =>
    clean(
      el.getAttribute('aria-label') ||
        (el.labels && el.labels[0] && el.labels[0].innerText) ||
        el.innerText ||
        el.getAttribute('title') ||
        el.getAttribute('alt') ||
        el.getAttribute('placeholder') ||
        el.getAttribute('name') ||
        '',
      70,
    );
  const roleOf = (el) => {
    const r = el.getAttribute('role');
    if (r) return r;
    const t = el.tagName.toLowerCase();
    const type = (el.getAttribute('type') || '').toLowerCase();
    if (t === 'a') return 'link';
    if (t === 'button' || (t === 'input' && ['button', 'submit', 'reset', 'image'].includes(type))) return 'button';
    if (t === 'input' && (type === 'checkbox' || type === 'radio')) return type;
    if (t === 'input' || t === 'textarea') return 'textbox';
    if (t === 'select') return 'select';
    if (t === 'summary') return 'summary';
    return 'clickable';
  };
  const INTERACTIVE =
    'a[href],button,input:not([type=hidden]),textarea,select,summary,' +
    '[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=switch],' +
    '[onclick],[contenteditable=true],[tabindex]:not([tabindex="-1"])';
  const isInteractive = (el) => el.matches(INTERACTIVE);
  const vh = innerHeight;

  const refs = [];
  const lines = [];
  let more = 0;
  const add = (line) => {
    if (lines.length < MAX_LINES) lines.push(line);
    else more++;
  };
  const where = (el) => {
    const r = el.getBoundingClientRect();
    if (r.bottom < 0) return ' (above the window)';
    if (r.top > vh) return ' (below the window)';
    return '';
  };
  // Something else drawn over the middle of an element in the window.
  const coverOf = (el) => {
    const r = el.getBoundingClientRect();
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    if (x < 0 || y < 0 || x >= innerWidth || y >= vh) return null;
    const hit = document.elementFromPoint(x, y);
    return hit && hit !== el && !el.contains(hit) && !hit.contains(el) ? hit : null;
  };

  const nodes = document.body
    ? document.body.querySelectorAll('h1,h2,h3,h4,h5,h6,p,li,td,th,label,img,' + INTERACTIVE)
    : [];
  for (const el of nodes) {
    if (!shown(el)) continue;
    const tag = el.tagName.toLowerCase();
    if (isInteractive(el)) {
      // An interactive element inside another one is part of it.
      if (el.parentElement && el.parentElement.closest(INTERACTIVE)) continue;
      refs.push(el);
      const n = refs.length;
      const role = roleOf(el);
      let line = '[' + n + '] ' + role + ' "' + label(el) + '"';
      if (role === 'link') {
        const href = el.getAttribute('href') || '';
        line += ' → ' + clean(href, 80);
      } else if (role === 'textbox' || role === 'select') {
        line += ' value="' + clean(el.value, 60) + '"';
      } else if (role === 'checkbox' || role === 'radio') {
        line += el.checked ? ' (checked)' : ' (not checked)';
      }
      if (el.disabled) line += ' (disabled)';
      const cover = coverOf(el);
      if (cover) line += ' (covered by ' + describe(cover) + ': it can\'t be clicked)';
      add(line + where(el));
    } else if (/^h[1-6]$/.test(tag)) {
      add('heading ' + tag[1] + ' "' + clean(el.innerText, 100) + '"' + where(el));
    } else if (tag === 'img') {
      const alt = el.getAttribute('alt');
      let line = 'image "' + clean(alt || '', 60) + '"';
      if (el.complete && el.naturalWidth === 0) line += ' (broken: ' + clean(el.getAttribute('src'), 60) + ')';
      if (alt === null) line += ' (no alt text)';
      add(line + where(el));
    } else {
      // Text blocks, skipping ones that only wrap an interactive element.
      if (el.closest(INTERACTIVE)) continue;
      const text = clean(el.innerText, 160);
      if (!text) continue;
      const inner = el.querySelector(INTERACTIVE);
      if (inner && clean(inner.innerText, 160) === text) continue;
      add('text "' + text + '"' + where(el));
    }
  }
  window.__rcnRefs = refs;

  // Problems a reviewer would flag.
  const problems = [];
  const doc = document.documentElement;
  if (doc.scrollWidth > doc.clientWidth + 1) {
    problems.push(
      'The page is wider than the window (' + doc.scrollWidth + ' px content in a ' +
        doc.clientWidth + ' px window): it scrolls sideways.',
    );
    // The outermost elements that stick out on the right.
    const wide = [];
    for (const el of document.body ? document.body.querySelectorAll('*') : []) {
      const r = el.getBoundingClientRect();
      if (r.width > 0 && r.right > doc.clientWidth + 1 && getComputedStyle(el).position !== 'fixed') {
        if (!wide.some((w) => w.contains(el))) wide.push(el);
        if (wide.length >= 5) break;
      }
    }
    if (wide.length) problems.push('Sticking out on the right: ' + wide.map(describe).join(', '));
  }
  if (innerWidth > screen.width + 1 && !document.querySelector('meta[name=viewport]')) {
    problems.push(
      'No viewport meta tag: on a phone this page is laid out ' + innerWidth +
        ' px wide and shown zoomed out. Add <meta name="viewport" content="width=device-width, initial-scale=1"> to <head>.',
    );
  }
  const broken = [...document.images].filter((i) => i.complete && i.naturalWidth === 0);
  if (broken.length) {
    problems.push(
      broken.length + ' image(s) failed to load: ' +
        broken.slice(0, 5).map((i) => clean(i.getAttribute('src'), 60)).join(', '),
    );
  }

  const head = [
    'Page: "' + clean(document.title, 100) + '" — ' + location.href,
    'Window ' + innerWidth + '×' + innerHeight + ' px; page ' + doc.scrollHeight +
      ' px high, scrolled to ' + Math.round(scrollY) + '.',
  ];
  if (problems.length) head.push('Problems:\n- ' + problems.join('\n- '));
  head.push('Elements (click or type with their [number]):');
  if (more) lines.push('(+' + more + ' more — scroll and snapshot again to see further down)');

  // Element lookup for clicks: scroll it into view and say where it is,
  // where its link goes, and whether something else covers it.
  window.__rcnTarget = (n) => {
    const el = (window.__rcnRefs || [])[n - 1];
    if (!el || !el.isConnected) return { error: 'gone' };
    el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
    const r = el.getBoundingClientRect();
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    const hit = document.elementFromPoint(x, y);
    const covered = hit && hit !== el && !el.contains(hit) && !hit.contains(el) ? describe(hit) : null;
    const a = el.closest('a[href]');
    return {
      x,
      y,
      href: a ? a.href : null,
      newTab: !!(a && a.target === '_blank'),
      label: roleOf(el) + ' "' + label(el) + '"',
      covered,
    };
  };

  return head.join('\n') + '\n' + (lines.length ? lines.join('\n') : '(nothing visible)');
}
