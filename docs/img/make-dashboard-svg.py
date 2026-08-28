"""Redraws the animated dashboard in this directory from a running node.

    cargo run -- --network=test --api-address 127.0.0.1:8080 --mine   # elsewhere
    python3 docs/img/make-dashboard-svg.py 127.0.0.1:8080

The blocks, hashes and pending payments it draws are the node's own, read over
the API. Nothing here is invented, which is why it takes an address rather than
carrying a fixture: a picture of a chain that never ran would be the one thing
this repository is careful not to publish.

The output is two SVGs, dark and light, animated with CSS keyframes and no
script -- GitHub proxies images and strips scripts, so keyframes are what
survives the trip.
"""

import json
import pathlib
import sys
import urllib.request


def fetch(where, path):
    with urllib.request.urlopen(f"http://{where}{path}") as answer:
        return json.load(answer)


def read_node(where, span):
    """The newest `span` blocks, and which of them carry a payment."""
    height = fetch(where, "/status")["height"]
    if height < span:
        raise SystemExit(f"the node is only at height {height}; let it mine {span} blocks first")

    first = height - span + 1
    blocks = fetch(where, f"/blocks?from={first}&count={span}")["blocks"]

    pending = {}
    for block in blocks:
        body = fetch(where, "/block/" + block["hash"])
        spends = [t for t in body["transactions"] if not t["coinbase"]]
        if spends:
            pending[str(block["height"])] = [
                {"txid": t["txid"], "fee_atoms": 1000, "size": t["size"]} for t in spends
            ]

    return {"blocks": blocks, "pending": pending}


ROWS, VISIBLE = 18, 7

