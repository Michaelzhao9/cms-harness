// cms-harness admin: the one script the admin loads. Every page works without it; this only enhances:
// the chat (sending without a reload, the event stream, sizes, toasts), the site preview (history, change
// numbers, pick mode) and forms that show their buttons once something changed. Plain ES2020, no build step.
// The page's language (settings → 语言 / Language): Chinese text, or the English beside it.
const tr = (zh, en) => (document.documentElement.lang === 'en' ? en : zh);

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
document.documentElement.classList.add('js');

const shell = $('.shell');
const chat = $('[data-chat]');
const csrf = $('input[name=csrf]')?.value || '';

/** Posts a form body from the script: 204 or a plain-text error. */
async function send(url, body) {
  const res = await fetch(url, { method: 'POST', body, headers: { 'x-hc-fetch': '1' }, credentials: 'same-origin' });
  if (!res.ok) throw new Error((await res.text()) || tr(`出错了（${res.status}）`, `Something went wrong (${res.status})`));
}

function form(fields) {
  const body = new URLSearchParams({ csrf });
  for (const [k, v] of Object.entries(fields)) body.set(k, v);
  return body;
}

// ---------- the chat ----------

const log = $('[data-log]');
const composer = $('[data-composer]');
const ask = $('[data-ask]');
const ctxInput = $('[data-ctx]');
const chipsBox = $('[data-chips]');
const errorBox = $('[data-chat-error]');
const baseTitle = document.title;
let running = shell?.classList.contains('busy') || false;
let picked = [];
let asked = false;

function mode() {
  return ['docked', 'floating', 'mini'].find((m) => shell.classList.contains(`chat-${m}`)) || 'docked';
}

function setMode(m, focus) {
  if (!shell || mode() === m) return;
  shell.classList.remove('chat-docked', 'chat-floating', 'chat-mini');
  shell.classList.add(`chat-${m}`);
  send('/admin/prefs', form({ chat: m })).catch(() => {});
  if (focus && m !== 'mini') setTimeout(() => ask?.focus(), 0);
}

function openLog(open) {
  chat?.classList.toggle('open', open && log && log.children.length > 0 && !!log.textContent.trim());
}

function showError(text) {
  if (!errorBox) return;
  errorBox.textContent = text || '';
  errorBox.hidden = !text;
}

/** Asks for notification permission once, and only after the admin did something in the chat. */
function askToNotify() {
  if (asked || !('Notification' in window) || Notification.permission !== 'default') return;
  asked = true;
  Notification.requestPermission().catch(() => {});
}

function ctxText() {
  const base = ctxInput?.value || '';
  if (!picked.length) return base;
  const items = picked.map((p, i) => `${i + 1}) ${p.selector}「${p.text}」${p.change ? `（第 ${p.change} 处改动）` : ''}`);
  return `${base}；管理员在预览里选中了 ${picked.length} 个元素（页面 ${picked[0].path}）：${items.join('；')}`;
}

function renderChips() {
  if (!chipsBox) return;
  chipsBox.replaceChildren(
    ...picked.map((p, i) => {
      const chip = document.createElement('span');
      chip.className = 'chip';
      const label = document.createElement('span');
      label.textContent = p.text ? p.text.slice(0, 24) : p.selector;
      const x = document.createElement('button');
      x.type = 'button';
      x.setAttribute('aria-label', tr('去掉这个元素', 'Remove this element'));
      x.textContent = '×';
      x.addEventListener('click', () => {
        picked.splice(i, 1);
        renderChips();
      });
      chip.append(label, x);
      return chip;
    }),
  );
  chipsBox.hidden = picked.length === 0;
}

async function sendMessage(body) {
  showError('');
  askToNotify();
  try {
    await send('/admin/chat', body);
  } catch (e) {
    showError(e.message);
    return false;
  }
  setRunning(true);
  listen();
  return true;
}

function setRunning(on) {
  running = on;
  shell?.classList.toggle('busy', on);
  if (ask) {
    ask.disabled = on;
    ask.placeholder = on ? tr('正在处理…', 'Working…') : tr('想改什么，直接说。图片可以附上。', 'Say what to change. You can attach images.');
  }
  const sendBtn = $('.send', composer || document);
  if (sendBtn) sendBtn.disabled = on;
}

composer?.addEventListener('submit', async (e) => {
  const by = e.submitter;
  const action = by?.getAttribute('formaction');
  e.preventDefault();
  if (action === '/admin/prefs') return setMode(by.value, true);
  if (action === '/admin/chat/stop') return send('/admin/chat/stop', form({})).catch((err) => showError(err.message));
  const body = new FormData(composer);
  body.set('ctx', ctxText());
  if (!String(body.get('text') || '').trim() && !$('[data-files]')?.files.length) return;
  if (await sendMessage(body)) {
    composer.reset();
    picked = [];
    renderChips();
    if (ctxInput) ctxInput.value = currentCtx;
    if (mode() === 'floating') openLog(true);
  }
});

ask?.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    composer.requestSubmit();
  }
});
ask?.addEventListener('focus', () => mode() === 'floating' && openLog(true));
chat?.addEventListener('click', askToNotify);

