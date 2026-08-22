#!/bin/bash
# Launch Robot Whisperer headlessly and prove it actually rendered.
#
# Runs INSIDE a target root filesystem (chroot, container or a real machine).
# Needs: Xvfb, xwininfo, ImageMagick's `import`, python3.
#
#   scripts/verify-launch.sh <label> <outdir> <command> [args...]
#
# Exit 0 only if a top-level window appeared AND its pixels look like a drawn
# UI. "The process is still alive" is not enough: a Tauri app whose WebKit web
# process fails to start shows a perfectly stable, perfectly blank window.
set -u

LABEL="${1:?usage: verify-launch.sh <label> <outdir> <command> [args...]}"
OUTDIR="${2:?missing outdir}"
shift 2
[ "$#" -gt 0 ] || { echo "missing command"; exit 2; }
DISPLAY_NUM="${DISPLAY_NUM:-:99}"
SETTLE_SECONDS="${SETTLE_SECONDS:-25}"

mkdir -p "$OUTDIR"
SHOT="$OUTDIR/$LABEL.ppm"
LOG="$OUTDIR/$LABEL.log"

echo "=============================================================="
echo " $LABEL"
if [ -r /etc/os-release ]; then . /etc/os-release; echo " host userspace : $PRETTY_NAME"; fi
echo " glibc          : $(ldd --version 2>/dev/null | head -1 | awk '{print $NF}')"
echo " app            : $*"
echo "=============================================================="

Xvfb "$DISPLAY_NUM" -screen 0 1400x900x24 >/dev/null 2>&1 &
XVFB_PID=$!
sleep 3
export DISPLAY="$DISPLAY_NUM"

export HOME="${HOME:-/root}"
mkdir -p "$HOME/.local/share" "$HOME/.config" "$HOME/.cache"

"$@" > "$LOG" 2>&1 &
APP_PID=$!

# Wait for a mapped top-level window rather than a fixed sleep.
WINDOW=""
for _ in $(seq 1 "$SETTLE_SECONDS"); do
  if xwininfo -root -tree 2>/dev/null | grep -qiE '"(Robot Whisperer|robot-whisperer)"'; then
    WINDOW=yes
    break
  fi
  kill -0 "$APP_PID" 2>/dev/null || break
  sleep 1
done

STATUS=0
if ! kill -0 "$APP_PID" 2>/dev/null; then
  echo "RESULT: FAIL — the process exited before showing a window"
  echo "--- last 30 log lines ---"; tail -30 "$LOG"
  STATUS=1
elif [ -z "$WINDOW" ]; then
  echo "RESULT: FAIL — process alive but no window appeared in ${SETTLE_SECONDS}s"
  echo "--- last 30 log lines ---"; tail -30 "$LOG"
  STATUS=1
else
  echo "window appeared:"
  xwininfo -root -tree 2>/dev/null | grep -iE '"(Robot Whisperer|robot-whisperer)"' | head -2 | sed 's/^/  /'
  # Give WebKit time to paint the first frame after mapping the window.
  sleep 8
  import -display "$DISPLAY_NUM" -window root "$SHOT" 2>/dev/null
  if [ ! -s "$SHOT" ]; then
    echo "RESULT: FAIL — could not capture a screenshot"
    STATUS=1
  else
    echo "screenshot: $SHOT"
    python3 "$(dirname "$0")/analyse-screenshot.py" "$SHOT" || STATUS=1
  fi
fi

# Surface WebKit/GTK complaints even on success; they predict trouble on real
# hardware even when the headless run looks fine.
if grep -qiE "cannot open display|failed to create|GLib-GObject|Gdk-ERROR|web process|EGL|GL error" "$LOG" 2>/dev/null; then
  echo "--- notable log lines ---"
  grep -iE "cannot open display|failed to create|GLib-GObject|Gdk-ERROR|web process|EGL|GL error" "$LOG" | head -8 | sed 's/^/  /'
fi

kill "$APP_PID" 2>/dev/null
kill "$XVFB_PID" 2>/dev/null
wait 2>/dev/null
exit $STATUS
