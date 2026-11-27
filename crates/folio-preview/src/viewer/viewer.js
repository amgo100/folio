// The viewer of `folio watch` (SPEC §12.2): it receives the display lists
// of the pages over the websocket, in `folio-render`'s wire format, and
// draws them on canvases with Canvas 2D, from glyphs that the server
// rasterises at the current zoom (SPEC §12.4). A click asks the server for
// the source it shows; the server tells it where an editor's cursor is
// (`reveal`) and where the diagnostics point (`overlays`). The protocol is
// documented in `folio-preview`'s crate documentation.
"use strict";

const token = new URLSearchParams(location.search).get("token") || "";
const q = (id) => document.getElementById(id);
const pagesEl = q("pages");
const statusLine = q("status");
const sourceLine = q("source");
const banner = q("banner");
const problems = q("problems");
const problemsSummary = q("problems-summary");
const list = q("diagnostics");

// The thickness of an object's frame, in points (`FRAME_THICKNESS`).
const FRAME_THICKNESS = 0.5;
// The most glyphs one `/glyphs/` request asks for.
const GLYPHS_PER_REQUEST = 800;
// The largest size in pixels the server rasterises glyphs at; larger ones
// are scaled up from it.
const MAX_GLYPH_SIZE = 2048;
// The most pixels a page's canvas holds; past it, a page is drawn at a
// lower resolution and stretched.
const MAX_CANVAS_PIXELS = 16 * 1024 * 1024;
// How long a glyph that could not be fetched waits before it is asked for
// again, in milliseconds.
const RETRY_AFTER = 2000;

// ---------------------------------------------------------------------
// The wire format (`folio_render::wire`, version 2).

class DecodeError extends Error {}

class Reader {
  constructor(bytes) {
    this.bytes = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.pos = 0;
  }
  left() { return this.bytes.length - this.pos; }
  u8() {
    if (this.pos >= this.bytes.length) throw new DecodeError("the display list ends early");
    return this.bytes[this.pos++];
  }
  // An unsigned varint, as a Number: exact below 2^53, which every value
  // the viewer reads is; spans, which may be larger, are only skipped.
  uint() {
    let value = 0;
    let scale = 1;
    for (let i = 0; i < 10; i++) {
      const byte = this.u8();
      value += (byte & 0x7f) * scale;
      if ((byte & 0x80) === 0) return value;
      scale *= 128;
    }
    throw new DecodeError("a number in the display list is out of range");
  }
  int() {
    const v = this.uint();
    return v % 2 === 0 ? v / 2 : -(v + 1) / 2;
  }
  count(min) {
    const n = this.uint();
    if (n * min > this.left()) throw new DecodeError("a count is larger than the message");
    return n;
  }
  f32() {
    if (this.left() < 4) throw new DecodeError("the display list ends early");
    const v = this.view.getFloat32(this.pos, true);
    this.pos += 4;
    if (!Number.isFinite(v)) throw new DecodeError("a length is not finite");
    return v;
  }
  point() { return { x: this.f32(), y: this.f32() }; }
  rect() { return { x: this.f32(), y: this.f32(), width: this.f32(), height: this.f32() }; }
  paint() {
    if (this.u8() !== 0) throw new DecodeError("an unknown paint");
    const [r, g, b, a] = [this.u8(), this.u8(), this.u8(), this.u8()];
    return { r, g, b, a };
  }
  span() { this.uint(); this.uint(); }
  skip(n) {
    if (n > this.left()) throw new DecodeError("the display list ends early");
    this.pos += n;
  }
}

