pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io
import "lib/home.js" as HomeData

// Persist only the most recent Home response so the opening page can render immediately after a
// relaunch. HomeData bounds its age and size; the ordinary network request still revalidates it.
Singleton {
    id: root

    property bool restored: false

    function restore() {
        if (root.restored)
            return;
        root.restored = true;
        if (!HomeData.restorePersistentSnapshot(adapter.snapshot)) {
            if (adapter.snapshot !== "") {
                adapter.snapshot = "";
                file.writeAdapter();
            }
        }
    }

    function save(feed) {
        var snapshot = HomeData.persistentSnapshot(feed);
        if (!snapshot)
            return;
        adapter.snapshot = snapshot;
        file.writeAdapter();
    }

    function clear() {
        adapter.snapshot = "";
        file.writeAdapter();
    }

    FileView {
        id: file
        path: (Quickshell.env("XDG_CONFIG_HOME") || (Quickshell.env("HOME") + "/.config"))
              + "/ryotunes/home-cache.json"
        blockLoading: true
        printErrors: false
        atomicWrites: true
        JsonAdapter {
            id: adapter
            property string snapshot: ""
        }
    }
}