// Forms inside the log (example prompts, ask_user options) and the chat's own small forms.
document.addEventListener('submit', async (e) => {
  const f = e.target;
  if (log?.contains(f) && f.action.endsWith('/admin/chat')) {
    e.preventDefault();
    sendMessage(new FormData(f, e.submitter));
  } else if (f.matches('[data-new]')) {
    e.preventDefault();
    await send('/admin/chat/new', new FormData(f)).catch((err) => showError(err.message));
    fetchView();
  } else if (f.matches('[data-size], [data-mini]') && e.submitter?.name === 'chat') {
    e.preventDefault();
    setMode(e.submitter.value, true);
    if (mode() === 'floating') openLog(running ? false : true);
  }
});
$('[data-log-close]')?.addEventListener('click', () => openLog(false));

document.addEventListener('keydown', (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k' && chat) {
    e.preventDefault();
    if (mode() === 'mini') setMode('floating', true);
    else ask?.focus();
  }
  if (e.key === 'Escape') {
    if (pickOn) setPick(false);
    $$('details[data-menu][open]').forEach((d) => d.removeAttribute('open'));
  }
});

// ---------- the event stream ----------

let source = null;

/** Opens the stream only while a turn runs: each tab otherwise holds no connection. */
function listen() {
  if (source || !shell) return;
  source = new EventSource(`/admin/chat/events?since=${encodeURIComponent(shell.dataset.chatId || '0-0')}`);
  source.onmessage = (e) => apply(JSON.parse(e.data));
}

function fetchView() {
  // One answer of the stream, right away: the id it is given is never the current one.
  const once = new EventSource('/admin/chat/events?since=0-x');
  once.onmessage = (e) => {
    once.close();
    apply(JSON.parse(e.data));
  };
  once.onerror = () => once.close();
}

function apply(view) {
  if (!shell || !log) return;
  const was = running;
  shell.dataset.chatId = view.id;
  const open = new Set($$('details[data-k][open]', log).map((d) => d.dataset.k));
  const nearBottom = log.scrollHeight - log.scrollTop - log.clientHeight < 80;
  log.innerHTML = view.html;
  chat.classList.toggle('has-log', !!$('.turn', log));
  for (const d of $$('details[data-k]', log)) if (open.has(d.dataset.k)) d.open = true;
  if (nearBottom || view.running) log.scrollTop = log.scrollHeight;
  markActive();
  setRunning(view.running);
  if (!view.model && ask) ask.disabled = true;
  for (const el of $$('[data-status-label]')) el.textContent = view.status.label;
  const step = $('[data-status-step]');
  if (step) step.textContent = view.status.step ? tr(`第 ${view.status.step} 步`, `Step ${view.status.step}`) : '';
  const miniStep = $('[data-mini-step]');
  if (miniStep) miniStep.textContent = view.status.step || '';
  setPending(view.pending);
  if (!view.running && source) {
    source.close();
    source = null;
  }
  if (was && !view.running) ended(view);
}

function setPending(n) {
  for (const el of $$('[data-publish]')) {
    el.hidden = !n;
    const c = $('.count', el);
    if (c) c.textContent = n;
  }
  const badge = $('[data-pending-badge]');
  if (badge) {
    badge.hidden = !n;
    badge.textContent = n;
  }
}

/** A turn finished: reload the preview, then say so where the admin will see it. */
function ended(view) {
  reloadFrames();
  if (view.stopped) return;
  const failed = !!view.error;
  const n = view.summary.changes;
  const text = failed ? view.error : n ? tr(`${view.summary.places}，${n} 处改动，都是草稿。`, `${view.summary.places}: ${n} changes, all drafts.`) : tr('这一轮做完了。', 'This turn is done.');
  if (mode() !== 'docked' || document.hidden) toast(failed ? tr('没做成', 'Not done') : tr('改好了', 'Done'), text, failed, n > 0);
  if (document.hidden) {
    document.title = `${failed ? '! ' : '✓ '}${baseTitle}`;
    if ('Notification' in window && Notification.permission === 'granted') {
      try {
        new Notification(failed ? tr('对话没做成', 'The chat could not finish') : tr('改好了', 'Done'), { body: text, tag: 'hc-chat' });
      } catch (_) {}
    }
  }
}

document.addEventListener('visibilitychange', () => {
  if (!document.hidden) document.title = baseTitle;
});

function toast(title, text, failed, withChanges) {
  const box = $('[data-toasts]');
  if (!box) return;
  const t = document.createElement('div');
  t.className = `toast${failed ? ' bad' : ''}`;
  t.setAttribute('role', 'status');
  const top = document.createElement('div');
  top.className = 'top';
  const badge = document.createElement('span');
  badge.className = 'badge';
  badge.textContent = failed ? '!' : '✓';
  const strong = document.createElement('strong');
  strong.textContent = title;
  const close = document.createElement('button');
  close.type = 'button';
  close.className = 'ib sm';
  close.setAttribute('aria-label', tr('关闭', 'Close'));
  close.textContent = '×';
  close.addEventListener('click', () => t.remove());
  top.append(badge, strong, close);
  const p = document.createElement('p');
  p.textContent = text;
  const acts = document.createElement('div');
  acts.className = 'acts';
  if (withChanges) {
    const look = document.createElement('button');
    look.type = 'button';
    look.className = 'btn';
    look.textContent = tr('看看改了哪里', 'See what changed');
    look.addEventListener('click', () => {
      t.remove();
      const first = $$('.turn', log).pop()?.querySelector('.chg');
      if (first) goToChange(first);
    });
    acts.append(look);
  }
  const openChat = document.createElement('button');
  openChat.type = 'button';
  openChat.className = 'btn2';
  openChat.textContent = tr('打开对话', 'Open chat');
  openChat.addEventListener('click', () => {
    t.remove();
    if (mode() === 'mini') setMode('floating', true);
    if (mode() === 'floating') openLog(true);
    ask?.focus();
  });
  acts.append(openChat);
  t.append(top, p, acts);
  box.append(t);
  setTimeout(() => t.remove(), 12000);
}

