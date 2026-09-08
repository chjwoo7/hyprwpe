import QtQuick
import Quickshell
import Quickshell.Io

Item {
    id: root

    /// Path to the external wallpaper socket (defaults to hyprwpe socket in XDG_RUNTIME_DIR).
    property string socketPath: {
        var runtime = Quickshell.env("XDG_RUNTIME_DIR");
        if (runtime && runtime.length > 0) {
            return runtime + "/hyprwpe.sock";
        }
        return "/tmp/hyprwpe.sock";
    }

    /// Whether an external daemon currently owns the background layer.
    property bool externalActive: false

    Timer {
        id: pollTimer
        interval: 1000
        repeat: true
        running: true
        triggeredOnStart: true

        onTriggered: {
            if (root.socketPath.length === 0) {
                root.externalActive = false;
                return;
            }
            // Check file presence via FileUtils / FileView
            root.externalActive = FileUtils.exists(root.socketPath);
        }
    }
}
