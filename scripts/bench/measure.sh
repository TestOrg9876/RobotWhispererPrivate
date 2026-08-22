#!/usr/bin/env bash
# Measures one app: cold start to mapped window, then RSS and CPU over a
# settle window.
#
# Every number is summed over the whole process tree. Tauri runs its UI in
# separate WebKitWebProcess/WebKitNetworkProcess children, so reading only the
# launched PID would credit it with none of its own rendering.
set -u

LABEL="$1"; shift
SETTLE="${SETTLE:-20}"
DISPLAY_NUM="${DISPLAY_NUM:-:98}"

tree_pids() {
  local root=$1
  local out=$root
  local queue=$root
  local next
  while [ -n "$queue" ]; do
    next=""
    for p in $queue; do
      for c in $(pgrep -P "$p" 2>/dev/null); do out="$out $c"; next="$next $c"; done
    done
    queue="$next"
  done
  echo "$out"
}

sum_rss_kb() { local t=0 v; for p in $(tree_pids "$1"); do v=$(awk '/^VmRSS:/{print $2}' /proc/$p/status 2>/dev/null); t=$((t + ${v:-0})); done; echo "$t"; }
sum_cpu_ticks() { local t=0 u s; for p in $(tree_pids "$1"); do read -r u s < <(awk '{print $14, $15}' /proc/$p/stat 2>/dev/null); t=$((t + ${u:-0} + ${s:-0})); done; echo "$t"; }

Xvfb "$DISPLAY_NUM" -screen 0 1440x900x24 >/dev/null 2>&1 &
XVFB=$!
sleep 2

START=$(date +%s.%N)
DISPLAY="$DISPLAY_NUM" "$@" >/tmp/$LABEL.stdout 2>/tmp/$LABEL.stderr &
APP=$!

# GPUI maps an *unnamed* window, so `xdotool search --name` never sees it.
# The project's own harness reads the root's children and picks the one with a
# real size; the same test finds the Tauri window too.
find_win() {
  DISPLAY="$DISPLAY_NUM" xwininfo -root -children 2>/dev/null \
    | awk 'match($0, /0x[0-9a-f]+/) {
        id = substr($0, RSTART, RLENGTH)
      }
      match($0, /[0-9]+x[0-9]+\+/) {
        split(substr($0, RSTART, RLENGTH - 1), wh, "x")
        if (wh[1] >= 200 && wh[2] >= 200) { print id; exit }
      }'
}

MAPPED=""
for _ in $(seq 1 600); do
  WIN=$(find_win)
  if [ -n "$WIN" ]; then MAPPED=$(date +%s.%N); break; fi
  sleep 0.05
done

if [ -z "$MAPPED" ]; then echo "$LABEL: no window appeared"; kill $APP $XVFB 2>/dev/null; exit 1; fi
STARTUP=$(echo "$MAPPED - $START" | bc)

# Both windows to the same size before anything is measured. The Tauri app
# opens at 800x600 and ours at 1440x900; per-pixel costs are most of what is
# being compared, so leaving that alone would hand Tauri a 2.7x head start.
DISPLAY="$DISPLAY_NUM" xdotool windowsize "$WIN" 1440 900 2>/dev/null
sleep 2
GEOM=$(DISPLAY="$DISPLAY_NUM" xdotool getwindowgeometry "$WIN" 2>/dev/null | tr '\n' ' ')

# Optional UI steps before measuring, so a state past the welcome screen can
# be compared as well as the welcome screen itself.
if [ -n "${STEPS:-}" ]; then
  bash -c "DISPLAY=$DISPLAY_NUM WIN=$WIN; $STEPS"
fi

sleep "$SETTLE"
RSS=$(sum_rss_kb $APP)
C1=$(sum_cpu_ticks $APP); sleep 10; C2=$(sum_cpu_ticks $APP)
HZ=$(getconf CLK_TCK)
CPU=$(echo "scale=2; ($C2 - $C1) * 100 / $HZ / 10" | bc)
PROCS=$(tree_pids $APP | wc -w)

echo "$LABEL startup_s=$STARTUP rss_kb=$RSS cpu_pct=$CPU procs=$PROCS geom=[$GEOM]"
kill $APP 2>/dev/null; sleep 1; kill -9 $APP 2>/dev/null
pkill -f "$DISPLAY_NUM" 2>/dev/null; kill $XVFB 2>/dev/null
wait 2>/dev/null