if (running) listen();

// ---------- the site preview ----------

const frame = $('[data-frame=draft]');
const stage = $('[data-stage]');
const addr = $('[data-addr]');
let currentCtx = ctxInput?.value || '';
let page = { path: null, changes: [] };
const seen = new Map(); // page path -> change numbers seen there
let want = null; // a change to show once its page is ready
let navIndex = 0;
let pickOn = false;
let marksOn = !stage?.classList.contains('nomarks');
// Drafts come from the admin's preview entry (/_hc/preview/<token>); 线上 is the live site ('' or its origin).
const previewBase = stage?.dataset.preview || '';
const liveBase = stage?.dataset.live || '';

function post(msg) {
  frame?.contentWindow?.postMessage(msg, '*');
}

function markColor() {
  return marksOn ? getComputedStyle(document.body).getPropertyValue('--mark').trim() || '#f5b700' : 'transparent';
}

function navTo(path) {
  if (!frame) return;
  frame.src = previewBase + path;
}

function reloadFrames() {
  for (const f of $$('[data-frame]')) {
    const path = page.path || stage?.dataset.path || '/';
    f.src = (f.dataset.frame === 'live' ? liveBase : previewBase) + path;
  }
}

function activate(n) {
  post({ type: 'hc:activate', n });
  const i = page.changes.indexOf(n);
  if (i >= 0) navIndex = i;
  updateNavigator();
  markActive(n);
}

function markActive(n = page.active) {
  page.active = n;
  for (const a of $$('.chg', log || document)) a.classList.toggle('on', n != null && a.dataset.n === String(n));
}

/** Shows a change: on this page, on a page where it was seen, on its own page, else follows its link. */
function goToChange(a) {
  const n = a.dataset.n ? Number(a.dataset.n) : null;
  if (!frame || n == null) return (location.href = a.href);
  if (page.changes.includes(n)) return activate(n);
  const where = [...seen].find(([, list]) => list.includes(n))?.[0] || a.dataset.page;
  if (!where) return (location.href = a.href);
  want = n;
  navTo(where);
}

document.addEventListener('click', (e) => {
  const a = e.target.closest?.('a.chg');
  if (a && frame) {
    e.preventDefault();
    goToChange(a);
  }
  for (const d of $$('details[data-menu][open]')) if (!d.contains(e.target)) d.removeAttribute('open');
});

function updateNavigator() {
  const nav = $('[data-navigator]');
  if (!nav) return;
  const n = page.changes.length;
  nav.hidden = n === 0 || !marksOn;
  $('[data-nav-text]', nav).textContent = tr(`本页 ${n} 处改动，还没发布`, `${n} unpublished changes on this page`);
  $('[data-nav-pos]', nav).textContent = `${Math.min(navIndex + 1, n)} / ${n}`;
}

$('[data-nav-prev]')?.addEventListener('click', () => {
  const n = page.changes.length;
  if (n) activate(page.changes[(navIndex - 1 + n) % n]);
});
$('[data-nav-next]')?.addEventListener('click', () => {
  const n = page.changes.length;
  if (n) activate(page.changes[(navIndex + 1) % n]);
});

function setHistory(canBack, canForward) {
  const back = $('[data-back]');
  const fwd = $('[data-forward]');
  if (back) back.disabled = !canBack;
  if (fwd) fwd.disabled = !canForward;
}

function setPage(path) {
  page.path = path;
  if (addr && document.activeElement !== addr) addr.value = path;
  currentCtx = `page:${path}`;
  if (ctxInput) ctxInput.value = currentCtx;
  const strong = $('[data-address] strong');
  if (strong) strong.hidden = path !== '/';
  const open = $('[data-open]');
  if (open) open.href = previewBase + path;
  const url = new URL(location.href);
  url.searchParams.set('p', path);
  history.replaceState(null, '', url);
}

