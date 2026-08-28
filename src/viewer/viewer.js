"use strict";

// Everything here reads what the API already encoded. Hashes arrive
// big-endian and amounts arrive as an AVI string beside their atoms, so this
// file does no encoding of its own -- invariant 5 puts that at the API's edge
// and nowhere else.

const POLL_MS = 2000;
const RECENT = 12;

const $ = (id) => document.getElementById(id);
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

function link(text, onClick) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "link";
  button.textContent = text;
  button.addEventListener("click", onClick);
  return button;
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
  const block = await ask(`/block/${hash}`);
  const heading = document.createElement("h3");
  heading.textContent = `transactions (${block.transaction_count})`;

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
  const transaction = await ask(`/tx/${txid}`);
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
  const held = await ask(`/address/${text}`);
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

async function refresh() {
  const status = await ask("/status");
  put("network", status.network);
  put("height", status.height);
  put("peers", status.peers);
  put("mempool", status.mempool);
  put("tip", status.tip);
  document.title = `Avi Coin — ${status.height}`;

  const from = Math.max(0, status.height - RECENT + 1);
  const page = await ask(`/blocks?from=${from}&count=${RECENT}`);
  // Sorted by height rather than reversed: the page arrives oldest-first, and
  // saying which order this wants beats assuming the API's.
  const newest = page.blocks.slice().sort((a, b) => b.height - a.height);
  $("block-rows").replaceChildren(
    ...newest.map((block) =>
      row([block.height, link(short(block.hash), () => openBlock(block.hash)), when(block.time)]),
    ),
  );

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

  const log = await ask("/log");
  $("log-lines").textContent = log.lines.join("\n");
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