// Decodes a message: `{ kind: "list", list }` or `{ kind: "spans" }`.
function decode(bytes) {
  const r = new Reader(bytes);
  if (r.u8() !== 0x46 || r.u8() !== 0x44 || r.u8() !== 0x4c) {
    throw new DecodeError("not a display list");
  }
  const version = r.u8();
  if (version !== 2) throw new DecodeError(`display list format version ${version} is not supported`);
  const kind = r.u8();
  if (kind === 1) return { kind: "spans" };
  if (kind !== 0) throw new DecodeError(`unknown display list message kind ${kind}`);
  const width = r.f32();
  const height = r.f32();
  r.span();
  const count = r.count(2);
  const items = [];
  for (let i = 0; i < count; i++) {
    const tag = r.uint();
    const len = r.uint();
    if (len > r.left()) throw new DecodeError("an item is longer than the message");
    const end = r.pos + len;
    const body = new Reader(bytes.subarray(r.pos, end));
    body.span();
    items.push(command(tag, body));
    r.pos = end; // Fields a later version appends are skipped.
  }
  if (r.left() !== 0) throw new DecodeError("bytes follow the display list");
  return { kind: "list", list: { width, height, items } };
}

function command(tag, r) {
  switch (tag) {
    case 0: { // Glyphs
      const origin = r.point();
      const font = r.uint();
      const size = r.f32();
      const fill = r.paint();
      r.skip(r.count(1)); // The text: for selection, which comes later.
      const n = r.count(6);
      const glyphs = new Array(n);
      for (let i = 0; i < n; i++) {
        const id = r.uint();
        const advance = r.f32();
        const flags = r.u8();
        const dx = flags & 1 ? r.f32() : 0;
        const dy = flags & 2 ? r.f32() : 0;
        r.int(); r.int(); // The glyph's range of the run's text.
        r.int(); r.int(); r.int(); // Its span-map entry.
        glyphs[i] = { id, advance, dx, dy };
      }
      return { kind: "glyphs", origin, font, size, fill, glyphs };
    }
    case 1: { // Path
      const n = r.count(1);
      const segments = [];
      for (let i = 0; i < n; i++) {
        const seg = r.u8();
        if (seg === 0) segments.push(["M", r.point()]);
        else if (seg === 1) segments.push(["L", r.point()]);
        else if (seg === 2) segments.push(["C", r.point(), r.point(), r.point()]);
        else if (seg === 3) segments.push(["Z"]);
        else throw new DecodeError("an unknown path segment");
      }
      const fill = r.u8() ? r.paint() : null;
      const evenOdd = r.u8() === 1;
      let stroke = null;
      if (r.u8()) {
        stroke = {
          color: r.paint(),
          thickness: r.f32(),
          cap: ["butt", "round", "square"][r.u8()] || "butt",
          join: ["miter", "round", "bevel"][r.u8()] || "miter",
          miterLimit: r.f32(),
          dash: null,
        };
        if (r.u8()) {
          const k = r.count(4);
          const array = [];
          for (let i = 0; i < k; i++) array.push(r.f32());
          stroke.dash = { array, phase: r.f32() };
        }
      }
      return { kind: "path", segments, fill, evenOdd, stroke };
    }
    case 2: return { kind: "frame", rect: r.rect() };
    case 3: return { kind: "box" }; // Draws nothing.
    default: return { kind: "unknown" }; // A command of a later version.
  }
}

// ---------------------------------------------------------------------
// Glyphs, rasterised by the server.

// `font|size|id|sub` → { left, top, width, height, mask } or null (no
// outline); undefined while a request is out.
let glyphCache = new Map();
// `font|size` → Set of `id.sub` wanted.
let glyphWants = new Map();
let glyphTimer = 0;
// Tinted copies of masks: mask → Map(colour → canvas).
let tinted = new WeakMap();
// `font|size|id|sub` → when a glyph that could not be fetched is asked for
// again.
let failed = new Map();
let retryTimer = 0;
// The server's font generation: when it changes, font ids may name other
// fonts, and every glyph is fetched again.
let fontGeneration = null;

function dropGlyphs() {
  glyphCache = new Map();
  glyphWants = new Map();
  tinted = new WeakMap();
  failed = new Map();
}

// A glyph that could not be fetched, for a reason that may pass (the
// network, the server): asked for again after a while.
function glyphFailed(key) {
  glyphCache.delete(key);
  failed.set(key, Date.now() + RETRY_AFTER);
  armRetry();
}