window.addEventListener('message', (e) => {
  if (!frame || e.source !== frame.contentWindow || typeof e.data !== 'object' || !e.data) return;
  const m = e.data;
  if (m.type === 'hc:navigated') {
    setPage(m.path);
    setHistory(m.canBack, m.canForward);
  } else if (m.type === 'hc:ready') {
    setPage(m.path);
    setHistory(m.canBack, m.canForward);
    page.changes = Array.isArray(m.changes) ? m.changes : [];
    seen.set(m.path, page.changes);
    navIndex = 0;
    const dot = $('[data-page-dot]');
    if (dot) dot.hidden = page.changes.length === 0;
    post({ type: 'hc:mark-color', color: markColor() });
    if (pickOn) post({ type: 'hc:pick', on: true });
    updateNavigator();
    if (want != null && page.changes.includes(want)) activate(want);
    want = null;
  } else if (m.type === 'hc:picked') {
    picked = (m.items || []).slice(0, 10);
    renderChips();
    if (mode() === 'mini') setMode('floating', true);
    else ask?.focus();
  }
});

$('[data-back]')?.addEventListener('click', () => post({ type: 'hc:back' }));
$('[data-forward]')?.addEventListener('click', () => post({ type: 'hc:forward' }));
$('[data-reload]')?.addEventListener('click', (e) => {
  if (!frame) return;
  e.preventDefault();
  reloadFrames();
});
$('[data-marks]')?.addEventListener('click', (e) => {
  if (!frame) return;
  e.preventDefault();
  marksOn = !marksOn;
  e.currentTarget.setAttribute('aria-pressed', String(marksOn));
  stage.classList.toggle('nomarks', !marksOn);
  post({ type: 'hc:mark-color', color: markColor() });
  if (!marksOn) post({ type: 'hc:activate', n: null });
  updateNavigator();
  const url = new URL(location.href);
  url.searchParams.set('marks', marksOn ? '1' : '0');
  history.replaceState(null, '', url);
});

const pickBtn = $('[data-pick]');
function setPick(on) {
  pickOn = on;
  pickBtn?.setAttribute('aria-pressed', String(on));
  post({ type: 'hc:pick', on });
}
if (pickBtn) {
  if (!frame) pickBtn.hidden = true;
  pickBtn.addEventListener('click', () => setPick(!pickOn));
}

// ---------- the canvas: the bar for the element selected in the preview ----------
// The preview reports what was selected (hc:select) and text typed into it (hc:text). It shares its frame with
// the site's own scripts, so nothing it says is trusted: this page sends each edit with its CSRF token and the
// server checks it. Style edits show at once (hc:style) and are saved as drafts.

const cbar = $('[data-cbar]');
let csel = null; // {src, tag, text, rect, style}
let tierOnly = false;

function canvasTier() {
  return tierOnly ? cbar.querySelector('[data-c-tier]').dataset.tier : 'all';
}

async function canvasSave(url, fields) {
  const res = await fetch(url, { method: 'POST', body: form(fields), headers: { 'x-hc-fetch': '1' }, credentials: 'same-origin' });
  if (!res.ok) throw new Error((await res.text()) || tr(`出错了（${res.status}）`, `Something went wrong (${res.status})`));
  const out = await res.json();
  setPending(out.pending);
  return out;
}

function hex(rgb) {
  const m = String(rgb).match(/\d+(\.\d+)?/g);
  if (!m || m.length < 3) return '#000000';
  if (m.length === 4 && Number(m[3]) === 0) return '#ffffff';
  return '#' + m.slice(0, 3).map((n) => Math.round(Number(n)).toString(16).padStart(2, '0')).join('');
}

