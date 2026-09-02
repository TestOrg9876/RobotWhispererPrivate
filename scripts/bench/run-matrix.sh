#!/usr/bin/env bash
# The throughput matrix. One app, one load, at a time — two apps on one X
# display corrupt each other's input.
set -u
OURS=/home/user/RobotWhispererPrivate/target/release/robot-whisperer
TAURI=/home/user/tauri-baseline/src-tauri/target/release/robot-whisperer
OUT=/home/user/bench/results.txt

# Subscribe recipes, mapped by screenshot and verified live.
steps_ours() {  # $1 = topic
  echo "tap 244 69 2; tap 700 116; typ \"$1\"; sleep 0.6; xdotool key Return; sleep 1; tap 1362 116 2; tap 1373 172 4; tap 1362 116 5"
}
steps_tauri() {
  echo "tap 214 124 2; tap 472 152 1.2; tap 430 230 1.2; tap 940 153; typ \"$1\"; tap 1362 152 5"
}

run() { # id proto dialect preset count hz topic
  local id=$1 proto=$2 dialect=$3 preset=$4 count=$5 hz=$6 topic=$7
  for app in ours tauri; do
    local bin steps name
    if [ "$app" = ours ]; then bin=$OURS; steps=$(steps_ours "$topic"); name="Robot";
    else bin=$TAURI; steps=$(steps_tauri "$topic"); name="Robot Whisperer"; fi
    PORT=$((9200 + RANDOM % 300)) SETTLE=12 WINDOW=15 \
      /home/user/bench/matrix.sh "$id-$app" "$bin" "$name" "$proto" "$dialect" \
      "$preset" "$count" "$hz" "$topic" "$steps" 2>/dev/null | tail -1 | tee -a "$OUT"
    sleep 2
  done
}

: > "$OUT"
run A foxglove ros2 chatter    1 1000 /bench/chatter_0
run B foxglove ros2 pointcloud 1 10   /bench/points_0
run C foxglove ros2 image1080c 1 60   /bench/image_c_0
run D foxglove ros2 image1080  1 30   /bench/image_0
run E foxglove ros1 pointcloud 1 10   /bench/points_0
run F rosbridge ros1 chatter   1 100  /bench/chatter_0
echo "--- done ---"; cat "$OUT"