// Redraws when the earliest failed glyph is due again.
function armRetry() {
  if (retryTimer || failed.size === 0) return;
  let due = Infinity;
  for (const at of failed.values()) if (at < due) due = at;
  retryTimer = setTimeout(() => {
    retryTimer = 0;
    redrawVisible();
    // Glyphs not due yet, or not on a visible page, wait for the next.
    const now = Date.now();
    for (const [key, at] of failed) if (at <= now) failed.delete(key);
    armRetry();
  }, Math.max(0, due - Date.now()));
}

function wantGlyph(font, size, id, sub) {
  const key = `${font}|${size}`;
  let set = glyphWants.get(key);
  if (!set) glyphWants.set(key, (set = new Set()));
  set.add(`${id}.${sub}`);
  if (!glyphTimer) glyphTimer = setTimeout(fetchGlyphs, 0);
}

async function fetchGlyphs() {
  glyphTimer = 0;
  const wants = [...glyphWants];
  glyphWants.clear();
  const cache = glyphCache;
  const requests = [];
  for (const [key, set] of wants) {
    const [font, size] = key.split("|");
    const keyOf = (g) => `${key}|${g.replace(".", "|")}`;
    const all = [...set].filter((g) => !cache.has(keyOf(g)));
    for (const g of all) cache.set(keyOf(g), undefined);
    for (let i = 0; i < all.length; i += GLYPHS_PER_REQUEST) {
      const part = all.slice(i, i + GLYPHS_PER_REQUEST);
      const url = `/glyphs/${font}?token=${encodeURIComponent(token)}&size=${size}&g=${part.join(",")}`;
      requests.push(fetch(url).then(async (response) => {
        if (cache !== glyphCache) return false;
        if (response.status === 404) {
          // The server has no such font: the glyphs are drawn as nothing
          // until the fonts change.
          for (const g of part) cache.set(keyOf(g), null);
          return true;
        }
        if (!response.ok) throw new Error(`${response.status}`);
        const bytes = new Uint8Array(await response.arrayBuffer());
        // Glyphs of fonts the server has replaced since are dropped.
        if (cache !== glyphCache) return false;
        const count = readGlyphs(font, size, bytes);
        // Glyphs an answer left out past its size limit are asked for
        // again at once, as long as answers hold some; otherwise later.
        for (const g of part) {
          if (glyphCache.get(keyOf(g)) !== undefined) continue;
          if (count > 0) {
            glyphCache.delete(keyOf(g));
            wantGlyph(font, size, ...g.split("."));
          } else {
            glyphFailed(keyOf(g));
          }
        }
        return true;
      }).catch(() => {
        if (cache !== glyphCache) return false;
        // Glyphs read before the answer broke off are kept.
        for (const g of part) if (cache.get(keyOf(g)) === undefined) glyphFailed(keyOf(g));
        return false;
      }));
    }
  }
  const done = await Promise.all(requests);
  if (done.some(Boolean)) redrawVisible();
}

function readGlyphs(font, size, bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const count = view.getUint32(0, true);
  let at = 4;
  for (let i = 0; i < count; i++) {
    const id = view.getUint16(at, true);
    const sub = bytes[at + 2];
    const left = view.getInt32(at + 4, true);
    const top = view.getInt32(at + 8, true);
    const width = view.getUint32(at + 12, true);
    const height = view.getUint32(at + 16, true);
    at += 20;
    let glyph = null;
    if (width > 0 && height > 0) {
      const mask = document.createElement("canvas");
      mask.width = width;
      mask.height = height;
      const pixels = new ImageData(width, height);
      const coverage = bytes.subarray(at, at + width * height);
      for (let p = 0; p < coverage.length; p++) pixels.data[p * 4 + 3] = coverage[p];
      mask.getContext("2d").putImageData(pixels, 0, 0);
      glyph = { left, top, width, height, mask };
    }
    at += width * height;
    glyphCache.set(`${font}|${size}|${id}|${sub}`, glyph);
  }
  return count;
}

