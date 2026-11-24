// harness-cms preview: draws change markers over the page and talks to the admin that frames it.
//
// The page runs in a sandbox without allow-same-origin, so its own origin is opaque ("null"). It never reaches the
// admin's DOM: the only channel is postMessage with window.parent. Outgoing messages are addressed to the site's
// origin (the admin lives there); incoming ones are accepted only from window.parent and from that origin.
//
// admin -> preview: {type:"hc:activate", n} {type:"hc:pick", on} {type:"hc:back"} {type:"hc:forward"} {type:"hc:mark-color", color}
// preview -> admin: {type:"hc:ready", path, changes, canBack, canForward} {type:"hc:picked", items:[{path, selector, text, change}]}
//                   {type:"hc:navigated", path, canBack, canForward}
(() => {
  "use strict";
  const ORIGIN = new URL(location.href).origin;
  const framed = window.parent !== window;
  const script = document.currentScript;
  const pageChanges = ((script && script.dataset.hcPage) || "").split(",").filter(Boolean).map(Number);

  const send = (msg) => { if (framed) window.parent.postMessage(msg, ORIGIN); };

  // The page's path as a visitor sees it: without the preview switch.
  const path = () => {
    const u = new URL(location.href);
    u.searchParams.delete("preview");
    return u.pathname + u.search + u.hash;
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

  // ---- messages from the admin ----
  window.addEventListener("message", (e) => {
    if (e.source !== window.parent || e.origin !== ORIGIN || !e.data || typeof e.data !== "object") return;
    const m = e.data;
    if (m.type === "hc:activate") activate(m.n == null ? null : Number(m.n), true);
    else if (m.type === "hc:pick") setPicking(m.on);
    else if (m.type === "hc:back" && hist.i > 0) history.back();
    else if (m.type === "hc:forward" && hist.i < hist.stack.length - 1) history.forward();
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
