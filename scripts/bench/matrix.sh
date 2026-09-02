#!/usr/bin/env bash
# One row of the throughput matrix: start a bridge, start an app already
# seeded to auto-connect, drive it to a subscription, then measure the whole
# process tree while the load runs.
set -u
LABEL="$1"; APP="$2"; APPNAME="$3"; PROTO="$4"; DIALECT="$5"; PRESET="$6"; COUNT="$7"; HZ="$8"; TOPIC="$9"; STEPS="${10}"
PORT="${PORT:-9101}"
SETTLE="${SETTLE:-15}"
WINDOW="${WINDOW:-15}"

# Release build: a debug build of the generator delivered 58 Hz of an offered
# 1000, which would have made every number a measurement of the harness.
XTASK="/home/user/RobotWhispererPrivate/target/release/xtask"

"$XTASK" load-bridge --protocol "$PROTO" --dialect "$DIALECT" --port "$PORT" \
  --preset "$PRESET" --count "$COUNT" --hz "$HZ" > /tmp/bridge-$LABEL.log 2>&1 &
BRIDGE=$!
sleep 3

python3 /home/user/bench/seed.py "$( [ "$PROTO" = foxglove ] && echo foxglove_ws || echo rosbridge )" "ws://127.0.0.1:$PORT" >/dev/null

NUM=$((RANDOM % 60 + 130)); D=":$NUM"
rm -f "/tmp/.X$NUM-lock" "/tmp/.X11-unix/X$NUM" 2>/dev/null
Xvfb $D -screen 0 1440x900x24 >/dev/null 2>&1 & XV=$!
sleep 2
DISPLAY=$D openbox >/dev/null 2>&1 & WM=$!
sleep 2
DISPLAY=$D "$APP" >/tmp/app-$LABEL.out 2>/tmp/app-$LABEL.err & PID=$!
export DISPLAY=$D
W=""
for _ in $(seq 1 400); do
  W=$(xdotool search --name "$APPNAME" 2>/dev/null | tail -1)
  [ -z "$W" ] && W=$(xwininfo -root -children 2>/dev/null | awk 'match($0,/0x[0-9a-f]+/){id=substr($0,RSTART,RLENGTH)} match($0,/[0-9]+x[0-9]+\+/){split(substr($0,RSTART,RLENGTH-1),wh,"x"); if(wh[1]>=400&&wh[2]>=300){print id;exit}}')
  [ -n "$W" ] && break; sleep 0.1
done
xdotool windowsize "$W" 1440 900 2>/dev/null; xdotool windowactivate "$W" 2>/dev/null
sleep 4
tap() { xdotool mousemove "$1" "$2"; sleep 0.4; xdotool mousedown 1; sleep 0.15; xdotool mouseup 1; sleep "${3:-0.8}"; }
typ() { xdotool type --delay 40 "$1"; sleep 0.5; }
eval "$STEPS"

sleep "$SETTLE"
tree_pids() { local r=$1; local o=$r; local q=$r; local n; while [ -n "$q" ]; do n=""; for p in $q; do for c in $(pgrep -P "$p" 2>/dev/null); do o="$o $c"; n="$n $c"; done; done; q="$n"; done; echo "$o"; }
rss() { local t=0 v; for p in $(tree_pids "$1"); do v=$(awk '/^VmRSS:/{print $2}' /proc/$p/status 2>/dev/null); t=$((t+${v:-0})); done; echo $t; }
cpu() { local t=0 u s; for p in $(tree_pids "$1"); do read -r u s < <(awk '{print $14,$15}' /proc/$p/stat 2>/dev/null); t=$((t+${u:-0}+${s:-0})); done; echo $t; }

B1=$(grep -c . /dev/null); C1=$(cpu $PID); R1=$(rss $PID)
sleep "$WINDOW"
C2=$(cpu $PID); R2=$(rss $PID)
HZC=$(getconf CLK_TCK)
CPUPCT=$(echo "scale=1; ($C2-$C1)*100/$HZC/$WINDOW" | bc)
RSSMB=$(echo "scale=1; $R2/1024" | bc)
PROCS=$(tree_pids $PID | wc -w)

DISPLAY=$D import -window root "/home/user/bench/shot-$LABEL.png" 2>/dev/null
# What the bridge actually wrote during the measurement window, so a run that
# was bridge-bound cannot be mistaken for a client that kept up.
DELIV=$(grep -o "delivered [0-9]* msg/s, [0-9.]* MiB/s" /tmp/bridge-$LABEL.log | tail -5 \
        | awk '{m+=$2; b+=$4; n++} END {if(n) printf "%.0f msg/s %.1f MiB/s", m/n, b/n; else print "none"}')
echo "$LABEL cpu_pct=$CPUPCT rss_mb=$RSSMB procs=$PROCS delivered=[$DELIV]"
kill -9 $PID $WM $XV $BRIDGE 2>/dev/null
sleep 1; rm -f "/tmp/.X$NUM-lock" "/tmp/.X11-unix/X$NUM" 2>/dev/null