// The mask in a colour: black masks are drawn as they are.
function inColour(glyph, fill) {
  if (fill.r === 0 && fill.g === 0 && fill.b === 0 && fill.a === 255) return glyph.mask;
  let copies = tinted.get(glyph.mask);
  if (!copies) tinted.set(glyph.mask, (copies = new Map()));
  const key = `${fill.r},${fill.g},${fill.b},${fill.a}`;
  let copy = copies.get(key);
  if (!copy) {
    copy = document.createElement("canvas");
    copy.width = glyph.width;
    copy.height = glyph.height;
    const ctx = copy.getContext("2d");
    ctx.drawImage(glyph.mask, 0, 0);
    ctx.globalCompositeOperation = "source-in";
    ctx.fillStyle = css(fill);
    ctx.fillRect(0, 0, glyph.width, glyph.height);
    copies.set(key, copy);
  }
  return copy;
}

// ---------------------------------------------------------------------
// Drawing.

const css = (c) => `rgba(${c.r},${c.g},${c.b},${c.a / 255})`;

// The pages: { list, el, canvas, img, marks, drawn } by index.
let pages = [];
let zoom = 0; // CSS pixels per point; 0 until the first fit.
let fitting = true;
const visible = new Set();

// Draws a page at the current zoom; returns whether every glyph was there.
function draw(page) {
  const { list, canvas } = page;
  if (!list || !canvas) return true;
  const dpr = window.devicePixelRatio || 1;
  const area = list.width * list.height;
  const scale = Math.min(zoom * dpr, Math.sqrt(MAX_CANVAS_PIXELS / Math.max(1, area)));
  const width = Math.max(1, Math.round(list.width * scale));
  const height = Math.max(1, Math.round(list.height * scale));
  if (canvas.width !== width) canvas.width = width;
  if (canvas.height !== height) canvas.height = height;
  const ctx = canvas.getContext("2d");
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.fillStyle = "#fff";
  ctx.fillRect(0, 0, width, height);
  let complete = true;
  for (const item of list.items) {
    if (item.kind === "glyphs") {
      complete = drawGlyphs(ctx, item, scale) && complete;
    } else if (item.kind === "path") {
      drawPath(ctx, item, scale);
    } else if (item.kind === "frame") {
      const t = FRAME_THICKNESS;
      const { x, y, width: w, height: h } = item.rect;
      ctx.setTransform(scale, 0, 0, scale, 0, 0);
      ctx.strokeStyle = "#000";
      ctx.lineWidth = t;
      ctx.setLineDash([]);
      ctx.strokeRect(x + t / 2, y + t / 2, Math.max(0, w - t), Math.max(0, h - t));
      ctx.setTransform(1, 0, 0, 1, 0, 0);
    }
  }
  page.drawn = complete ? zoom * dpr : 0;
  return complete;
}

function drawGlyphs(ctx, run, scale) {
  const want = Math.round(run.size * scale * 4) / 4;
  if (!(want > 0)) return true;
  // Glyphs larger than the server rasterises are scaled up.
  const size = Math.min(want, MAX_GLYPH_SIZE);
  const stretch = want / size;
  let complete = true;
  let pen = run.origin.x;
  const now = Date.now();
  for (const g of run.glyphs) {
    const x = (pen + g.dx) * scale;
    const y = Math.round((run.origin.y + g.dy) * scale);
    pen += g.advance;
    const ix = Math.floor(x);
    const sub = stretch > 1 ? 0 : Math.min(3, Math.floor((x - ix) * 4));
    const key = `${run.font}|${size}|${g.id}|${sub}`;
    const glyph = glyphCache.get(key);
    if (glyph === undefined) {
      const waiting = glyphCache.has(key) || (failed.get(key) || 0) > now;
      if (!waiting) {
        failed.delete(key);
        wantGlyph(run.font, size, g.id, sub);
      }
      complete = false;
    } else if (glyph && stretch > 1) {
      ctx.drawImage(inColour(glyph, run.fill), x + glyph.left * stretch, y + glyph.top * stretch,
        glyph.width * stretch, glyph.height * stretch);
    } else if (glyph) {
      ctx.drawImage(inColour(glyph, run.fill), ix + glyph.left, y + glyph.top);
    }
  }
  return complete;
}