// An element of a shared template (a layout, an include) changes on every page using it, unless "only this
// page" is on: then edits carry the page's address.
let onlyThisPage = false;
const pageAddress = () => (page.path || '/').split(/[?#]/)[0];

/** Shows one property in the preview at once and saves it as a draft; returns what it was (for undo). */
async function canvasPut(src, tier, prop, value, scope) {
  post({ type: 'hc:style', src, prop, value });
  // The stylesheet gives a border width its solid style; the preview needs it too until it reloads.
  if (prop === 'border-width') post({ type: 'hc:style', src, prop: 'border-style', value: value ? 'solid' : '' });
  const out = await canvasSave('/admin/canvas/style', { src, tier, prop, value, page: scope || '' });
  return out.old ?? '';
}

// Undo and redo: each step is a list of {src, tier, prop, old, value, scope}.
const undone = [];
const done = [];

/** Applies a step's edits, saving each; on success it can be undone. */
async function canvasStep(src, edits) {
  const step = [];
  const scope = onlyThisPage ? pageAddress() : '';
  try {
    for (const { tier, prop, value } of edits) step.push({ src, tier, prop, value, scope, old: await canvasPut(src, tier, prop, value, scope) });
  } catch (err) {
    toast(tr('没存上', 'Not saved'), err.message, true);
  }
  if (step.length) {
    done.push(step);
    undone.length = 0;
  }
}

async function canvasSet(prop, value) {
  if (csel) await canvasStep(csel.src, [{ tier: canvasTier(), prop, value }]);
}

async function canvasUndo(redo) {
  const step = (redo ? undone : done).pop();
  if (!step) return;
  try {
    for (const e of redo ? step : [...step].reverse()) await canvasPut(e.src, e.tier, e.prop, redo ? e.value : e.old, e.scope);
    (redo ? done : undone).push(step);
  } catch (err) {
    toast(tr('没撤销成', 'Could not undo'), err.message, true);
  }
}

/** Uploads an image for the canvas: added at `src` (`mode` add) or put into the `<img>` at `src` (replace). */
async function canvasImage(src, mode, file) {
  const body = new FormData();
  body.set('csrf', csrf);
  body.set('src', src);
  body.set('mode', mode);
  body.set('file', file);
  const res = await fetch('/admin/canvas/image', { method: 'POST', body, headers: { 'x-hc-fetch': '1' }, credentials: 'same-origin' });
  if (!res.ok) throw new Error((await res.text()) || tr(`出错了（${res.status}）`, `Something went wrong (${res.status})`));
  const out = await res.json();
  setPending(out.pending);
  return out;
}

/** Takes the selected element out of its template, or copies it, then shows the page again. */
async function canvasElement(action) {
  if (!csel) return;
  try {
    await canvasSave('/admin/canvas/element', { src: csel.src, action });
    closeBar();
    reloadFrames();
  } catch (err) {
    toast(action === 'remove' ? tr('没删成', 'Not deleted') : tr('没复制成', 'Not copied'), err.message, true);
  }
}

const width = () => cbar.querySelector('[data-c-tier]').dataset.tier;

const PRESETS = {
  plain: { color: '', 'background-color': '', 'border-width': '', 'border-color': '', 'box-shadow': '' },
  dark: { color: '#ffffff', 'background-color': '#1d1d1f', 'border-width': '', 'box-shadow': '' },
  light: { color: '#1d1d1f', 'background-color': '#f5f5f7', 'border-width': '', 'box-shadow': '' },
  accent: { color: '#1d1d1f', 'background-color': '#f5b700', 'border-width': '', 'box-shadow': '' },
  outline: { 'background-color': 'transparent', 'border-width': '1.5px', 'border-color': '#1d1d1f', 'box-shadow': '' },
};
const ALIGNS = ['left', 'center', 'right'];

// The desktop preview is never narrower than a desktop (DESKTOP px, where the site's desktop edits apply): a
// narrower pane shows it scaled down, the way a thumbnail would. Tablet and phone previews are their real width.
const DESKTOP = 1024;
let frameScale = 1;
function fitFrames() {
  frameScale = 1;
  for (const f of $$('.preview')) {
    const box = f.parentElement;
    const fit = stage.classList.contains('w-desktop') && box.clientWidth < DESKTOP;
    const s = fit ? box.clientWidth / DESKTOP : 1;
    f.style.flex = fit ? 'none' : '';
    f.style.width = fit ? `${DESKTOP}px` : '';
    f.style.height = fit ? `${box.clientHeight / s}px` : '';
    f.style.transform = fit ? `scale(${s})` : '';
    f.style.transformOrigin = fit ? '0 0' : '';
    box.style.overflow = fit ? 'hidden' : '';
    box.style.justifyContent = fit ? 'flex-start' : '';
    if (f === frame) frameScale = s;
  }
}
if (stage) {
  fitFrames();
  new ResizeObserver(fitFrames).observe(stage);
}

function placeBar() {
  if (!cbar || !csel || !frame) return;
  const fr = frame.getBoundingClientRect();
  const st = stage.getBoundingClientRect();
  const s = frameScale;
  const r = { x: csel.rect.x * s, y: csel.rect.y * s, w: csel.rect.w * s, h: csel.rect.h * s };
  const h = cbar.offsetHeight || 40;
  const w = cbar.offsetWidth || 420;
  let top = fr.top - st.top + r.y - h - 14;
  if (top < 8) top = fr.top - st.top + r.y + r.h + 40;
  const left = Math.max(8, Math.min(fr.left - st.left + r.x - 4, st.width - w - 8));
  cbar.style.top = `${Math.round(top)}px`;
  cbar.style.left = `${Math.round(left)}px`;
}

function fillBar() {
  const s = csel.style;
  $('[data-c-size]', cbar).value = parseInt(s.fontSize, 10) || '';
  for (const b of $$('[data-c-toggle]', cbar)) {
    const prop = b.dataset.cToggle;
    const cur = prop === 'font-weight' ? (Number(s.fontWeight) >= 600 ? '700' : '400') : prop === 'font-style' ? s.fontStyle : s.textDecorationLine;
    b.setAttribute('aria-pressed', String(cur === b.dataset.on));
  }
  for (const [prop, key] of [['color', 'color'], ['background-color', 'backgroundColor']]) {
    $(`[data-c-dot="${prop}"]`, cbar).style.background = s[key];
    $(`[data-c-color="${prop}"]`, cbar).value = hex(s[key]);
  }
  const tierBtn = $('[data-c-tier]', cbar);
  const names = { desktop: tr('只改电脑', 'Desktop only'), tablet: tr('只改平板', 'Tablet only'), mobile: tr('只改手机', 'Phone only') };
  tierBtn.textContent = tierOnly ? names[tierBtn.dataset.tier] : tr('所有宽度', 'All widths');
  tierBtn.setAttribute('aria-pressed', String(tierOnly));
  const scope = $('[data-c-scope]', cbar);
  const pages = csel.pages || 1;
  scope.hidden = pages < 2;
  scope.textContent = onlyThisPage ? tr('只改这一页', 'This page only') : tr(`共用 · ${pages} 页`, `Shared · ${pages} pages`);
  scope.setAttribute('aria-pressed', String(onlyThisPage));
  scope.title = tr('这个元素在共用的模板里，改了会影响所有用它的页面。点一下改成只改这一页。', 'This element is in a shared template: edits change every page using it. Click to edit this page only.');
  $('[data-c-image]', cbar).hidden = csel.tag !== 'img';
  // A narrower width with edits of its own can go back to what the wider width shows.
  $('[data-c-reset]', cbar).hidden = width() === 'desktop' || !(csel.tiers || []).includes(width());
  placeBar();
}

/** Asks how many pages share the selected element and where it has edits of its own. */
async function canvasInfo() {
  const sel = csel;
  if (!sel) return;
  try {
    const res = await fetch(`/admin/canvas/info?src=${encodeURIComponent(sel.src)}&page=${encodeURIComponent(pageAddress())}`, { credentials: 'same-origin' });
    if (!res.ok || csel !== sel) return;
    Object.assign(sel, await res.json());
    post({ type: 'hc:shared', on: sel.pages > 1 });
    fillBar();
  } catch {
    // The bar works without it.
  }
}

function closeBar() {
  csel = null;
  if (cbar) cbar.hidden = true;
}

if (cbar && frame) {
  window.addEventListener('message', (e) => {
    if (e.source !== frame.contentWindow || typeof e.data !== 'object' || !e.data) return;
    const m = e.data;
    if (m.type === 'hc:select' && typeof m.src === 'string') {
      const same = csel?.src === m.src;
      csel = { src: m.src, tag: String(m.tag || ''), text: String(m.text || ''), rect: m.rect || {}, style: m.style || {}, pages: same ? csel.pages : 1, tiers: same ? csel.tiers : [] };
      if (!same) onlyThisPage = false;
      fillBar();
      cbar.hidden = false;
      placeBar();
      canvasInfo();
    } else if (m.type === 'hc:rect' && csel && m.rect) {
      csel.rect = m.rect;
      placeBar();
    } else if (m.type === 'hc:deselect' || m.type === 'hc:navigated' || m.type === 'hc:ready') {
      closeBar();
    } else if (m.type === 'hc:text' && typeof m.src === 'string' && typeof m.text === 'string') {
      canvasSave('/admin/canvas/text', { src: m.src, text: m.text }).catch((err) => {
        toast(tr('文字没改成', 'Text not changed'), err.message, true);
        reloadFrames();
      });
    } else if (m.type === 'hc:move' && typeof m.src === 'string') {
      // Where things sit is an edit of the width being looked at.
      canvasStep(m.src, [{ tier: width(), prop: 'translate', value: `${Number(m.x) || 0}px ${Number(m.y) || 0}px` }]);
    } else if (m.type === 'hc:resize' && typeof m.src === 'string') {
      const edits = [];
      if (m.w != null) edits.push({ tier: width(), prop: 'width', value: `${Number(m.w)}px` });
      if (m.h != null) edits.push({ tier: width(), prop: 'height', value: `${Number(m.h)}px` });
      edits.push({ tier: width(), prop: 'translate', value: `${Number(m.x) || 0}px ${Number(m.y) || 0}px` });
      canvasStep(m.src, edits);
    } else if (m.type === 'hc:key') {
      if (m.key === 'undo' || m.key === 'redo') canvasUndo(m.key === 'redo');
      else if (m.key === 'delete') canvasElement('remove');
      else if (m.key === 'duplicate') canvasElement('duplicate');
    }
  });
  window.addEventListener('resize', placeBar);
  $('[data-c-size]', cbar).addEventListener('change', (e) => {
    const n = Number(e.target.value);
    if (n >= 6 && n <= 400) canvasSet('font-size', `${n}px`);
  });
  for (const b of $$('[data-c-toggle]', cbar)) {
    b.addEventListener('click', () => {
      const on = b.getAttribute('aria-pressed') !== 'true';
      b.setAttribute('aria-pressed', String(on));
      canvasSet(b.dataset.cToggle, on ? b.dataset.on : b.dataset.off);
    });
  }
  for (const input of $$('[data-c-color]', cbar)) {
    input.addEventListener('input', () => { $(`[data-c-dot="${input.dataset.cColor}"]`, cbar).style.background = input.value; });
    input.addEventListener('change', () => canvasSet(input.dataset.cColor, input.value));
  }
  $('[data-c-align]', cbar).addEventListener('click', () => {
    const next = ALIGNS[(ALIGNS.indexOf(csel?.style.textAlign) + 1) % ALIGNS.length];
    if (csel) csel.style.textAlign = next;
    canvasSet('text-align', next);
  });
  for (const b of $$('[data-c-preset]', cbar)) {
    b.addEventListener('click', async () => {
      b.closest('details').removeAttribute('open');
      for (const [prop, value] of Object.entries(PRESETS[b.dataset.cPreset])) await canvasSet(prop, value);
    });
  }
  for (const b of $$('[data-c-set]', cbar)) {
    b.addEventListener('click', () => {
      b.closest('details').removeAttribute('open');
      canvasSet(b.dataset.cSet, b.dataset.v);
    });
  }
  $('[data-c-tier]', cbar).addEventListener('click', () => {
    tierOnly = !tierOnly;
    fillBar();
  });
  $('[data-c-scope]', cbar).addEventListener('click', () => {
    onlyThisPage = !onlyThisPage;
    fillBar();
  });
  $('[data-c-reset]', cbar).addEventListener('click', async () => {
    if (!csel) return;
    try {
      await canvasSave('/admin/canvas/reset', { src: csel.src, tier: width() });
      reloadFrames();
    } catch (err) {
      toast(tr('没恢复成', 'Not reset'), err.message, true);
    }
  });
  const cfile = $('[data-c-file]', cbar);
  $('[data-c-image]', cbar).addEventListener('click', () => cfile.click());
  cfile.addEventListener('change', async () => {
    const file = cfile.files[0];
    cfile.value = '';
    if (!file || !csel) return;
    try {
      await canvasImage(csel.src, 'replace', file);
      closeBar();
      reloadFrames();
    } catch (err) {
      toast(tr('图片没换成', 'Image not replaced'), err.message, true);
    }
  });
  $('[data-c-copy]', cbar).addEventListener('click', () => canvasElement('duplicate'));
  $('[data-c-remove]', cbar).addEventListener('click', () => canvasElement('remove'));
  for (const b of $$('[data-c-layer]', cbar)) {
    b.addEventListener('click', () => {
      b.closest('details').removeAttribute('open');
      if (!csel) return;
      const z = Math.max(-50, Math.min(50, (parseInt(csel.style.zIndex, 10) || 0) + Number(b.dataset.cLayer)));
      csel.style.zIndex = String(z);
      canvasStep(csel.src, [{ tier: width(), prop: 'z-index', value: String(z) }]);
    });
  }
  $('[data-c-hide]', cbar).addEventListener('click', (e) => {
    e.currentTarget.closest('details').removeAttribute('open');
    if (!csel) return;
    canvasStep(csel.src, [{ tier: width(), prop: 'display', value: 'none' }]);
    closeBar();
  });
  $('[data-c-ai]', cbar).addEventListener('click', () => {
    if (!csel) return;
    picked = [{ path: page.path || '/', selector: `[data-hc-src="${csel.src}"]`, text: csel.text, change: null }];
    renderChips();
    if (mode() === 'mini') setMode('floating', true);
    else ask?.focus();
  });
  document.addEventListener('keydown', (e) => {
    const typing = e.target.closest?.('input, textarea, [contenteditable]');
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'z' && !typing && (done.length || undone.length)) {
      e.preventDefault();
      canvasUndo(e.shiftKey);
    }
    if (e.key === 'Escape' && csel && !typing) {
      post({ type: 'hc:deselect' });
      closeBar();
    }
  });
}

// ---------- warnings: hand edits that run off the screen, cover content or have tiny text ----------

const warnBtn = $('[data-warn]');
let warnings = [];
let warnAt = 0;
const WARN_TEXT = {
  overflow: () => tr('超出了屏幕', 'runs off the screen'),
  overlap: () => tr('盖住了别的内容', 'covers other content'),
  small: () => tr('字太小', 'text is too small'),
};
if (warnBtn && frame) {
  window.addEventListener('message', (e) => {
    if (e.source !== frame.contentWindow || typeof e.data !== 'object' || !e.data) return;
    if (e.data.type === 'hc:warnings' && Array.isArray(e.data.items)) {
      warnings = e.data.items.filter((w) => typeof w.src === 'string' && WARN_TEXT[w.kind]).slice(0, 50);
      warnAt = 0;
      warnBtn.hidden = !warnings.length;
      $('[data-warn-n]', warnBtn).textContent = warnings.length;
      const list = warnings.map((w) => WARN_TEXT[w.kind]()).join(tr('；', '; '));
      warnBtn.title = tr(`这个宽度下：${list}。点一下看下一处。`, `At this width: ${list}. Click to see the next one.`);
    } else if (e.data.type === 'hc:ready') {
      warnings = [];
      warnBtn.hidden = true;
    }
  });
  warnBtn.addEventListener('click', () => {
    if (!warnings.length) return;
    const w = warnings[warnAt % warnings.length];
    warnAt += 1;
    post({ type: 'hc:focus', src: w.src });
    // Which problem this one is, in the button itself.
    $('[data-warn-n]', warnBtn).textContent = `${WARN_TEXT[w.kind]()} ${((warnAt - 1) % warnings.length) + 1}/${warnings.length}`;
  });
}

// ---------- the pen: tools that add elements, and undo/redo ----------

const pen = $('[data-pen]');
let placing = null; // an element just added, to put where the click was once the preview reloads
let penImage = null; // the image file chosen for the image tool, until the click that places it

if (pen && frame) {
  const tools = $('[data-pen-tools]', pen);
  const toggle = $('[data-pen-toggle]', pen);
  const pick = (kind) => {
    for (const b of $$('[data-tool]', pen)) b.setAttribute('aria-pressed', String(b.dataset.tool === kind));
    post({ type: 'hc:tool', kind });
  };
  const refreshUndo = () => {
    $('[data-undo]', pen).disabled = !done.length;
    $('[data-redo]', pen).disabled = !undone.length;
  };
  toggle.addEventListener('click', () => {
    const open = toggle.getAttribute('aria-expanded') !== 'true';
    toggle.setAttribute('aria-expanded', String(open));
    tools.hidden = !open;
    stage.classList.toggle('pen-open', open);
    if (!open) pick(null);
    refreshUndo();
  });
  const penFile = $('[data-pen-file]', pen);
  for (const b of $$('[data-tool]', pen)) {
    b.addEventListener('click', () => {
      if (b.getAttribute('aria-pressed') === 'true') return pick(null);
      // The image tool asks for the picture first; the click on the page then says where.
      if (b.dataset.tool === 'image') return penFile.click();
      pick(b.dataset.tool);
    });
  }
  penFile.addEventListener('change', () => {
    penImage = penFile.files[0] || null;
    penFile.value = '';
    if (penImage) pick('image');
  });
  $('[data-undo]', pen).addEventListener('click', () => canvasUndo(false).then(refreshUndo));
  $('[data-redo]', pen).addEventListener('click', () => canvasUndo(true).then(refreshUndo));
  setInterval(refreshUndo, 1000);
  window.addEventListener('message', (e) => {
    if (e.source !== frame.contentWindow || typeof e.data !== 'object' || !e.data) return;
    const m = e.data;
    if (m.type === 'hc:add' && typeof m.src === 'string') {
      pick(null);
      const adding = m.kind === 'image' && penImage ? canvasImage(m.src, 'add', penImage) : canvasSave('/admin/canvas/add', { src: m.src, kind: String(m.kind) });
      penImage = null;
      adding
        .then((out) => {
          placing = { src: out.src, x: m.x, y: m.y, sx: m.sx, sy: m.sy };
          reloadFrames();
        })
        .catch((err) => toast(tr('没加上', 'Not added'), err.message, true));
    } else if (m.type === 'hc:ready' && placing) {
      post(Object.assign({ type: 'hc:place' }, placing));
      placing = null;
    }
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && toggle.getAttribute('aria-expanded') === 'true') pick(null);
  });
}

