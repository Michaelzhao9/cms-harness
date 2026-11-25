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
      box.className = "hc-sel" + (sharedSel ? " shared" : "");
      overlay.classList.toggle("hc-shared", sharedSel);
      place(box, b, 3);
      overlay.appendChild(box);
      if (!editing) {
        for (const [x, y] of [[0, 0], [.5, 0], [1, 0], [0, .5], [1, .5], [0, 1], [.5, 1], [1, 1]]) {
          const h = document.createElement("div");
          h.className = "hc-handle";
          h.dataset.dir = (y === 0 ? "n" : y === 1 ? "s" : "") + (x === 0 ? "w" : x === 1 ? "e" : "");
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
    for (const g of guides) {
      const line = document.createElement("div");
      line.className = "hc-guide";
      if (g.x != null) Object.assign(line.style, { left: g.x + "px", top: "0px", width: "1px", height: "100%" });
      else Object.assign(line.style, { left: "0px", top: g.y + "px", width: "100%", height: "1px" });
      overlay.appendChild(line);
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
    requestAnimationFrame(() => { queued = false; draw(); checkSoon(); });
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
  let sharedSel = false; // it is in a template other pages use too (the admin says so)
  let editing = null; // the element whose text is being edited, and its text before
  let before = "";
  let press = null;
  let nudged = 0, nudgeFrom = null;
  let swallow = false; // the click that ends a long press selects, it does not navigate
  const INTERACTIVE = "a[href], area[href], button, input, select, textarea, label, summary, video, audio, [contenteditable], [onclick], [role=button]";
  // Controls the visitor works with: pressing, dragging or typing in them never edits the page.
  const CONTROLS = "input, textarea, select, option, video, audio, [contenteditable], [draggable=true]";
  const inControl = (t) => t instanceof Element && !!t.closest(CONTROLS) && !(editing && editing.contains(t));
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
    if (sel !== el) sharedSel = false;
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
  // The text as written, line breaks as "\n": not innerText, which applies text-transform (an uppercase style
  // would be saved as capitals).
  const plainText = (el) => Array.from(el.childNodes).map((n) => (n.nodeName === "BR" ? "\n" : n.nodeType === 3 ? n.data : n.textContent)).join("");
  const setPlain = (el, text) => {
    el.textContent = "";
    text.split("\n").forEach((line, i) => {
      if (i) el.appendChild(document.createElement("br"));
      el.appendChild(document.createTextNode(line));
    });
  };
  function startEdit(el) {
    if (el.children.length && Array.from(el.children).some((c) => c.localName !== "br")) return;
    editing = el;
    before = plainText(el);
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
    const text = plainText(el).replace(/\n$/, "");
    if (!keep || text === before) { if (!keep) setPlain(el, before); }
    else send({ type: "hc:text", src: el.dataset.hcSrc, text });
    redraw();
  }
  // Dragging moves an element (translate), a handle resizes it (width/height). Both apply at once to every
  // element from the same template line; the admin saves them for the width being looked at.
  let drag = null; // {kind: "move"|"size", el, x0, y0, t0, w0, h0, dir, rect0, lines}
  let guides = [];
  const sameAs = (el) => document.querySelectorAll(`[data-hc-src="${CSS.escape(el.dataset.hcSrc)}"]`);
  const getT = (el) => {
    const t = (el.style.translate || getComputedStyle(el).translate || "none").split(" ").map(parseFloat);
    return [t[0] || 0, t[1] || 0];
  };
  const setProp = (el, prop, v) => { for (const e of sameAs(el)) e.style.setProperty(prop, v, "important"); };
  const SNAP = 6;
  function snapLines(el) {
    const xs = [], ys = [];
    let n = 0;
    for (const o of document.querySelectorAll("[data-hc-src]")) {
      if (n > 400) break;
      if (o.dataset.hcSrc === el.dataset.hcSrc || el.contains(o)) continue;
      const b = o.getBoundingClientRect();
      if (!b.width || !b.height || b.bottom < 0 || b.top > innerHeight) continue;
      n++;
      xs.push(b.left, b.left + b.width / 2, b.right);
      ys.push(b.top, b.top + b.height / 2, b.bottom);
    }
    return { xs, ys };
  }
  const nearest = (edges, lines) => {
    let best = null;
    for (const e of edges) for (const l of lines) {
      const d = l - e;
      if (Math.abs(d) <= SNAP && (!best || Math.abs(d) < Math.abs(best.d))) best = { d, at: l };
    }
    return best;
  };
  document.addEventListener("pointerdown", (e) => {
    if (picking || tool || e.button !== 0 || editing && editing.contains(e.target)) return;
    const handle = e.target instanceof Element && e.target.closest(".hc-handle");
    if (handle && sel) {
      e.preventDefault();
      const b = sel.getBoundingClientRect();
      drag = { kind: "size", el: sel, dir: handle.dataset.dir, x0: e.clientX, y0: e.clientY, t0: getT(sel), w0: b.width, h0: b.height };
      return;
    }
    if (inControl(e.target)) return;
    press = { x: e.clientX, y: e.clientY, t: performance.now(), el: sourceOf(e.target), target: e.target };
  }, true);
  document.addEventListener("pointermove", (e) => {
    if (press && !drag && press.el && Math.hypot(e.clientX - press.x, e.clientY - press.y) > MOVE) {
      const el = press.el;
      select(el);
      drag = { kind: "move", el, x0: press.x, y0: press.y, t0: getT(el), rect0: el.getBoundingClientRect(), lines: snapLines(el) };
      document.documentElement.classList.add("hc-dragging");
    }
    if (!drag) return;
    e.preventDefault();
    let dx = e.clientX - drag.x0, dy = e.clientY - drag.y0;
    if (drag.kind === "move") {
      const r = drag.rect0;
      // Alt turns snapping off.
      const sx = e.altKey ? null : nearest([r.left + dx, r.left + r.width / 2 + dx, r.right + dx], drag.lines.xs);
      const sy = e.altKey ? null : nearest([r.top + dy, r.top + r.height / 2 + dy, r.bottom + dy], drag.lines.ys);
      if (sx) dx += sx.d;
      if (sy) dy += sy.d;
      guides = [sx && { x: sx.at }, sy && { y: sy.at }].filter(Boolean);
      setProp(drag.el, "translate", `${Math.round(drag.t0[0] + dx)}px ${Math.round(drag.t0[1] + dy)}px`);
    } else {
      const d = drag.dir;
      const w = d.includes("e") ? drag.w0 + dx : d.includes("w") ? drag.w0 - dx : null;
      const h = d.includes("s") ? drag.h0 + dy : d.includes("n") ? drag.h0 - dy : null;
      setProp(drag.el, "box-sizing", "border-box");
      if (w != null) { drag.w = Math.max(8, Math.round(w)); setProp(drag.el, "width", drag.w + "px"); setProp(drag.el, "max-width", "none"); }
      if (h != null) { drag.h = Math.max(8, Math.round(h)); setProp(drag.el, "height", drag.h + "px"); }
      // Pulling a left or top handle keeps the opposite edge where it was.
      const tx = drag.t0[0] + (d.includes("w") && drag.w ? drag.w0 - drag.w : 0);
      const ty = drag.t0[1] + (d.includes("n") && drag.h ? drag.h0 - drag.h : 0);
      if (tx !== drag.t0[0] || ty !== drag.t0[1]) setProp(drag.el, "translate", `${Math.round(tx)}px ${Math.round(ty)}px`);
    }
    redraw();
  }, true);
  document.addEventListener("pointerup", () => {
    const p = press;
    press = null;
    if (drag) {
      const d = drag;
      drag = null;
      guides = [];
      document.documentElement.classList.remove("hc-dragging");
      if (d.kind === "move") swallowNext();
      const t = getT(d.el);
      const moved = t[0] !== d.t0[0] || t[1] !== d.t0[1];
      if (d.kind === "move" && moved) send({ type: "hc:move", src: d.el.dataset.hcSrc, x: t[0], y: t[1], from: d.t0 });
      if (d.kind === "size" && (d.w != null || d.h != null)) send({ type: "hc:resize", src: d.el.dataset.hcSrc, w: d.w ?? null, h: d.h ?? null, x: t[0], y: t[1], from: d.t0 });
      redraw();
      return;
    }
    if (!p || !p.el) return;
    if (performance.now() - p.t > HOLD) { swallowNext(); select(p.el); }
  }, true);
  // A gesture the browser takes over (scrolling, a system gesture) ends ours without a save.
  document.addEventListener("pointercancel", () => {
    // Back to where it was when the gesture began.
    if (drag) {
      setProp(drag.el, "translate", `${drag.t0[0]}px ${drag.t0[1]}px`);
      if (drag.kind === "size" && drag.w != null) setProp(drag.el, "width", drag.w0 + "px");
      if (drag.kind === "size" && drag.h != null) setProp(drag.el, "height", drag.h0 + "px");
    }
    press = null;
    drag = null;
    guides = [];
    document.documentElement.classList.remove("hc-dragging");
    redraw();
  }, true);
  // The click that ends a long press or a drag does nothing; if none follows, the next real click still works.
  function swallowNext() {
    swallow = true;
    setTimeout(() => { swallow = false; }, 400);
  }
  document.addEventListener("dragstart", (e) => { if (!picking && !inControl(e.target)) e.preventDefault(); }, true);

  // The pen: with a tool chosen (hc:tool), the next click says where to add. The admin adds it to the template
  // and reloads; then hc:place puts the new element exactly where the click was (in page coordinates).
  let tool = null;
  const setTool = (kind) => {
    tool = kind || null;
    document.documentElement.classList.toggle("hc-tool", !!tool);
  };
  function placeAt(src, x, y, sx, sy) {
    scrollTo(sx, sy);
    const el = document.querySelector(`[data-hc-src="${CSS.escape(src)}"]`);
    if (!el) return;
    const r = el.getBoundingClientRect();
    const dx = Math.round(x - (r.left + scrollX)), dy = Math.round(y - (r.top + scrollY));
    setProp(el, "translate", `${dx}px ${dy}px`);
    select(el);
    send({ type: "hc:move", src, x: dx, y: dy, from: [0, 0] });
  }
  document.addEventListener("click", (e) => {
    if (tool) {
      e.preventDefault();
      e.stopPropagation();
      // What was clicked; on empty space, the element nearest the click.
      const dist = (el) => {
        const b = el.getBoundingClientRect();
        return Math.hypot(Math.max(b.left - e.clientX, 0, e.clientX - b.right), Math.max(b.top - e.clientY, 0, e.clientY - b.bottom));
      };
      const near = () => [...document.querySelectorAll("body [data-hc-src]")].filter((el) => el.getBoundingClientRect().width).sort((a, b) => dist(a) - dist(b))[0];
      const at = document.elementsFromPoint(e.clientX, e.clientY).map(sourceOf).find(Boolean) || near();
      if (at) send({ type: "hc:add", kind: tool, src: at.dataset.hcSrc, x: e.clientX + scrollX, y: e.clientY + scrollY, sx: scrollX, sy: scrollY });
      setTool(null);
      return;
    }
    if (picking) return;
    if (swallow) { swallow = false; e.preventDefault(); e.stopPropagation(); return; }
    if (editing && editing.contains(e.target)) return;
    const el = sourceOf(e.target);
    if (!el) return deselect(true);
    // A quick click on a link, a button or a field does what it does, and the selection lets go, so typing
    // there never reaches the canvas.
    if (e.target.closest(INTERACTIVE)) return deselect(true);
    e.preventDefault();
    select(el);
  }, true);
  document.addEventListener("dblclick", (e) => {
    if (picking) return;
    const el = sourceOf(e.target);
    if (!el || inControl(e.target)) return;
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
    if (picking || !sel || inControl(e.target)) return;
    const mod = e.metaKey || e.ctrlKey;
    if (e.key === "Escape") deselect(true);
    else if (e.key === "Enter") { e.preventDefault(); startEdit(sel); }
    else if (e.key === "Delete" || e.key === "Backspace") { e.preventDefault(); send({ type: "hc:key", key: "delete" }); }
    else if (mod && e.key.toLowerCase() === "z") { e.preventDefault(); send({ type: "hc:key", key: e.shiftKey ? "redo" : "undo" }); }
    else if (mod && e.key.toLowerCase() === "d") { e.preventDefault(); send({ type: "hc:key", key: "duplicate" }); }
    else if (e.key.startsWith("Arrow")) {
      e.preventDefault();
      const step = e.shiftKey ? 10 : 1;
      const [x, y] = getT(sel);
      const nx = x + (e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0);
      const ny = y + (e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0);
      setProp(sel, "translate", `${nx}px ${ny}px`);
      redraw();
      clearTimeout(nudged);
      const el = sel, from = nudgeFrom || [x, y];
      nudgeFrom = from;
      // One save for a run of key presses.
      nudged = setTimeout(() => { nudgeFrom = null; send({ type: "hc:move", src: el.dataset.hcSrc, x: nx, y: ny, from }); }, 400);
    }
  }, true);
  document.addEventListener("focusout", (e) => { if (editing && e.target === editing) finishEdit(true); }, true);

  // ---- checks: hand-edited elements that run off the screen, cover other content, or have tiny text ----
  // Measured at this width only, as warnings; nothing is changed.
  const MIN_TEXT = 12;
  let lastWarn = "";
  function check() {
    const edited = [...document.querySelectorAll("[data-hc-src]")].filter((el) => el.dataset.hc || /translate|width|height/.test(el.getAttribute("style") || ""));
    const width = document.documentElement.clientWidth;
    const content = [...document.querySelectorAll("[data-hc-src]")].filter((el) => {
      const b = el.getBoundingClientRect();
      return b.width && b.height && !el.querySelector("[data-hc-src]");
    }).slice(0, 600);
    const items = [];
    for (const el of edited) {
      const b = el.getBoundingClientRect();
      if (!b.width || !b.height) continue;
      const src = el.dataset.hcSrc;
      if (b.right > width + 1 || b.left < -1) items.push({ src, kind: "overflow" });
      const text = (el.innerText || "").trim();
      if (text && parseFloat(getComputedStyle(el).fontSize) < MIN_TEXT) items.push({ src, kind: "small" });
      const hit = content.find((o) => {
        if (o === el || o.dataset.hcSrc === src || o.contains(el) || el.contains(o)) return false;
        const r = o.getBoundingClientRect();
        const w = Math.min(b.right, r.right) - Math.max(b.left, r.left);
        const h = Math.min(b.bottom, r.bottom) - Math.max(b.top, r.top);
        return w > 2 && h > 2 && w * h > 0.1 * Math.min(b.width * b.height, r.width * r.height);
      });
      if (hit) items.push({ src, kind: "overlap" });
    }
    const key = JSON.stringify(items);
    if (key !== lastWarn) { lastWarn = key; send({ type: "hc:warnings", items }); }
  }
  let checkTimer = 0;
  const checkSoon = () => { clearTimeout(checkTimer); checkTimer = setTimeout(check, 300); };

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
    else if (m.type === "hc:shared") { sharedSel = !!m.on; redraw(); }
    else if (m.type === "hc:tool") setTool(typeof m.kind === "string" ? m.kind : null);
    else if (m.type === "hc:focus" && typeof m.src === "string") {
      const el = document.querySelector(`[data-hc-src="${CSS.escape(m.src)}"]`);
      if (el) {
        // Vertically only: an element that runs off the side must not scroll the page sideways.
        const r = el.getBoundingClientRect();
        scrollTo({ top: scrollY + r.top - (innerHeight - r.height) / 2, behavior: "smooth" });
        setTimeout(() => select(el), 350);
      }
    }
    else if (m.type === "hc:place" && typeof m.src === "string") placeAt(m.src, Number(m.x) || 0, Number(m.y) || 0, Number(m.sx) || 0, Number(m.sy) || 0);
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