function drawPath(ctx, shape, scale) {
  const path = new Path2D();
  for (const [op, a, b, c] of shape.segments) {
    if (op === "M") path.moveTo(a.x, a.y);
    else if (op === "L") path.lineTo(a.x, a.y);
    else if (op === "C") path.bezierCurveTo(a.x, a.y, b.x, b.y, c.x, c.y);
    else path.closePath();
  }
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  if (shape.fill) {
    ctx.fillStyle = css(shape.fill);
    ctx.fill(path, shape.evenOdd ? "evenodd" : "nonzero");
  }
  const s = shape.stroke;
  if (s) {
    ctx.strokeStyle = css(s.color);
    ctx.lineWidth = s.thickness;
    ctx.lineCap = s.cap;
    ctx.lineJoin = s.join;
    ctx.miterLimit = s.miterLimit;
    ctx.setLineDash(s.dash ? s.dash.array : []);
    ctx.lineDashOffset = s.dash ? s.dash.phase : 0;
    ctx.stroke(path);
  }
  ctx.setTransform(1, 0, 0, 1, 0, 0);
}

function redrawVisible() {
  const dpr = window.devicePixelRatio || 1;
  for (const i of visible) {
    const page = pages[i];
    if (page && page.drawn !== zoom * dpr) draw(page);
  }
}

// ---------------------------------------------------------------------
// Pages on screen.

const observer = new IntersectionObserver((entries) => {
  for (const entry of entries) {
    const i = Number(entry.target.dataset.index);
    if (entry.isIntersecting) visible.add(i);
    else visible.delete(i);
  }
  redrawVisible();
  sendVisible();
}, { rootMargin: "200px" });

let socket = null;
let lastVisible = "";
function sendVisible() {
  const pagesShown = [...visible].sort((a, b) => a - b);
  const text = JSON.stringify({ type: "visible", pages: pagesShown });
  if (socket && socket.readyState === WebSocket.OPEN && text !== lastVisible) {
    socket.send(text);
    lastVisible = text;
  }
}

function pageElement(i) {
  let page = pages[i];
  if (page) return page;
  const el = document.createElement("div");
  el.className = "page";
  el.dataset.index = String(i);
  el.setAttribute("aria-label", `Page ${i + 1}`);
  const canvas = document.createElement("canvas");
  el.appendChild(canvas);
  // On the page, so that clicks on marks count too.
  el.addEventListener("click", (event) => click(i, event));
  pagesEl.appendChild(el);
  observer.observe(el);
  page = { list: null, el, canvas, img: null, size: null, marks: [], drawn: 0 };
  pages[i] = page;
  return page;
}

function layoutPage(page) {
  const size = page.list || page.size || { width: 595, height: 842 };
  const w = size.width;
  const h = size.height;
  page.el.style.width = `${w * zoom}px`;
  page.el.style.height = `${h * zoom}px`;
}

function fitZoom() {
  const widest = Math.max(1, ...pages.map((p) => (p && (p.list || p.size) ? (p.list || p.size).width : 0)));
  const room = Math.max(200, pagesEl.clientWidth - 32);
  return Math.min(4, Math.max(0.25, room / widest));
}

function setZoom(value) {
  zoom = value;
  for (const page of pages) if (page) layoutPage(page);
  placeMarks();
  redrawVisible();
}

// A page whose display list cannot be decoded is shown as the server
// renders it.
function showImage(page, i, revision) {
  if (page.list) page.size = { width: page.list.width, height: page.list.height };
  page.list = null;
  if (!page.img) {
    page.img = document.createElement("img");
    page.img.alt = `Page ${i + 1}`;
    // The PNG is at 96 dpi: its size in points is three quarters of it.
    page.img.addEventListener("load", () => {
      page.size = { width: page.img.naturalWidth * 0.75, height: page.img.naturalHeight * 0.75 };
      layoutPage(page);
      placeMarks();
    });
    page.el.insertBefore(page.img, page.canvas.nextSibling);
  }
  page.img.src = `/page/${i}.png?token=${encodeURIComponent(token)}&r=${revision}`;
}

