// cms-harness preview: draws change markers over the page and talks to the admin that frames it.
//
// The page is served under the admin's preview entry, /_hc/preview/<token>/… on the admin's own address, in a
// sandbox without allow-same-origin, so its own origin is opaque ("null"). It never reaches the admin's DOM: the
// only channel is postMessage with window.parent. Outgoing messages are addressed to the address it was served
// from (the admin's); incoming ones are accepted only from window.parent and from that origin.
// The server keeps the page's own absolute paths under the preview (attributes, CSS, the import map); this script
// does the same for fetch() and XMLHttpRequest, and reports paths without the prefix.
//
// admin -> preview: {type:"hc:activate", n} {type:"hc:pick", on} {type:"hc:back"} {type:"hc:forward"} {type:"hc:mark-color", color}
//                   {type:"hc:style", src, prop, value} {type:"hc:deselect"}
// preview -> admin: {type:"hc:ready", path, changes, canBack, canForward} {type:"hc:picked", items:[{path, selector, text, change}]}
//                   {type:"hc:navigated", path, canBack, canForward}
//                   {type:"hc:select", src, tag, text, rect, style, menu} {type:"hc:rect", rect} {type:"hc:deselect"} {type:"hc:text", src, text}
// The canvas (selecting, editing text) only reports: the admin page checks and saves every edit.
(() => {
  "use strict";
  const ORIGIN = new URL(location.href).origin;
  const framed = window.parent !== window;
  const script = document.currentScript;
  const pageChanges = ((script && script.dataset.hcPage) || "").split(",").filter(Boolean).map(Number);
  const BASE = (script && script.dataset.hcBase) || "";

  const send = (msg) => { if (framed) window.parent.postMessage(msg, ORIGIN); };

  // An absolute site path, moved under the preview; server routes and other hosts stay.
  const SITE_PATH = /^\/(?![\/\\]|(?:admin|api|uploads|_hc|mcp)(?:[\/?#]|$))/;
  const inPreview = (u) => (typeof u === "string" && BASE && SITE_PATH.test(u) ? BASE + u : u);
  const realFetch = window.fetch;
  if (realFetch) window.fetch = function (input, init) { return realFetch.call(window, inPreview(input), init); };
  const realOpen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (method, url, ...rest) { return realOpen.call(this, method, inPreview(url), ...rest); };

  // The page's path as a visitor sees it: without the preview's prefix.
  const path = () => {
    const u = new URL(location.href);
    const p = BASE && u.pathname.startsWith(BASE) ? u.pathname.slice(BASE.length) || "/" : u.pathname;
    return p + u.search + u.hash;
  };

  // ---- history: the frame's own stack, kept in window.name (it survives navigations; storage does not exist here) ----
  const KEY = "hc-preview:";
  let hist = { stack: [], i: -1 };
  try { if (window.name.startsWith(KEY)) hist = JSON.parse(window.name.slice(KEY.length)); } catch (e) { /* start over */ }
  const navType = (performance.getEntriesByType("navigation")[0] || {}).type;
  const here = path();
  const firstLoad = hist.i < 0;
  if (navType === "back_forward" && hist.stack[hist.i - 1] === here) hist.i -= 1;
  else if (navType === "back_forward" && hist.stack[hist.i + 1] === here) hist.i += 1;
  else if (navType === "reload" && hist.stack[hist.i] === here) { /* same entry */ }
  else { hist.stack = hist.stack.slice(0, hist.i + 1); hist.stack.push(here); hist.i = hist.stack.length - 1; }
  const save = () => { try { window.name = KEY + JSON.stringify(hist); } catch (e) { /* ignore */ } };
  save();
  const where = () => ({ path: path(), canBack: hist.i > 0, canForward: hist.i < hist.stack.length - 1 });

  // ---- markers ----
  const overlay = document.createElement("div");
  overlay.id = "hc-overlay";
  let selected = null; // chosen by the admin (hc:activate)
  let pointed = null; // under the pointer
  let picking = false;
  let hovered = null;
  let picked = [];

  const marks = () => Array.from(document.querySelectorAll("[data-hc-change]"));
  // Spans may break across lines and regions have no box of their own (display:contents): measure the contents.
  const rectOf = (el) => {
    const r = document.createRange();
    r.selectNodeContents(el);
    const box = r.getBoundingClientRect();
    return box.width || box.height ? box : el.getBoundingClientRect();
  };
  const place = (node, box, pad) => {
    node.style.left = (box.left - pad) + "px";
    node.style.top = (box.top - pad) + "px";
    node.style.width = (box.width + pad * 2) + "px";
    node.style.height = (box.height + pad * 2) + "px";
  };

  const draw = () => {
    overlay.textContent = "";
    for (const el of marks()) {
      const n = Number(el.dataset.hcChange);
      const active = n === selected || n === pointed;
      const box = rectOf(el);
      if (!box.width && !box.height) continue;
      const ring = document.createElement("div");
      ring.className = "hc-box" + (active ? " active" : "");
      place(ring, box, 2);
      overlay.appendChild(ring);
      const tip = document.createElement("div");
      tip.className = active ? "hc-pin" : "hc-dot";
      if (active) tip.textContent = String(n);
      tip.style.left = (box.left - 2) + "px";
      tip.style.top = (box.top - 2) + "px";
      overlay.appendChild(tip);
    }
    if (picking && hovered) {
      const h = document.createElement("div");
      h.className = "hc-hover";
      place(h, hovered.getBoundingClientRect(), 0);
      overlay.appendChild(h);
    }
    if (sel && sel.isConnected) {
      for (const el of document.querySelectorAll(`[data-hc-src="${CSS.escape(sel.dataset.hcSrc)}"]`)) {
        if (el === sel) continue;
        const o = document.createElement("div");
        o.className = "hc-same";
        place(o, el.getBoundingClientRect(), 0);
        overlay.appendChild(o);
      }
      const b = sel.getBoundingClientRect();
      const box = document.createElement("div");
      box.className = "hc-sel";
      place(box, b, 3);
      overlay.appendChild(box);
      if (!editing) {
        for (const [x, y] of [[0, 0], [.5, 0], [1, 0], [0, .5], [1, .5], [0, 1], [.5, 1], [1, 1]]) {
          const h = document.createElement("div");
          h.className = "hc-handle";
          h.style.left = (b.left - 3 + (b.width + 6) * x) + "px";
          h.style.top = (b.top - 3 + (b.height + 6) * y) + "px";
          overlay.appendChild(h);
        }
        const size = document.createElement("div");
        size.className = "hc-size";
        size.textContent = Math.round(b.width) + " × " + Math.round(b.height);
        size.style.left = (b.left + b.width / 2) + "px";
        size.style.top = (b.bottom + 12) + "px";
        overlay.appendChild(size);
      }
      reportRect();
    }
    for (const el of picked) {
      const p = document.createElement("div");
      p.className = "hc-picked";
      place(p, el.getBoundingClientRect(), 0);
      overlay.appendChild(p);
    }
  };
  let queued = false;
  const redraw = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => { queued = false; draw(); });
  };

  const activate = (n, scroll) => {
    selected = n;
    redraw();
    const first = marks().find((el) => Number(el.dataset.hcChange) === n);
    if (scroll && first) (first.firstElementChild || first.parentElement || first).scrollIntoView({ block: "center", behavior: "smooth" });
  };

  // ---- picking elements for the chat ----
  const selectorOf = (el) => {
    const parts = [];
    for (let e = el; e && e.nodeType === 1 && e !== document.documentElement; e = e.parentElement) {
      if (e.id) { parts.unshift("#" + CSS.escape(e.id)); break; }
      let part = e.localName;
      const same = e.parentElement ? Array.from(e.parentElement.children).filter((c) => c.localName === e.localName) : [];
      if (same.length > 1) part += ":nth-of-type(" + (same.indexOf(e) + 1) + ")";
      parts.unshift(part);
    }
    return parts.join(" > ");
  };
  const itemOf = (el) => {
    const mark = el.closest("[data-hc-change]") || el.querySelector("[data-hc-change]");
    return {
      path: path(),
      selector: selectorOf(el),
      text: (el.innerText || el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 200),
      change: mark ? Number(mark.dataset.hcChange) : null,
    };
  };
  const setPicking = (on) => {
    picking = !!on;
    document.documentElement.classList.toggle("hc-picking", picking);
    if (!picking) { hovered = null; picked = []; }
    redraw();
  };

  document.addEventListener("mouseover", (e) => {
    const target = e.target instanceof Element ? e.target : null;
    if (picking) { hovered = target; redraw(); return; }
    const mark = target && target.closest("[data-hc-change]");
    const n = mark ? Number(mark.dataset.hcChange) : null;
    if (n !== pointed) { pointed = n; redraw(); }
  }, true);
  document.addEventListener("click", (e) => {
    if (!picking || !(e.target instanceof Element)) return;
    // While picking, a click selects: links do not navigate and forms do not submit.
    e.preventDefault();
    e.stopPropagation();
    const el = e.target;
    picked = e.shiftKey ? (picked.includes(el) ? picked.filter((p) => p !== el) : picked.concat(el)) : [el];
    redraw();
    send({ type: "hc:picked", items: picked.map(itemOf) });
  }, true);
  document.addEventListener("submit", (e) => { if (picking) e.preventDefault(); }, true);
  document.addEventListener("keydown", (e) => { if (picking && e.key === "Escape") setPicking(false); }, true);

  // ---- the canvas: select an element, edit its text, see style edits at once ----
  // A quick press (released within HOLD ms, moved less than MOVE px) is a plain click: links and buttons work.
  // Anything longer, a right-click, or a click on something that does nothing selects instead.
  const HOLD = 200, MOVE = 4;
  let sel = null; // the selected element
  let editing = null; // the element whose text is being edited, and its text before
  let before = "";
  let press = null;
  let swallow = false; // the click that ends a long press selects, it does not navigate
  const INTERACTIVE = "a[href], button, input, select, textarea, label, summary, [contenteditable], [onclick], [role=button]";
  const sourceOf = (t) => {
    if (!(t instanceof Element)) return null;
    const svg = t.closest("svg");
    return (svg || t).closest("[data-hc-src]");
  };
  const STYLE = ["color", "backgroundColor", "fontSize", "fontWeight", "fontStyle", "textDecorationLine", "textAlign", "lineHeight", "borderRadius", "boxShadow", "fontFamily"];
  const rectOfSel = () => {
    const b = sel.getBoundingClientRect();
    return { x: b.left, y: b.top, w: b.width, h: b.height };
  };
  let lastRect = "";
  function reportRect() {
    if (!sel) return;
    const r = rectOfSel();
    const key = [r.x, r.y, r.w, r.h].map(Math.round).join(",");
    if (key !== lastRect) { lastRect = key; send({ type: "hc:rect", rect: r }); }
  }
  function select(el, menu) {
    if (editing && editing !== el) finishEdit(true);
    sel = el;
    lastRect = "";
    const cs = getComputedStyle(el);
    const style = {};
    for (const k of STYLE) style[k] = cs[k];
    send({ type: "hc:select", src: el.dataset.hcSrc, tag: el.localName, text: (el.innerText || "").trim().slice(0, 200), rect: rectOfSel(), style, menu: !!menu });
    redraw();
  }
  function deselect(tell) {
    if (editing) finishEdit(true);
    if (!sel) return;
    sel = null;
    redraw();
    if (tell) send({ type: "hc:deselect" });
  }
  function startEdit(el) {
    if (el.children.length && Array.from(el.children).some((c) => c.localName !== "br")) return;
    editing = el;
    before = el.innerText;
    el.setAttribute("data-hc-editing", "");
    el.contentEditable = "plaintext-only";
    el.focus();
    const r = document.createRange();
    r.selectNodeContents(el);
    const s = getSelection();
    s.removeAllRanges();
    s.addRange(r);
    redraw();
  }
  function finishEdit(keep) {
    const el = editing;
    if (!el) return;
    editing = null;
    el.removeAttribute("data-hc-editing");
    el.removeAttribute("contenteditable");
    const text = el.innerText.replace(/\n$/, "");
    if (!keep || text === before) { if (!keep) el.innerText = before; }
    else send({ type: "hc:text", src: el.dataset.hcSrc, text });
    redraw();
  }
  document.addEventListener("pointerdown", (e) => {
    if (picking || e.button !== 0 || editing && editing.contains(e.target)) return;
    press = { x: e.clientX, y: e.clientY, t: performance.now(), el: sourceOf(e.target), target: e.target };
  }, true);
  document.addEventListener("pointerup", (e) => {
    const p = press;
    press = null;
    if (!p || !p.el) return;
    const moved = Math.hypot(e.clientX - p.x, e.clientY - p.y) > MOVE;
    const held = performance.now() - p.t > HOLD;
    if (held || moved) { swallow = true; select(p.el); }
  }, true);
  document.addEventListener("click", (e) => {
    if (picking) return;
    if (swallow) { swallow = false; e.preventDefault(); e.stopPropagation(); return; }
    if (editing && editing.contains(e.target)) return;
    const el = sourceOf(e.target);
    if (!el) return deselect(true);
    if (e.target.closest(INTERACTIVE)) return; // a quick click on a link or button does what it does
    e.preventDefault();
    select(el);
  }, true);
  document.addEventListener("dblclick", (e) => {
    if (picking) return;
    const el = sourceOf(e.target);
    if (!el || e.target.closest("input, select, textarea")) return;
    e.preventDefault();
    if (sel !== el) select(el);
    startEdit(el);
  }, true);
  document.addEventListener("contextmenu", (e) => {
    if (picking) return;
    const el = sourceOf(e.target);
    if (!el) return;
    e.preventDefault();
    select(el, true);
  }, true);
  document.addEventListener("keydown", (e) => {
    if (editing) {
      if (e.key === "Escape") { e.preventDefault(); finishEdit(false); }
      else if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); finishEdit(true); }
      return;
    }
    if (picking || !sel) return;
    if (e.key === "Escape") deselect(true);
    else if (e.key === "Enter") { e.preventDefault(); startEdit(sel); }
  }, true);
  document.addEventListener("focusout", (e) => { if (editing && e.target === editing) finishEdit(true); }, true);

  // ---- messages from the admin ----
  window.addEventListener("message", (e) => {
    if (e.source !== window.parent || e.origin !== ORIGIN || !e.data || typeof e.data !== "object") return;
    const m = e.data;
    if (m.type === "hc:activate") activate(m.n == null ? null : Number(m.n), true);
    else if (m.type === "hc:pick") setPicking(m.on);
    else if (m.type === "hc:back" && hist.i > 0) history.back();
    else if (m.type === "hc:forward" && hist.i < hist.stack.length - 1) history.forward();
    else if (m.type === "hc:style" && typeof m.src === "string" && typeof m.prop === "string") {
      for (const el of document.querySelectorAll(`[data-hc-src="${CSS.escape(m.src)}"]`)) {
        if (m.value) el.style.setProperty(m.prop, String(m.value), "important");
        else el.style.removeProperty(m.prop);
      }
      redraw();
    } else if (m.type === "hc:deselect") deselect(false);
    else if (m.type === "hc:mark-color" && typeof m.color === "string" && CSS.supports("color", m.color)) {
      document.documentElement.style.setProperty("--mark", m.color);
    }
  });

  // In-page navigation (hash links, history.pushState by a site script followed by popstate).
  const moved = () => { hist.stack[hist.i] = path(); save(); send(Object.assign({ type: "hc:navigated" }, where())); redraw(); };
  window.addEventListener("hashchange", moved);
  window.addEventListener("popstate", moved);

  const start = () => {
    document.documentElement.appendChild(overlay);
    window.addEventListener("scroll", redraw, true);
    window.addEventListener("resize", redraw);
    if (document.body) new MutationObserver(redraw).observe(document.body, { childList: true, subtree: true, characterData: true });
    if (window.ResizeObserver) new ResizeObserver(redraw).observe(document.documentElement);
    draw();
    const changes = Array.from(new Set(marks().map((el) => Number(el.dataset.hcChange)).concat(pageChanges))).sort((a, b) => a - b);
    if (!firstLoad) send(Object.assign({ type: "hc:navigated" }, where()));
    send(Object.assign({ type: "hc:ready", changes }, where()));
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
