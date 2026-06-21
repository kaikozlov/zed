#!/bin/bash
# Phase V measurement runbook — typometer on three builds.
#
# Prerequisites:
#   - typometer jar at /tmp/typometer-bin/typometer-1.1.0/typometer-1.1.0.jar
#   - Java 8+ (java -version)
#   - This branch's Zed built: target/release/zed
#   - VS Code installed at /Applications/Visual Studio Code.app
#
# typometer settings (set in the GUI before each run):
#   Count:    200        (characters per run)
#   Delay:    150 ms     (interval between keystrokes)
#   Mode:     Synchronous (most accurate)
#   Pause:    disabled
#
# Run order: default Zed → legacy Zed → VS Code. 3 runs each for variance.
# Between runs: save CSV (File > Export), note the title, close the measurement.
#
# Measurement protocol per run:
#   1. Launch the target editor (this script does it for you).
#   2. Open a scratch file, position cursor top-left, make font large (24pt+).
#   3. Switch to typometer, configure, click Start.
#   4. Within ~5s, Cmd-Tab back to the editor and click in the text area.
#   5. Do NOT touch the machine until the run finishes (~30s).
#   6. Record: median, mean, min, max, and the frequency distribution shape.

set -euo pipefail

ZED_BIN="/Users/kai/.cargo/target/release/zed"
TYPOMETER_JAR="/tmp/typometer-bin/typometer-1.1.0/typometer-1.1.0.jar"
VSCODE_APP="/Applications/Visual Studio Code.app"

usage() {
    cat <<EOF
Usage: $0 <target>
  target:
    zed-default   — this branch's CA/IOSurface path (default)
    zed-legacy    — this branch with ZED_MACOS_LEGACY_METAL_LAYER=1
    vscode        — VS Code (Chromium parity target)
    typometer     — launch typometer GUI only
EOF
    exit 1
}

launch_typometer() {
    echo "Launching typometer..."
    java -jar "$TYPOMETER_JAR" &
    echo "typometer is running. Configure it, click Start, then Cmd-Tab to your editor."
}

case "${1:-}" in
    zed-default)
        echo "Launching Zed (default CA/IOSurface path)..."
        "$ZED_BIN" "$@" 2>/dev/null &
        ;;
    zed-legacy)
        echo "Launching Zed (legacy CAMetalLayer path)..."
        ZED_MACOS_LEGACY_METAL_LAYER=1 "$ZED_BIN" 2>/dev/null &
        ;;
    vscode)
        echo "Launching VS Code..."
        open -a "$VSCODE_APP" 2>/dev/null || { echo "VS Code not found at $VSCODE_APP"; exit 1; }
        ;;
    typometer)
        launch_typometer
        ;;
    *)
        echo "Building typometer config..."
        if [ ! -f "$TYPOMETER_JAR" ]; then
            echo "ERROR: typometer jar not found at $TYPOMETER_JAR"
            echo "  Download: https://github.com/frarees/typometer/releases/download/v1.1.0/typometer-1.1.0-bin.zip"
            exit 1
        fi
        if [ ! -f "$ZED_BIN" ]; then
            echo "ERROR: Zed binary not found at $ZED_BIN"
            echo "  Build with: cargo build --release --bin zed"
            exit 1
        fi
        echo ""
        echo "Everything ready. Run sequence:"
        echo "  1. $0 zed-default   (then run typometer)"
        echo "  2. $0 zed-legacy    (then run typometer)"
        echo "  3. $0 vscode        (then run typometer)"
        echo ""
        echo "Each typometer run: ~30s. Do 3 runs per target for variance."
        echo "Record results in research.md under 'Phase V measurements'."
        ;;
esac

# After launching the target, also launch typometer if it was a target run
if [ "${1:-}" != "typometer" ] && [ "${1:-}" != "" ]; then
    echo "Waiting 3s for editor to launch, then starting typometer..."
    sleep 3
    launch_typometer
fi

echo ""
echo ">>> NOW: switch to the editor, open a scratch file, click in the text area."
echo ">>> Then switch to typometer, set Synchronous mode, click Start."
echo ">>> Cmd-Tab back to the editor within 5 seconds."
echo ">>> When done, export CSV and record stats."
wait
