"use strict";

// Everything here reads what the API already encoded. Hashes arrive
// big-endian and amounts arrive as an AVI string beside their atoms, so this
// file does no encoding of its own -- invariant 5 puts that at the API's edge
// and nowhere else.

const POLL_MS = 2000;
const RECENT = 12;

const $ = (id) => document.getElementById(id);

// How far the log has been read. `/log` without it returns the *oldest* lines
// the node still holds, which on anything long-running is a frozen startup
// transcript under a heading called "Log".
let seen = null;
const field = (name) => document.querySelectorAll(`[data-field="${name}"]`);

function put(name, value) {
  for (const node of field(name)) node.textContent = value;
}

async function ask(path) {
  const response = await fetch(path, { headers: { accept: "application/json" } });
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
  return body;
}

function short(hash) {
  return hash.length > 20 ? `${hash.slice(0, 10)}…${hash.slice(-6)}` : hash;
}

function when(seconds) {
  return new Date(seconds * 1000).toISOString().replace("T", " ").slice(0, 19);
}

function row(cells) {
  const tr = document.createElement("tr");
  for (const cell of cells) {
    const td = document.createElement("td");
    if (cell instanceof Node) td.append(cell);
    else td.textContent = cell;
    tr.append(td);
  }
  return tr;
}

// Every link opens something that can 404 — genesis has no previous block,
// and a funding transaction older than the API's scan window is gone from it.
// Without this, clicking one of those does nothing at all and says nothing.
function link(text, open) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "link";
  button.textContent = text;
  button.addEventListener("click", async () => {
    try {
      await open();
    } catch (why) {
      showDetail("Not found", said(why.message, "error"));
    }
  });
  return button;
}

function said(text, className = "") {
  const p = document.createElement("p");
  p.className = className;
  p.textContent = text;
  return p;
}

function pairs(entries) {
  const dl = document.createElement("dl");
  for (const [name, value] of entries) {
    const dt = document.createElement("dt");
    dt.textContent = name;
    const dd = document.createElement("dd");
    if (value instanceof Node) dd.append(value);
    else dd.textContent = value;
    dl.append(dt, dd);
  }
  return dl;
}

function showDetail(title, ...nodes) {
  $("detail-title").textContent = title;
  $("detail-body").replaceChildren(...nodes);
  $("detail").hidden = false;
  $("detail").scrollIntoView({ behavior: "smooth", block: "start" });
}

async function openBlock(hash) {
  const block = await ask(`/block/${encodeURIComponent(hash)}`);
  const shown = block.transactions.length;
  const heading = document.createElement("h3");
  heading.textContent =
    shown < block.transaction_count
      ? `transactions (showing ${shown} of ${block.transaction_count})`
      : `transactions (${block.transaction_count})`;

  const list = document.createElement("table");
  const body = document.createElement("tbody");
  for (const transaction of block.transactions) {
    body.append(
      row([
        link(short(transaction.txid), () => openTransaction(transaction.txid)),
        transaction.coinbase ? "coinbase" : `${transaction.inputs.length} in`,
        `${transaction.outputs.length} out`,
      ]),
    );
  }
  list.append(body);

  showDetail(
    `Block ${block.height}`,
    pairs([
      ["hash", block.hash],
      ["previous", link(short(block.previous_block), () => openBlock(block.previous_block))],
      ["merkle root", block.merkle_root],
      ["time", `${when(block.time)} UTC`],
      ["bits", block.n_bits],
      ["nonce", block.nonce],
      ["size", `${block.size} bytes`],
      ["confirmations", block.confirmations],
      ["on best chain", block.best_chain ? "yes" : "no"],
    ]),
    heading,
    list,
  );
}

function outputs(transaction) {
  const table = document.createElement("table");
  const body = document.createElement("tbody");
  for (const output of transaction.outputs) {
    body.append(row([output.index, `${output.avi} AVI`, short(output.script_pubkey)]));
  }
  table.append(body);
  return table;
}

function inputs(transaction) {
  if (transaction.coinbase) {
    const none = document.createElement("p");
    none.className = "empty";
    none.textContent = "A coinbase spends nothing; it is where coins come from.";
    return none;
  }

  const table = document.createElement("table");
  const body = document.createElement("tbody");
  for (const input of transaction.inputs) {
    body.append(
      row([
        link(short(input.previous_output.txid), () => openTransaction(input.previous_output.txid)),
        input.previous_output.index,
        `${input.witness_items} witness items`,
      ]),
    );
  }
  table.append(body);
  return table;
}

async function openTransaction(txid) {
  const transaction = await ask(`/tx/${encodeURIComponent(txid)}`);
  const where = transaction.block
    ? [["block", link(short(transaction.block), () => openBlock(transaction.block))],
       ["height", transaction.height]]
    : [["status", "in the mempool"]];

  const inputHeading = document.createElement("h3");
  inputHeading.textContent = "inputs";
  const outputHeading = document.createElement("h3");
  outputHeading.textContent = "outputs";

  showDetail(
    "Transaction",
    pairs([
      ["txid", transaction.txid],
      ["wtxid", transaction.wtxid],
      ...where,
      ["size", `${transaction.size} bytes`],
    ]),
    inputHeading,
    inputs(transaction),
    outputHeading,
    outputs(transaction),
  );
}