// The publish toast and its query flag go away after a while.
const pubbed = $('[data-published]');
if (pubbed) {
  const url = new URL(location.href);
  url.searchParams.delete('published');
  history.replaceState(null, '', url);
  setTimeout(() => pubbed.remove(), 6000);
}

// ---------- forms that show their buttons only once changed ----------

function snapshot(f) {
  const out = new Map();
  for (const el of f.elements) {
    if (!el.name || el.type === 'hidden' || el.type === 'submit' || el.type === 'button') continue;
    const v = el.type === 'checkbox' || el.type === 'radio' ? (el.checked ? el.value : '') : el.type === 'file' ? [...el.files].map((x) => x.name).join(',') : el.multiple ? [...el.selectedOptions].map((o) => o.value).join(',') : el.value;
    out.set(el.name, (out.get(el.name) || '') + '\u0000' + v);
  }
  return out;
}

for (const f of $$('form[data-dirty]')) {
  const start = snapshot(f);
  const count = $('[data-dirty-count]', f);
  const check = () => {
    const now = snapshot(f);
    const n = [...now].filter(([k, v]) => k !== 'note' && start.get(k) !== v).length;
    f.classList.toggle('is-dirty', n > 0);
    if (count) {
      count.hidden = n === 0;
      count.textContent = tr(`改了 ${n} 处，还没保存`, `${n} unsaved changes`);
    }
  };
  f.addEventListener('input', check);
  f.addEventListener('change', check);
  f.addEventListener('reset', () => setTimeout(check, 0));
  check();
}

