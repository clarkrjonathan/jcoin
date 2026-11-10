#!/usr/bin/env python3
"""Who mined which blocks on the persisted demo chain.

Reads a node's append-only block log, follows the best chain back from the
highest block, and attributes each block to the recipient of its coinbase
(the first transaction). Keys are named from keys.txt.

    ./miners.py                 # overall, per-epoch, and since the iMac joined
    ./miners.py --from 152      # count from another height
    ./miners.py --log data-jon/blocks.dat
"""
import argparse
import hashlib
import os
import struct
import time

HERE = os.path.dirname(os.path.abspath(__file__))
HEADER_LEN = 156
TX_LEN = 153
EPOCH = 12  # retarget_window
IMAC_FIRST_BLOCK = 152  # first block mined by the iMac node (see docs/HANDOFF.md)


def load(path):
    data = open(path, "rb").read()
    pos, blocks = 0, {}
    while pos + 4 <= len(data):
        n = struct.unpack(">I", data[pos:pos + 4])[0]
        b = data[pos + 4:pos + 4 + n]
        pos += 4 + n
        if len(b) < HEADER_LEN + 2:
            break
        h = hashlib.sha256(b[:HEADER_LEN]).digest()
        count = struct.unpack(">H", b[HEADER_LEN:HEADER_LEN + 2])[0]
        miner = b[HEADER_LEN + 2 + 33:HEADER_LEN + 2 + 65] if count else b""
        blocks[h] = dict(
            height=struct.unpack(">Q", b[4:12])[0],
            prev=b[12:44],
            ts=struct.unpack(">Q", b[108:116])[0],
            target=int.from_bytes(b[116:148], "big"),
            miner=miner,
        )
    return blocks


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--log", default=os.path.join(HERE, "data-noah", "blocks.dat"))
    ap.add_argument("--from", dest="start", type=int, default=IMAC_FIRST_BLOCK)
    args = ap.parse_args()

    names = {}
    for line in open(os.path.join(HERE, "keys.txt")):
        n, a = line.split()
        names[bytes.fromhex(a)] = n

    blocks = load(args.log)
    tip = max(blocks, key=lambda h: blocks[h]["height"])
    chain = []
    while tip in blocks:
        chain.append(blocks[tip])
        tip = blocks[tip]["prev"]
    chain.reverse()
    chain = [b for b in chain if b["height"] > 0]
    stale = len(blocks) - len(chain) - 1  # minus genesis
    label = lambda b: names.get(b["miner"], b["miner"].hex()[:8] or "?")

    def tally(seg):
        t = {}
        for b in seg:
            t[label(b)] = t.get(label(b), 0) + 1
        return " ".join(f"{k}={v}" for k, v in sorted(t.items()))

    print(f"best chain: height {chain[-1]['height']}, {stale} stale blocks in the log")
    print("all blocks:      ", tally(chain))
    since = [b for b in chain if b["height"] >= args.start]
    span = since[-1]["ts"] - since[0]["ts"]
    print(f"from {args.start}:        ", tally(since),
          f"({len(since)} blocks, avg {span / max(1, len(since) - 1):.0f} s/block)")
    print()
    print("epoch      avg s/blk  difficulty  imac/blocks  winners")
    base = chain[0]["target"]
    for s in range(EPOCH, chain[-1]["height"] + 1, EPOCH):
        seg = [b for b in chain if s <= b["height"] < s + EPOCH]
        if len(seg) < EPOCH:
            tag = "(open)"
        else:
            tag = ""
        avg = (seg[-1]["ts"] - seg[0]["ts"]) / max(1, len(seg) - 1)
        im = sum(1 for b in seg if label(b) == "imac")
        print(f"{s}-{s + EPOCH - 1:<5} {avg:8.0f}   x{base / seg[0]['target']:<8.1f} "
              f"{im:>3}/{len(seg):<3}      {tally(seg)} {tag}")
    print(f"\nnow: {time.strftime('%F %T')}")


if __name__ == "__main__":
    main()
