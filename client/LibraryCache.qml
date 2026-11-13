pragma Singleton
import QtQuick
import Quickshell
import Ryoku.Ui.Singletons
import "lib/library.js" as LibraryData
import "lib/liked-cache.js" as LikedCache
import "lib/detail-cache.js" as DetailCache

// Shared playlist collection for the sidebar, Library page, and Add to playlist picker. Those
// surfaces are open at different times, so retaining the last response avoids repeating the same
// remote library request each time one is opened.
Singleton {
    id: root

    property var items: []
    property bool loaded: false
    property bool loading: false
    property bool refreshPending: false
    property int requestId: 0
    property var inFlight: null

    function load(force) {
        if (root.loading) {
            if (force)
                root.refreshPending = true;
            return root.inFlight || Promise.resolve(root.items);
        }
        if (root.loaded && !force)
            return Promise.resolve(root.items);

        root.loading = true;
        const request = ++root.requestId;
        function finish() {
            if (request !== root.requestId)
                return;
            root.loading = false;
            root.inFlight = null;
            if (root.refreshPending) {
                root.refreshPending = false;
                root.load(true).catch(() => {});
            }
        }

        const pending = Daemon.call("get_library").then((result) => {
            if (request === root.requestId) {
                root.items = result || [];
                root.loaded = true;
            }
            finish();
            return root.items;
        }).catch((error) => {
            if (request === root.requestId)
                root.loaded = false;
            finish();
            throw error;
        });
        root.inFlight = pending;
        return pending;
    }

    Component.onCompleted: if (Daemon.connected) root.load(false).catch(() => {})

    Connections {
        target: Daemon
        function onSnapshot(snap): void { root.load(root.loaded).catch(() => {}); }
        function onEvent(name: string, data: var): void {
            if (name === "liked-songs-changed" || name === "spotify-auth")
                LikedCache.clear();
            if (name === "liked-songs-changed" || name === "spotify-auth" || name === "library-changed")
                DetailCache.clear();
            if (name === "library-changed") {
                LibraryData.clearCollectionCache();
                root.load(true).catch(() => {});
            }
        }
    }

    Connections {
        target: Playback
        function onProviderChanged(): void {
            DetailCache.clear();
            root.load(true).catch(() => {});
        }
        function onSpotifyChanged(): void {
            LibraryData.clearCollectionCache();
            LikedCache.clear();
            DetailCache.clear();
            if (Playback.provider === "spotify") root.load(true).catch(() => {});
        }
        function onAuthChanged(): void {
            LibraryData.clearCollectionCache();
            DetailCache.clear();
            if (Playback.provider === "youtube") root.load(true).catch(() => {});
        }
    }
}