// ---------------------------------------------------------------------
// Source sync and diagnostics.

let revision = 0;
let overlays = [];
let diagnostics = [];

function click(i, event) {
  const page = pages[i];
  if (!page || !socket || socket.readyState !== WebSocket.OPEN || !zoom) return;
  const box = page.el.getBoundingClientRect();
  const x = (event.clientX - box.left) / zoom;
  const y = (event.clientY - box.top) / zoom;
  socket.send(JSON.stringify({ type: "click", revision, page: i, x, y }));
}

function where(location) {
  return `${location.path}:${location.start.line}:${location.start.column}`;
}

function showSource(message) {
  sourceLine.textContent = message.location ? where(message.location) : "";
}

function mark(page, rect, className, title) {
  const el = document.createElement("div");
  el.className = `mark ${className}`;
  el.dataset.rect = JSON.stringify(rect);
  if (title) el.title = title;
  page.el.appendChild(el);
  page.marks.push(el);
  placeMark(el);
  return el;
}

function placeMark(el) {
  const [x, y, w, h] = JSON.parse(el.dataset.rect);
  el.style.left = `${x * zoom}px`;
  el.style.top = `${y * zoom}px`;
  el.style.width = `${Math.max(2, w * zoom)}px`;
  el.style.height = `${Math.max(2, h * zoom)}px`;
}

function placeMarks() {
  for (const page of pages) if (page) for (const el of page.marks) placeMark(el);
}

function clearMarks(kind) {
  for (const page of pages) {
    if (!page) continue;
    page.marks = page.marks.filter((el) => {
      if (!el.classList.contains(kind)) return true;
      el.remove();
      return false;
    });
  }
}

function reveal(message) {
  if (message.revision !== revision) return;
  clearMarks("reveal");
  let first = null;
  for (const area of message.areas) {
    const page = pages[area.page];
    if (!page) continue;
    const el = mark(page, area.rect, "reveal");
    if (!first) first = el;
  }
  if (first) scrollToMark(first);
}

function scrollToMark(el) {
  const box = el.getBoundingClientRect();
  const header = document.querySelector("header").getBoundingClientRect().height;
  const roomy = box.top >= header + 40 && box.bottom <= window.innerHeight - 40;
  if (!roomy) {
    window.scrollBy({ top: box.top - (window.innerHeight + header) / 2, behavior: "auto" });
  }
}

function showOverlays() {
  clearMarks("error");
  clearMarks("warning");
  const marked = new Map();
  for (const overlay of overlays) {
    const d = diagnostics[overlay.diagnostic];
    const page = pages[overlay.page];
    if (!d || !page) continue;
    const kind = d.severity === "error" ? "error" : "warning";
    for (const rect of overlay.rects) {
      const el = mark(page, rect, kind, d.message);
      if (!marked.has(overlay.diagnostic)) marked.set(overlay.diagnostic, el);
    }
  }
  return marked;
}

function showDiagnostics(marked) {
  const errors = diagnostics.filter((d) => d.severity === "error").length;
  const warnings = diagnostics.length - errors;
  problems.hidden = diagnostics.length === 0;
  problemsSummary.textContent = [
    errors && `${errors} error${errors === 1 ? "" : "s"}`,
    warnings && `${warnings} warning${warnings === 1 ? "" : "s"}`,
  ].filter(Boolean).join(", ");
  if (errors) problems.open = true;
  list.replaceChildren(...diagnostics.map((d, i) => {
    const li = document.createElement("li");
    li.className = d.severity;
    const at = d.location ? `${where(d.location)}: ` : "";
    li.textContent = `${at}${d.severity}: ${d.message}`;
    const el = marked.get(i);
    if (el) {
      li.classList.add("marked");
      li.title = "Show on the page";
      li.addEventListener("click", () => scrollToMark(el));
    }
    return li;
  }));
}

// ---------------------------------------------------------------------
// The websocket.

// The revision being received: its update message and lists by page.
let incoming = null;

