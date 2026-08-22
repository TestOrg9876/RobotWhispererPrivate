#!/bin/bash
# Run an assembled snap tree the way snapd would, without needing snapd.
#
# snapd executes a confined app inside a mount namespace where `/` is the base
# snap (core22) and the snap's own payload is at $SNAP, with the `layout:`
# stanzas bind-mounted into place. This reproduces the parts that decide
# whether the app can actually run: library resolution and the webkit2gtk-4.1
# layout bind. It does not reproduce AppArmor confinement.
#
#   run-snap-tree.sh <prime-dir> [command...]
set -eu
PRIME="${1:?usage: run-snap-tree.sh <prime-dir> [command...]}"
shift

ARCH_TRIPLET="x86_64-linux-gnu"

# The layout stanza from snapcraft.yaml. WebKitGTK looks for WebKitWebProcess
# at this absolute path; without the bind it silently fails to start a web
# process and the window stays blank.
if [ -d "$PRIME/usr/lib/$ARCH_TRIPLET/webkit2gtk-4.1" ]; then
  mkdir -p "/usr/lib/$ARCH_TRIPLET/webkit2gtk-4.1"
  mountpoint -q "/usr/lib/$ARCH_TRIPLET/webkit2gtk-4.1" || \
    mount --bind "$PRIME/usr/lib/$ARCH_TRIPLET/webkit2gtk-4.1" "/usr/lib/$ARCH_TRIPLET/webkit2gtk-4.1"
fi

export SNAP="$PRIME"
export LD_LIBRARY_PATH="$PRIME/usr/lib/$ARCH_TRIPLET:$PRIME/usr/lib:${LD_LIBRARY_PATH:-}"
export XDG_DATA_DIRS="$PRIME/usr/share:${XDG_DATA_DIRS:-/usr/share}"
export GDK_PIXBUF_MODULE_FILE="$PRIME/usr/lib/$ARCH_TRIPLET/gdk-pixbuf-2.0/2.10.0/loaders.cache"
export GIO_MODULE_DIR="$PRIME/usr/lib/$ARCH_TRIPLET/gio/modules"

if [ "$#" -eq 0 ]; then
  set -- "$PRIME/usr/bin/robot-whisperer"
fi
exec "$PRIME/bin/launcher" "$@"
