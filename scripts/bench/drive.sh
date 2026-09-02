#!/usr/bin/env bash
# Launch an app under Xvfb+openbox, run steps, capture. Steps use `tap X Y`
# and `typ TEXT`, which do press-hold-release: WebKit ignores an instantaneous
# xdotool click, which is what made the Tauri UI look undriveable.
set -u
OUT="$1"; APPNAME="$2"; shift 2
STEPS="${STEPS:-}"
# A fresh display each run, and its lock removed first: a stale
# /tmp/.XNN-lock with no server behind it makes Xvfb refuse to start, and the
# app then dies with no display at all.
NUM="${DNUM:-$((RANDOM % 60 + 130))}"
D=":$NUM"
rm -f "/tmp/.X$NUM-lock" "/tmp/.X11-unix/X$NUM" 2>/dev/null
Xvfb $D -screen 0 1440x900x24 >/dev/null 2>&1 & XV=$!
sleep 2
DISPLAY=$D openbox >/dev/null 2>&1 & WM=$!
sleep 2
DISPLAY=$D "$@" >/tmp/drive.out 2>/tmp/drive.err & APP=$!
export DISPLAY=$D
W=""
for _ in $(seq 1 400); do
  W=$(xdotool search --name "$APPNAME" 2>/dev/null | tail -1)
  [ -n "$W" ] && break
  # GPUI maps an unnamed window; fall back to the largest child.
  W=$(xwininfo -root -children 2>/dev/null | awk 'match($0,/0x[0-9a-f]+/){id=substr($0,RSTART,RLENGTH)} match($0,/[0-9]+x[0-9]+\+/){split(substr($0,RSTART,RLENGTH-1),wh,"x"); if(wh[1]>=400&&wh[2]>=300){print id;exit}}')
  [ -n "$W" ] && break
  sleep 0.1
done
[ -z "$W" ] && { echo "no window"; kill -9 $APP $WM $XV 2>/dev/null; exit 1; }
xdotool windowsize "$W" 1440 900 2>/dev/null
xdotool windowactivate "$W" 2>/dev/null
sleep 3
tap() { xdotool mousemove "$1" "$2"; sleep 0.4; xdotool mousedown 1; sleep 0.15; xdotool mouseup 1; sleep "${3:-0.8}"; }
typ() { xdotool type --delay 40 "$1"; sleep 0.5; }
export -f tap typ 2>/dev/null || true
[ -n "$STEPS" ] && eval "$STEPS"
sleep 1
import -window root "$OUT" 2>/dev/null
echo "PID=$APP WIN=$W DISPLAY=$D"
if [ "${KEEP:-0}" = "1" ]; then
  echo "$APP $WM $XV" > /tmp/drive.pids
else
  kill -9 $APP $WM $XV 2>/dev/null
  sleep 0.5
  rm -f "/tmp/.X$NUM-lock" "/tmp/.X11-unix/X$NUM" 2>/dev/null
fi