function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws?token=${encodeURIComponent(token)}`);
  ws.binaryType = "arraybuffer";
  socket = ws;
  ws.onmessage = (event) => {
    if (typeof event.data !== "string") {
      if (!incoming) return;
      const bytes = new Uint8Array(event.data);
      if (bytes.length < 4) return;
      const index = new DataView(event.data).getUint32(0, true);
      let decoded;
      try {
        decoded = decode(bytes.subarray(4));
      } catch (e) {
        decoded = { kind: "error", error: e };
      }
      incoming.frames.push([index, decoded]);
      return;
    }
    const message = JSON.parse(event.data);
    if (message.type === "update") incoming = { update: message, frames: [] };
    else if (message.type === "done" && incoming) apply(incoming);
    else if (message.type === "source") showSource(message);
    else if (message.type === "reveal") reveal(message);
  };
  ws.onopen = () => {
    statusLine.textContent = "Connected";
    lastVisible = "";
    sendVisible();
  };
  ws.onclose = () => {
    socket = null;
    incoming = null;
    // The server may come back as another process, with other fonts.
    fontGeneration = null;
    dropGlyphs();
    for (const page of pages) if (page) page.drawn = 0;
    statusLine.textContent = "Disconnected: is `folio watch` still running?";
    setTimeout(connect, 1000);
  };
}

function apply({ update, frames }) {
  incoming = null;
  revision = update.revision;
  if (update.fonts !== undefined && update.fonts !== fontGeneration) {
    if (fontGeneration !== null) {
      dropGlyphs();
      for (const page of pages) if (page) page.drawn = 0;
    }
    fontGeneration = update.fonts;
  }
  while (pages.length > update.pages) {
    const page = pages.pop();
    if (page) {
      observer.unobserve(page.el);
      page.el.remove();
      visible.delete(pages.length);
    }
  }
  // Pages are made in order, since lists arrive visible pages first.
  for (let i = 0; i < update.pages; i++) pageElement(i);
  for (const [i, decoded] of frames) {
    if (i >= update.pages) continue;
    const page = pages[i];
    if (decoded.kind === "list") {
      page.list = decoded.list;
      if (page.img) {
        page.img.remove();
        page.img = null;
      }
      page.drawn = 0;
    } else if (decoded.kind === "error") {
      showImage(page, i, update.revision);
    }
    // A span map changes nothing that is drawn.
  }
  if (fitting || !zoom) zoom = fitZoom();
  for (const page of pages) if (page) layoutPage(page);
  diagnostics = update.diagnostics || [];
  overlays = update.overlays || [];
  showDiagnostics(showOverlays());
  banner.hidden = !update.banner;
  banner.textContent = update.banner || "";
  const what = update.variant ? `${update.variant} (${update.target})` : update.target;
  statusLine.textContent = `${what}: ${update.pages} page${update.pages === 1 ? "" : "s"}, revision ${update.revision}`;
  redrawVisible();
  sendVisible();
}

// ---------------------------------------------------------------------
// Zoom.

function zoomBy(factor) {
  fitting = false;
  const anchor = window.scrollY / Math.max(1, document.body.scrollHeight);
  setZoom(Math.min(8, Math.max(0.1, (zoom || 1) * factor)));
  window.scrollTo({ top: anchor * document.body.scrollHeight });
}

q("zoom-in").addEventListener("click", () => zoomBy(1.25));
q("zoom-out").addEventListener("click", () => zoomBy(0.8));
q("zoom-fit").addEventListener("click", () => {
  fitting = true;
  setZoom(fitZoom());
});
window.addEventListener("keydown", (event) => {
  if (!(event.ctrlKey || event.metaKey)) return;
  if (event.key === "+" || event.key === "=") zoomBy(1.25);
  else if (event.key === "-") zoomBy(0.8);
  else if (event.key === "0") { fitting = true; setZoom(fitZoom()); }
  else return;
  event.preventDefault();
});
window.addEventListener("resize", () => {
  if (fitting) setZoom(fitZoom());
  else redrawVisible();
});

connect();
