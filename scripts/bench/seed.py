#!/usr/bin/env python3
"""Seed both apps' workspaces with the same connection, set to auto-connect.

Both apps carry the same SQLite schema, so the setup path is identical for
each and no clicking is involved. Driving the connection dialog by hand is how
the first attempt ended up with `ws://localhost:8765ws://127.0.0.1:9001` in
the URL column: the field already held a value and typing appended to it.
"""
import json, sqlite3, sys, time

TAURI = "/root/.local/share/com.mmarfeychuk.robot-whisperer/workspace.db"
OURS = "/root/.local/share/robot-whisperer/workspace.db"

def seed(path, kind, url, name="Bench"):
    now = time.strftime("%Y-%m-%dT%H:%M:%S.000000000Z", time.gmtime())
    config = json.dumps({"kind": kind, "url": url, "headers": []})
    c = sqlite3.connect(path)
    c.execute("delete from connections")
    c.execute(
        "insert into connections (name, transport_kind, config_json, auto_connect,"
        " color, created_at, updated_at) values (?,?,?,1,NULL,?,?)",
        (name, kind, config, now, now),
    )
    c.commit()
    got = list(c.execute("select id,name,transport_kind,config_json,auto_connect from connections"))
    c.close()
    return got

if __name__ == "__main__":
    kind, url = sys.argv[1], sys.argv[2]
    for label, path in (("tauri", TAURI), ("ours", OURS)):
        try:
            print(label, seed(path, kind, url))
        except Exception as error:
            print(label, "FAILED", error)