// ---------- settings: the chat model ----------

document.addEventListener('change', (e) => {
  if (e.target.id === 'm-svc') {
    const url = $('#m-url');
    if (url) url.placeholder = e.target.selectedOptions[0]?.dataset.url || 'https://…';
  }
});

// 检测 without leaving the page, so a key typed for it is still in the field when the admin saves.
document.addEventListener('submit', async (e) => {
  const f = e.target;
  if (!f.matches('[data-model-form]') || !e.submitter?.getAttribute('formaction')?.endsWith('/detect')) return;
  e.preventDefault();
  const key = $('#m-key', f)?.value || '';
  const res = await fetch('/admin/settings/model/detect', { method: 'POST', body: new FormData(f), credentials: 'same-origin' });
  const doc = new DOMParser().parseFromString(await res.text(), 'text/html');
  const fresh = $('#s-model', doc);
  const old = $('#s-model');
  const error = $('.settings > .note.bad', doc);
  if (!fresh || !old) return;
  if (error) fresh.prepend(error);
  old.replaceWith(fresh);
  const k = $('#m-key');
  if (k) k.value = key;
});

// Scheduled publishing: the time field is in the browser's zone, the server keeps UTC.
for (const tz of document.querySelectorAll('[data-tz]')) tz.value = String(-new Date().getTimezoneOffset());
for (const t of document.querySelectorAll('time[data-utc]')) {
  const d = new Date(t.getAttribute('datetime'));
  if (!Number.isNaN(d.getTime())) t.textContent = d.toLocaleString(document.documentElement.lang === 'en' ? 'en' : 'zh-CN', { dateStyle: 'medium', timeStyle: 'short' });
}
