# Throughput harness

Measures what each client costs while a bridge streams at it. The pieces:

- `cargo xtask load-bridge` — a synthetic Foxglove/rosbridge server, both ROS 1
  and ROS 2 dialects, four payload presets. See `xtask/src/load_bridge.rs`.
- `seed.py` — writes the same connection into both apps' SQLite workspaces with
  `auto_connect = 1`, so neither has to be clicked through a connection dialog.
  Both apps carry the same schema, so the setup path is identical.
- `drive.sh` — launches an app under Xvfb **with openbox**, runs UI steps, and
  screenshots.
- `measure.sh` — startup, RSS and CPU, summed over the process tree.

## Two things that make this work at all

**WebKit ignores an instantaneous click.** `xdotool click` does press and
release in the same instant and the Tauri UI never saw it, which is what made
the whole load comparison look impossible in the first pass. `mousedown`,
sleep 0.15, `mouseup` works. `drive.sh` has that as `tap`.

**A window manager is required.** Without one nothing sets input focus, and
both apps swallow synthetic input. `openbox` is enough. It adds a ~20px title
bar, so root coordinates are 20px below where a screenshot of the client area
would suggest.

## Driving the Tauri app to a live subscription

```
tap 214 124 2          # + on REQUESTS
tap 472 152 1.2        # the connection select
tap 430 230 1.2        # the seeded connection
tap 940 153            # target field
typ "/bench/chatter_0"
tap 1362 152 5         # Subscribe
```

Verified: `Active std_msgs/Float64` with `{"data": 0}` decoded, which also
proves the bridge's CDR encoding is right.
