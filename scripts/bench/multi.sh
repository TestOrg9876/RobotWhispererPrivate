#!/usr/bin/env bash
# Many topics at once. Uses the `chatter` preset, whose encoding is verified
# end to end in both clients, so a failure here is the client and not the
# harness.
set -u
APP="$1"; NAME="$2"; N="${3:-5}"; HZ="${4:-200}"; LABEL="$5"
PORT=$((9600 + RANDOM % 200))
XTASK=/home/user/RobotWhispererPrivate/target/release/xtask
$XTASK load-bridge --protocol foxglove --dialect ros2 --port $PORT \
  --preset chatter --count $N --hz $HZ > /tmp/bridge-$LABEL.log 2>&1 & BR=$!
sleep 3
python3 /home/user/bench/seed.py foxglove_ws "ws://127.0.0.1:$PORT" >/dev/null

STEPS=""
for i in $(seq 0 $((N-1))); do
  if [ "$NAME" = "Robot" ]; then
    if [ "$i" = "0" ]; then
      STEPS="$STEPS tap 244 69 2; tap 700 116; typ \"/bench/chatter_$i\"; sleep 0.5; xdotool key Return; sleep 0.8; tap 1362 116 1.5; tap 1373 172 3; tap 1362 116 2;"
    else
      STEPS="$STEPS tap 244 69 2; tap 700 116; typ \"/bench/chatter_$i\"; sleep 0.5; xdotool key Return; sleep 0.8; tap 1362 116 2;"
    fi
  else
    STEPS="$STEPS tap 214 124 1.5; tap 472 152 1; tap 430 230 1; tap 940 153; typ \"/bench/chatter_$i\"; tap 1362 152 2;"
  fi
done
PORT=$PORT SETTLE=10 WINDOW=15 /home/user/bench/matrix.sh "$LABEL" "$APP" "$NAME" \
  foxglove ros2 chatter "$N" "$HZ" "/bench/chatter_0" "$STEPS" 2>/dev/null | tail -1
kill -9 $BR 2>/dev/null
