// A minimal viewer for `folio watch`: the pages as images that the server
// renders, refreshed when the websocket says which pages changed. The
// viewer that draws display lists itself comes later (#51).
"use strict";

const token = new URLSearchParams(location.search).get("token") || "";
const pages = document.getElementById("pages");
const statusLine = document.getElementById("status");
const banner = document.getElementById("banner");
const list = document.getElementById("diagnostics");

function connect() {
  const ws = new WebSocket(`ws://${location.host}/ws?token=${encodeURIComponent(token)}`);
  ws.onmessage = (event) => {
    if (typeof event.data !== "string") return; // display lists: not drawn here
    const message = JSON.parse(event.data);
    if (message.type === "update") update(message);
  };
  ws.onopen = () => { statusLine.textContent = "Connected"; };
  ws.onclose = () => {
    statusLine.textContent = "Disconnected: is `folio watch` still running?";
    setTimeout(connect, 1000);
  };
}

function update(message) {
  while (pages.children.length > message.pages) pages.lastChild.remove();
  for (const i of message.lists.concat(message.spans)) {
    let img = pages.children[i];
    if (!img) {
      while (pages.children.length <= i) {
        img = document.createElement("img");
        img.alt = `Page ${pages.children.length + 1}`;
        pages.appendChild(img);
      }
    }
    if (message.lists.includes(i)) {
      img.src = `/page/${i}.png?token=${encodeURIComponent(token)}&r=${message.revision}`;
    }
  }
  banner.hidden = !message.banner;
  banner.textContent = message.banner || "";
  list.replaceChildren(...message.diagnostics.map((d) => {
    const li = document.createElement("li");
    li.className = d.severity;
    const where = d.location ? `${d.location.path}:${d.location.start.line}: ` : "";
    li.textContent = `${where}${d.severity}: ${d.message}`;
    return li;
  }));
  const what = message.variant ? `${message.variant} (${message.target})` : message.target;
  statusLine.textContent = `${what}: ${message.pages} pages, revision ${message.revision}`;
}

connect();