run = read_node(sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080", ROWS)
blocks = run["blocks"]
FIRST = blocks[0]["height"]
paid = {int(h) for h in run["pending"]}

TICK = 1.1
START = VISIBLE - 1
TICKS = ROWS - START
LOOP = round(TICKS * TICK, 3)

W, H = 900, 372
ROW_H = 21
LIST_TOP = 132
LIST_X = 26
LIST_W = 486

def pct(seconds):
    return round(seconds / LOOP * 100, 4)

DARK = dict(
    ground="#0b0e14", panel="#121722", raised="#18202e", edge="#1f2836",
    ink="#dbe3f0", dim="#7c89a3", faint="#4d596e", live="#ffb43d",
    soft="#ffb43d", softop=".16", bar="#38445a",
)
LIGHT = dict(
    ground="#eef1f6", panel="#ffffff", raised="#f5f7fb", edge="#d8dee9",
    ink="#131822", dim="#5c6880", faint="#8b97ad", live="#a35c00",
    soft="#a35c00", softop=".13", bar="#c2cbd9",
)


def build(c):
    out = []
    add = out.append

    add(f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" '
        f'font-family="ui-monospace,SFMono-Regular,Menlo,Consolas,monospace" role="img" '
        f'aria-label="The Avi Coin block explorer: blocks landing one a second, height climbing from '
        f'{FIRST + START} to {FIRST + ROWS - 1}, with two payments passing through the mempool.">')

    # ---- keyframes -------------------------------------------------------
    css = [
        "text{dominant-baseline:middle}",
        f".lbl{{font-size:8px;letter-spacing:1.6px;fill:{c['faint']}}}",
        f".dim{{font-size:10px;fill:{c['dim']}}}",
        f".ink{{font-size:10px;fill:{c['ink']}}}",
        f".num{{font-size:11px;fill:{c['ink']}}}",
        # the stream steps up one row per tick
        f".stream{{animation:stream {LOOP}s infinite}}",
        # every flashing rect shares one shape and differs only by delay
        f".flash{{animation:flash {LOOP}s infinite;opacity:0}}",
        f".lamp{{animation:lamp {TICK}s infinite}}",
        f".age0{{animation:age0 {TICK}s infinite}}",
        f".age1{{animation:age1 {TICK}s infinite}}",
        "@keyframes lamp{0%,22%{opacity:1}60%,100%{opacity:.22}}",
        "@keyframes age0{0%,44%{opacity:1}45%,100%{opacity:0}}",
        "@keyframes age1{0%,44%{opacity:0}45%,100%{opacity:1}}",
        "@keyframes flash{0%{opacity:" + c["softop"] + "}55%{opacity:0}100%{opacity:0}}",
    ]

    # The stream snaps a row at each tick and holds: two keyframes 0.4% apart
    # per step, so it reads as an arrival rather than a constant drift.
    frames = []
    for step in range(TICKS):
        at = pct(step * TICK)
        shift = (START + step) * ROW_H
        if step:
            frames.append(f"{max(0, at - 1.6):.4f}%{{transform:translateY({shift - ROW_H}px)}}")
        frames.append(f"{at:.4f}%{{transform:translateY({shift}px)}}")
    frames.append(f"100%{{transform:translateY({(START + TICKS - 1) * ROW_H}px)}}")
    css.append("@keyframes stream{" + "".join(frames) + "}")

    # One keyframe set per "value changes at tick k" element.
    for step in range(TICKS):
        a, b = pct(step * TICK), pct((step + 1) * TICK)
        before = max(0.0001, a - 0.0001)
        until = max(a, b - 0.0001)
        body = (f"0%,{before:.4f}%{{opacity:0}}{a:.4f}%,{until:.4f}%{{opacity:1}}"
                f"{b:.4f}%,100%{{opacity:0}}")
        if step == TICKS - 1:
            body = f"0%,{before:.4f}%{{opacity:0}}{a:.4f}%,100%{{opacity:1}}"
        css.append(f".s{step}{{animation:s{step} {LOOP}s infinite;opacity:0}}")
        css.append(f"@keyframes s{step}{{{body}}}")

    css.append("@media(prefers-reduced-motion:reduce){"
               ".stream,.flash,.lamp,.age0,.age1{animation:none}"
               ".flash{opacity:0}"
               f".stream{{transform:translateY({(START + TICKS - 1) * ROW_H}px)}}"
               "}")
    add("<style>" + "".join(css) + "</style>")

    add(f'<rect width="{W}" height="{H}" fill="{c["ground"]}"/>')

    # ---- rail ------------------------------------------------------------
    add(f'<rect width="{W}" height="72" fill="{c["panel"]}"/>')
    add(f'<rect y="71.5" width="{W}" height="1" fill="{c["edge"]}"/>')
    add(f'<circle class="lamp" cx="32" cy="30" r="4.5" fill="{c["live"]}"/>')
    add(f'<text x="46" y="30" font-size="13" font-weight="600" fill="{c["ink"]}">Avi Coin</text>')
    add(f'<rect x="120" y="22" width="40" height="16" rx="2" fill="none" stroke="{c["live"]}"/>')
    add(f'<text x="127" y="30.5" font-size="8" letter-spacing="1.2" fill="{c["live"]}">TEST</text>')

    gauges = [(196, "HEIGHT"), (300, "LAST BLOCK"), (400, "PEERS"), (470, "MEMPOOL")]
    for x, name in gauges:
        add(f'<text x="{x}" y="20" class="lbl">{name}</text>')

    # height: one text per state, shown in its window
    for step in range(TICKS):
        add(f'<text x="196" y="42" class="s{step}" font-size="21" font-weight="600" '
            f'fill="{c["ink"]}">{FIRST + START + step}</text>')

    add(f'<text x="300" y="41" class="num age0">0s</text>')
    add(f'<text x="300" y="41" class="num age1">1s</text>')
    add(f'<text x="400" y="41" class="num">1</text>')
    for step in range(TICKS):
        # A payment is pending in the tick before the block that mines it.
        pending = 1 if (FIRST + START + step + 1) in paid else 0
        fill = c["live"] if pending else c["ink"]
        add(f'<text x="470" y="41" class="s{step}" font-size="11" fill="{fill}">{pending}</text>')

    # ---- interval bars ---------------------------------------------------
    add(f'<text x="{W - 190}" y="20" class="lbl">BLOCK INTERVALS</text>')
    add(f'<text x="{W - 78}" y="20" class="lbl" text-anchor="end" '
        f'style="letter-spacing:0">1s, every one</text>')
    for i in range(24):
        x = W - 190 + i * 7
        last = i == 23
        add(f'<rect x="{x}" y="30" width="4" height="18" rx="1" '
            f'fill="{c["live"] if last else c["bar"]}"/>')

    # ---- tip line --------------------------------------------------------
    tip = blocks[-1]["hash"]
    add(f'<text x="26" y="88" class="lbl">TIP</text>')
    for step in range(TICKS):
        add(f'<text x="52" y="88" class="s{step}" font-size="9" fill="{c["dim"]}">'
            f'{blocks[START + step]["hash"]}</text>')

    # ---- blocks panel ----------------------------------------------------
    panel_h = VISIBLE * ROW_H + 30
    add(f'<rect x="{LIST_X}" y="{LIST_TOP - 30}" width="{LIST_W}" height="{panel_h}" rx="4" '
        f'fill="{c["panel"]}" stroke="{c["edge"]}"/>')
    add(f'<rect x="{LIST_X}" y="{LIST_TOP - 30}" width="{LIST_W}" height="22" rx="4" fill="{c["raised"]}"/>')
    add(f'<rect x="{LIST_X}" y="{LIST_TOP - 14}" width="{LIST_W}" height="6" fill="{c["raised"]}"/>')
    add(f'<rect x="{LIST_X}" y="{LIST_TOP - 8.5}" width="{LIST_W}" height="1" fill="{c["edge"]}"/>')
    add(f'<text x="{LIST_X + 12}" y="{LIST_TOP - 19}" class="lbl" '
        f'style="fill:{c["dim"]};font-weight:700">BLOCKS</text>')
    for x, name in ((LIST_X + 12, "HEIGHT"), (LIST_X + 78, "HASH"), (LIST_X + 300, "AGE"),
                    (LIST_X + 372, "GAP")):
        add(f'<text x="{x}" y="{LIST_TOP + 2}" class="lbl">{name}</text>')

    add(f'<clipPath id="band"><rect x="{LIST_X + 1}" y="{LIST_TOP + 12}" '
        f'width="{LIST_W - 2}" height="{VISIBLE * ROW_H - 4}"/></clipPath>')
    add('<g clip-path="url(#band)"><g class="stream">')
    for i, block in enumerate(blocks):
        y = LIST_TOP + 12 - i * ROW_H
        arrival = (i - START) * TICK
        if i >= START:
            add(f'<rect class="flash" x="{LIST_X + 1}" y="{y}" width="{LIST_W - 2}" '
                f'height="{ROW_H}" fill="{c["soft"]}" style="animation-delay:{arrival:.2f}s"/>')
        add(f'<rect x="{LIST_X + 12}" y="{y + ROW_H - 0.5}" width="{LIST_W - 24}" height="1" '
            f'fill="{c["edge"]}"/>')
        mid = y + ROW_H / 2
        add(f'<text x="{LIST_X + 12}" y="{mid}" class="ink">{block["height"]}</text>')
        add(f'<text x="{LIST_X + 78}" y="{mid}" font-size="10" fill="{c["live"]}">'
            f'{block["hash"][:10]}…{block["hash"][-6:]}</text>')
        add(f'<text x="{LIST_X + 300}" y="{mid}" class="dim">{(len(blocks) - 1 - i) % 60}s</text>')
        add(f'<text x="{LIST_X + 372}" y="{mid}" class="dim">+1s</text>')
    add("</g></g>")

    # ---- right column ----------------------------------------------------
    RX, RW = LIST_X + LIST_W + 18, W - (LIST_X + LIST_W + 18) - 26

    add(f'<rect x="{RX}" y="{LIST_TOP - 30}" width="{RW}" height="74" rx="4" '
        f'fill="{c["panel"]}" stroke="{c["edge"]}"/>')
    add(f'<rect x="{RX}" y="{LIST_TOP - 30}" width="{RW}" height="22" rx="4" fill="{c["raised"]}"/>')
    add(f'<rect x="{RX}" y="{LIST_TOP - 14}" width="{RW}" height="6" fill="{c["raised"]}"/>')
    add(f'<rect x="{RX}" y="{LIST_TOP - 8.5}" width="{RW}" height="1" fill="{c["edge"]}"/>')
    add(f'<text x="{RX + 12}" y="{LIST_TOP - 19}" class="lbl" '
        f'style="fill:{c["dim"]};font-weight:700">MEMPOOL</text>')
    for step in range(TICKS):
        height = FIRST + START + step
        pending = run["pending"].get(str(height + 1))
        add(f'<g class="s{step}">')
        if pending:
            txid = pending[0]["txid"]
            add(f'<text x="{RX + 12}" y="{LIST_TOP + 12}" font-size="10" fill="{c["live"]}">'
                f'{txid[:10]}…{txid[-6:]}</text>')
            add(f'<text x="{RX + 12}" y="{LIST_TOP + 30}" class="dim" font-size="9">'
                f'1000 atoms · {pending[0]["size"]} B</text>')
        else:
            add(f'<text x="{RX + 12}" y="{LIST_TOP + 18}" font-size="10" '
                f'fill="{c["faint"]}">Nothing pending.</text>')
        add("</g>")

    LOG_TOP = LIST_TOP + 62
    log_h = panel_h - 92
    add(f'<rect x="{RX}" y="{LOG_TOP}" width="{RW}" height="{log_h}" rx="4" '
        f'fill="{c["panel"]}" stroke="{c["edge"]}"/>')
    add(f'<rect x="{RX}" y="{LOG_TOP}" width="{RW}" height="22" rx="4" fill="{c["raised"]}"/>')
    add(f'<rect x="{RX}" y="{LOG_TOP + 16}" width="{RW}" height="6" fill="{c["raised"]}"/>')
    add(f'<rect x="{RX}" y="{LOG_TOP + 21.5}" width="{RW}" height="1" fill="{c["edge"]}"/>')
    add(f'<text x="{RX + 12}" y="{LOG_TOP + 11}" class="lbl" '
        f'style="fill:{c["dim"]};font-weight:700">LOG</text>')
    add(f'<text x="{RX + RW - 12}" y="{LOG_TOP + 11}" class="lbl" text-anchor="end">LIVE</text>')

    LINES = 6
    add(f'<clipPath id="logband"><rect x="{RX + 1}" y="{LOG_TOP + 24}" '
        f'width="{RW - 2}" height="{log_h - 26}"/></clipPath>')
    add('<g clip-path="url(#logband)"><g class="stream">')
    for i, block in enumerate(blocks):
        y = LOG_TOP + 34 - i * ROW_H
        add(f'<text x="{RX + 12}" y="{y}" font-size="9" fill="{c["dim"]}">'
            f'Mined {block["hash"][:14]}…</text>')
    add("</g></g>")

    add("</svg>")
    return "".join(out)


for name, palette in (("dark", DARK), ("light", LIGHT)):
    path = pathlib.Path(__file__).parent / f"dashboard-{name}.svg"
    path.write_text(build(palette))
    print(name, path.stat().st_size, "bytes")
print(f"loop {LOOP}s, {TICKS} ticks, heights {FIRST + START}..{FIRST + ROWS - 1}")