async function openAddress(text) {
  const held = await ask(`/address/${encodeURIComponent(text)}`);
  const heading = document.createElement("h3");
  heading.textContent = `unspent (${held.unspent.length})`;

  const table = document.createElement("table");
  const body = document.createElement("tbody");
  for (const coin of held.unspent) {
    body.append(
      row([
        link(short(coin.txid), () => openTransaction(coin.txid)),
        coin.index,
        `${coin.avi} AVI`,
        coin.coinbase ? `coinbase @ ${coin.height}` : `@ ${coin.height}`,
      ]),
    );
  }
  table.append(body);

  const empty = document.createElement("p");
  empty.className = "empty";
  empty.textContent = "This address holds nothing.";

  showDetail(
    "Address",
    pairs([[ "address", held.address ], [ "balance", `${held.avi} AVI` ], [ "atoms", held.atoms ]]),
    heading,
    held.unspent.length ? table : empty,
  );
}

// A block hash, a txid and an address are told apart by shape: 64 hex
// characters is one of the two hashes, anything else is an address. Trying the
// block first and the transaction second costs one 404 and saves asking the
// visitor which kind of thing they pasted.
async function lookup(text) {
  const attempts = /^[0-9a-fA-F]{64}$/.test(text)
    ? [() => openBlock(text), () => openTransaction(text)]
    : [() => openAddress(text)];

  let last;
  for (const attempt of attempts) {
    try {
      await attempt();
      return;
    } catch (why) {
      last = why;
    }
  }
  throw last;
}

/// Each section is fetched on its own, so one endpoint failing leaves the
/// others current rather than frozen at whatever they said last.
async function refresh() {
  const sections = [status, blocks, mempool, peers, log];
  const outcomes = await Promise.allSettled(sections.map((section) => section()));
  const failed = outcomes.find((outcome) => outcome.status === "rejected");
  if (failed) throw failed.reason;
}

async function status() {
  const status = await ask("/status");
  put("network", status.network);
  put("height", status.height);
  put("peers", status.peers);
  put("mempool", status.mempool);
  put("tip", status.tip);
  document.title = `Avi Coin — ${status.height}`;
  return status;
}

async function blocks() {
  const at = Number($("status").querySelector('[data-field="height"]').textContent);
  const from = Math.max(0, (Number.isFinite(at) ? at : 0) - RECENT + 1);
  const page = await ask(`/blocks?from=${from}&count=${RECENT}`);
  // Sorted by height rather than reversed: the page arrives oldest-first, and
  // saying which order this wants beats assuming the API's.
  const newest = page.blocks.slice().sort((a, b) => b.height - a.height);
  $("block-rows").replaceChildren(
    ...newest.map((block) =>
      row([block.height, link(short(block.hash), () => openBlock(block.hash)), when(block.time)]),
    ),
  );
  $("no-blocks").hidden = newest.length > 0;
}

async function mempool() {
  const mempool = await ask("/mempool");
  put("mempool-count", mempool.count);
  $("mempool-rows").replaceChildren(
    ...mempool.transactions.map((transaction) =>
      row([
        link(short(transaction.txid), () => openTransaction(transaction.txid)),
        `${transaction.fee_atoms} atoms`,
        `${transaction.size} B`,
      ]),
    ),
  );
  $("no-mempool").hidden = mempool.count > 0;
}

async function peers() {
  const peers = await ask("/peers");
  put("peer-count", peers.count);
  $("no-peers").hidden = peers.count > 0;
  $("peer-rows").replaceChildren(
    ...peers.peers.map((peer) =>
      row([
        peer.listening || "(not yet known)",
        peer.direction,
        peer.handshake,
        `${peer.connected_seconds}s`,
      ]),
    ),
  );
}

async function log() {
  const log = await ask(seen === null ? "/log" : `/log?since=${seen}`);
  if (log.lines.length) {
    const pane = $("log-lines");
    const atBottom = pane.scrollTop + pane.clientHeight >= pane.scrollHeight - 4;
    pane.append(`${log.lines.join("\n")}\n`);
    // Only if the reader was already there: re-anchoring a pane somebody has
    // scrolled up in is the same rudeness as resetting it every two seconds.
    if (atBottom) pane.scrollTop = pane.scrollHeight;
  }
  seen = log.next;
  $("no-log").hidden = $("log-lines").textContent.length > 0;
}

$("close-detail").addEventListener("click", () => {
  $("detail").hidden = true;
});

$("lookup-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const text = $("lookup-text").value.trim();
  const error = $("lookup-error");
  error.hidden = true;
  if (!text) return;

  try {
    await lookup(text);
  } catch (why) {
    error.textContent = why.message;
    error.hidden = false;
  }
});

$("submit-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const result = $("submit-result");
  result.hidden = false;
  result.className = "";
  result.textContent = "sending…";

  try {
    const response = await fetch("/tx", {
      method: "POST",
      body: $("submit-text").value.trim(),
    });
    const body = await response.json();
    if (!response.ok) throw new Error(body.error);
    result.className = "ok";
    result.textContent = `accepted: ${body.txid}`;
    $("submit-text").value = "";
    refresh();
  } catch (why) {
    // The reason the node gave, not a generic failure. A demo where a
    // submission fails silently is worse than one where it fails.
    result.className = "bad";
    result.textContent = `refused: ${why.message}`;
  }
});

async function poll() {
  try {
    await refresh();
  } catch (why) {
    put("tip", `the node is not answering: ${why.message}`);
  }
  setTimeout(poll, POLL_MS);
}

poll();
