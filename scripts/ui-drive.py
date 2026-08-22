#!/usr/bin/env python3
"""Drive the running Tauri app through WebDriver.

Uses `tauri-driver` (Tauri's official WebDriver harness) talking to
WebKitWebDriver, so this exercises the real app in the real webview rather than
a browser stand-in. Clicks go through the WebDriver element-click endpoint, so
they are real synthesised input events, not `element.click()` calls that can
bypass handlers. Only the standard library is used.

    scripts/ui-drive.py dump                     # print the rendered UI outline
    scripts/ui-drive.py click LABEL [LABEL...]   # click controls by label, then dump
    scripts/ui-drive.py dashboard3d OUT.png      # build a 3D dashboard, screenshot

Environment:
    RW_APP      path to the app binary (default /usr/bin/robot-whisperer)
    RW_DRIVER   tauri-driver binary   (default tauri-driver)
"""
import base64
import json
import os
import subprocess
import sys
import time
import urllib.request

DRIVER_URL = "http://127.0.0.1:4444"
APP = os.environ.get("RW_APP", "/usr/bin/robot-whisperer")
DRIVER = os.environ.get("RW_DRIVER", "tauri-driver")
ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"


def rq(method, path, payload=None, timeout=60):
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(
        DRIVER_URL + path, data=data, method=method,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode())


class Session:
    def __init__(self):
        self.proc = subprocess.Popen(
            [DRIVER], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        self.id = None
        last = None
        for _ in range(30):
            time.sleep(1)
            try:
                body = rq("POST", "/session", {
                    "capabilities": {"alwaysMatch": {"tauri:options": {"application": APP}}}
                })
                self.id = body.get("value", {}).get("sessionId") or body.get("sessionId")
                if self.id:
                    return
            except Exception as exc:
                last = exc
        raise SystemExit(f"could not start a WebDriver session: {last}")

    def close(self):
        try:
            if self.id:
                rq("DELETE", f"/session/{self.id}", timeout=15)
        except Exception:
            pass
        self.proc.terminate()

    def js(self, script, *args):
        return rq("POST", f"/session/{self.id}/execute/sync",
                  {"script": script, "args": list(args)}).get("value")

    def find_all(self, css):
        body = rq("POST", f"/session/{self.id}/elements",
                  {"using": "css selector", "value": css})
        return [v[ELEMENT_KEY] for v in body.get("value", [])]

    def label_of(self, eid):
        for attr in ("title", "aria-label"):
            body = rq("GET", f"/session/{self.id}/element/{eid}/attribute/{attr}")
            if body.get("value"):
                return body["value"].strip()
        body = rq("GET", f"/session/{self.id}/element/{eid}/text")
        return (body.get("value") or "").strip()

    def click_element(self, eid):
        rq("POST", f"/session/{self.id}/element/{eid}/click", {})

    def screenshot(self, path):
        png = base64.b64decode(rq("GET", f"/session/{self.id}/screenshot")["value"])
        with open(path, "wb") as fh:
            fh.write(png)
        return len(png)


CLICKABLE = "button,[role=button],[role=menuitem],a"


def controls(session):
    """Every clickable control with its visible label."""
    out = []
    for eid in session.find_all(CLICKABLE):
        try:
            out.append((eid, session.label_of(eid)))
        except Exception:
            pass
    return out


def click_label(session, want, required=True):
    """Click the first control whose label matches (case-insensitive prefix)."""
    want_l = want.lower()
    for eid, label in controls(session):
        if label.lower() == want_l or label.lower().startswith(want_l):
            session.click_element(eid)
            print(f"  clicked {label!r}")
            time.sleep(2)
            return True
    if required:
        raise SystemExit(f"no control labelled {want!r}; available: "
                         f"{[l for _, l in controls(session)]}")
    print(f"  (no control labelled {want!r})")
    return False


def wait_for(session, expr, what, attempts=40):
    for _ in range(attempts):
        try:
            if session.js(f"return !!({expr})"):
                return True
        except Exception:
            pass
        time.sleep(1)
    raise SystemExit(f"timed out waiting for {what}")


def wait_ready(session):
    wait_for(session, "document.body && document.body.innerText.length > 0", "the app to render")


def cmd_dump(session):
    wait_ready(session)
    print("=== innerText ===")
    print(session.js("return document.body.innerText.slice(0, 1200)"))
    print("\n=== clickable controls ===")
    for i, (_, label) in enumerate(controls(session)):
        print(f"  {i}: {label[:60]}")


def cmd_dashboard3d(session, out_png):
    """Create a dashboard, add the 3D robot pane, and capture it."""
    wait_ready(session)
    print("building a dashboard with a 3D robot view")
    click_label(session, "New dashboard")

    click_label(session, "Add pane")
    # The pane tiles are labelled by their description text; the 3D robot pane
    # is "Pose a photorealistic 3D robot with manual joint controls."
    click_label(session, "Pose a photorealistic 3D robot")

    time.sleep(6)  # let three.js create its context and draw a frame
    size = session.screenshot(out_png)
    print(f"  screenshot written: {out_png} ({size} bytes)")

    canvases = session.js("""
        return Array.from(document.querySelectorAll('canvas')).map(c => {
          let kind = 'none';
          try {
            const gl = c.getContext('webgl2') || c.getContext('webgl');
            kind = gl ? 'webgl' : '2d-or-none';
          } catch (e) { kind = 'error:' + e.message; }
          return c.width + 'x' + c.height + ' ctx=' + kind;
        });
    """)
    print(f"  canvases: {canvases}")
    return canvases


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    action = sys.argv[1]
    session = Session()
    try:
        if action == "dump":
            cmd_dump(session)
        elif action == "click":
            wait_ready(session)
            for label in sys.argv[2:]:
                click_label(session, label, required=False)
            cmd_dump(session)
        elif action == "dashboard3d":
            out = sys.argv[2] if len(sys.argv) > 2 else "/tmp/dashboard3d.png"
            cmd_dashboard3d(session, out)
        else:
            raise SystemExit(f"unknown action {action}")
    finally:
        session.close()


if __name__ == "__main__":
    main()
