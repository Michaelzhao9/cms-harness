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
