#!/usr/bin/env python3
"""A GUI/backend protocol client with the standard library only.

Does the handshake, then reads the rail snapshot, through `gui/call` on the
crab's A2A gateway. The protocol is docs/gui-backend-protocol.md.

    python3 scripts/client-gui-backend.py --url http://127.0.0.1:18790 --token "$TOKEN"
"""

import argparse
import json
import sys
import urllib.error
import urllib.request

# Wire names, pinned to their op type. tests/gui_backend_protocol_test.py
# fails when the type lands under a different name.
HELLO_OP = "hello.handshake"  # op: Hello
RAIL_OP = "sessions.rail_snapshot"  # op: RailSnapshot


def call(url, token, name, params, rpc_id):
    body = {"jsonrpc": "2.0", "id": rpc_id, "method": "gui/call", "params": {"name": name, "params": params}}
    req = urllib.request.Request(url.rstrip("/") + "/a2a/v1", json.dumps(body).encode(), method="POST")
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            reply = json.load(resp)
    except urllib.error.HTTPError as e:
        reply = json.load(e)
    if "error" in reply:
        err = reply["error"]
        raise SystemExit(f"{name}: {err.get('code')} {err.get('message')} {json.dumps(err.get('data'))}")
    return reply.get("result")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--url", default="http://127.0.0.1:18790")
    ap.add_argument("--token", default=None, help="a2a api_key or an ocpair_ token")
    args = ap.parse_args()
    hello = call(args.url, args.token, HELLO_OP, {}, 1)
    print("hello:", json.dumps(hello, indent=2))
    rail = call(args.url, args.token, RAIL_OP, {}, 2)
    print("rail:", json.dumps(rail, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
