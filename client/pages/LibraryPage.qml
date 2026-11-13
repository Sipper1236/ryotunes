pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import Quickshell.Io
import Ryoku.Ui.Singletons
import "../"
import "../components"
import "../lib/library.js" as LibraryData

// The library page, ported from ui/src/routes/library/+page.svelte. Account collections load in
// parallel on open (get_library / _albums / _artists) and fill the card tabs (Albums / Artists /
// Playlists); Songs, Local and Insights are the ported LibrarySongs / LocalMusic /
// ListeningInsights, loaded lazily the first time their tab is opened so an unopened tab costs
// nothing. A PageHero-less title row carries the count and the toolbar (create a playlist, import
// one from a file over the socket with a path); a segmented control switches the body.
Item {
    id: page

    property var playlists: []
    property var albums: []
    property var artists: []
    property var loadedCollections: ({})
    property var loadingCollections: ({})
    property int collectionEpoch: 0
    property bool lastSpotifySignedIn: false
    property bool lastYoutubeSignedIn: false
    property bool loading: true
    property string errorMsg: ""
    property string tab: "songs"
    property var opened: []
    property bool creating: false
    property string newName: ""


    // Spotify is selected but not signed in: show the sign-in empty state instead of loading a
    // library from the daemon.
    readonly property bool spotifyGate: Playback.provider === "spotify" && !(Playback.spotify && Playback.spotify.signedIn)
    // The sidebar deep-links a tab (Router.push("library", { tab })). Honour it on every navigation,
    // not just the first mount, so clicking a Library sub-item while already on the page switches
    // tabs. A plain library nav (no tab) leaves the current tab alone. Tab clicks are local state
    // (no route push), so they never round-trip through here.
    readonly property var params: Router.current ? Router.current.params : ({})
    onParamsChanged: {
        var t = (page.params && page.params.tab) ? page.params.tab : "";
        if (t && t !== page.tab)
            page.selectTab(t);
    }

    readonly property var tabs: [
        { k: "songs", l: "Songs" },
        { k: "albums", l: "Albums" },
        { k: "artists", l: "Artists" },
        { k: "playlists", l: "Playlists" },
        { k: "local", l: "Local" },
        { k: "insights", l: "Insights" }
    ]
    readonly property bool gridTab: page.tab === "albums" || page.tab === "artists" || page.tab === "playlists"
    readonly property var gridModel: {
        if (page.tab === "playlists") return page.playlists;
        if (page.tab === "albums") return page.albums;
        if (page.tab === "artists") return page.artists;
        return [];
    }

    // The count meta beside the title reflects the active tab's collection size where the page owns
    // it (Songs / Local / Insights load their own rows, so they carry no page-level count).
    function tabMeta() {
        if (page.tab === "playlists") return page.playlists.length + (page.playlists.length === 1 ? " playlist" : " playlists");
        if (page.tab === "albums") return page.albums.length + (page.albums.length === 1 ? " album" : " albums");
        if (page.tab === "artists") return page.artists.length + (page.artists.length === 1 ? " artist" : " artists");
        return "";
    }

    Component.onCompleted: {
        page.lastSpotifySignedIn = !!(Playback.spotify && Playback.spotify.signedIn);
        page.lastYoutubeSignedIn = !!(Playback.auth && Playback.auth.signedIn);
        var t = (Router.current && Router.current.params && Router.current.params.tab)
            ? Router.current.params.tab : "songs";
        page.selectTab(t);
        page.load();
    }

    // A provider switch swaps the whole library: reload (or fall to the sign-in card).
    Connections {
        target: Playback
        function onProviderChanged(): void {
            page.clearCollections();
            page.load();
        }
        function onSpotifyChanged(): void {
            var signedIn = !!(Playback.spotify && Playback.spotify.signedIn);
            if (signedIn === page.lastSpotifySignedIn)
                return;
            page.lastSpotifySignedIn = signedIn;
            page.clearCollections(true);
            page.load();
        }
        function onAuthChanged(): void {
            var signedIn = !!(Playback.auth && Playback.auth.signedIn);
            if (signedIn === page.lastYoutubeSignedIn)
                return;
            page.lastYoutubeSignedIn = signedIn;
            page.clearCollections(true);
            page.load();
        }
    }

    Connections {
        target: LibraryCache
        function onItemsChanged(): void {
            if (page.loadedCollections.playlists)
                page.playlists = LibraryCache.items;
        }
    }

    function selectTab(k) {
        page.tab = k;
        if (page.opened.indexOf(k) < 0)
            page.opened = page.opened.concat([k]);
        if (page.isCollectionTab(k)) {
            page.loading = !page.loadedCollections[k];
            page.loadCollection(k);
        } else {
            page.loading = false;
        }
    }
    function isOpened(k) { return page.opened.indexOf(k) >= 0; }
    function isCollectionTab(k) { return k === "playlists" || k === "albums" || k === "artists"; }

    function setCollectionMap(map, key, value) {
        var next = Object.assign({}, map);
        next[key] = value;
        return next;
    }

    function clearCollections(clearCache) {
        page.collectionEpoch++;
        if (clearCache)
            LibraryData.clearCollectionCache();
        page.playlists = [];
        page.albums = [];
        page.artists = [];
        page.loadedCollections = ({});
        page.loadingCollections = ({});
        page.loading = page.isCollectionTab(page.tab);
        page.errorMsg = "";
    }

    function load() {
        // Gated: no daemon call, the sign-in card carries the page.
        if (page.spotifyGate) {
            page.clearCollections();
            page.loading = false;
            return;
        }
        page.errorMsg = "";
        if (page.isCollectionTab(page.tab)) {
            page.loading = !page.loadedCollections[page.tab];
            page.loadCollection(page.tab);
        } else {
            page.loading = false;
        }
    }

    function loadCollection(tab) {
        if (!page.isCollectionTab(tab) || page.spotifyGate
                || page.loadedCollections[tab] || page.loadingCollections[tab])
            return;
        var provider = Playback.provider;
        if (tab !== "playlists") {
            var cached = LibraryData.cachedCollection(provider, tab);
            if (cached !== null) {
                if (tab === "albums") page.albums = cached;
                else page.artists = cached;
                page.loadedCollections = page.setCollectionMap(page.loadedCollections, tab, true);
                if (page.tab === tab)
                    page.loading = false;
                return;
            }
        }
        var method = tab === "playlists" ? "get_library"
            : tab === "albums" ? "get_library_albums" : "get_library_artists";
        var epoch = page.collectionEpoch;
        page.loadingCollections = page.setCollectionMap(page.loadingCollections, tab, true);
        if (page.tab === tab)
            page.loading = true;
        page.errorMsg = "";
        const request = method === "get_library" ? LibraryCache.load(false) : Daemon.call(method);
        request.then((items) => {
            if (epoch !== page.collectionEpoch)
                return;
            if (tab === "playlists") page.playlists = items || [];
            else if (tab === "albums") {
                page.albums = items || [];
                LibraryData.rememberCollection(provider, tab, page.albums);
            } else {
                page.artists = items || [];
                LibraryData.rememberCollection(provider, tab, page.artists);
            }
            page.loadedCollections = page.setCollectionMap(page.loadedCollections, tab, true);
            page.loadingCollections = page.setCollectionMap(page.loadingCollections, tab, false);
            if (page.tab === tab)
                page.loading = false;
        }).catch((e) => {
            if (epoch !== page.collectionEpoch)
                return;
            page.loadingCollections = page.setCollectionMap(page.loadingCollections, tab, false);
            if (page.tab === tab) {
                page.errorMsg = (e && e.message) ? e.message : String(e);
                page.loading = false;
            }
        });
    }

    function refreshPlaylists(force) {
        if (page.spotifyGate || (!force && !page.loadedCollections.playlists))
            return;
        var epoch = page.collectionEpoch;
        LibraryCache.load(true)
            .then((r) => {
                if (epoch !== page.collectionEpoch)
                    return;
                page.playlists = r || [];
                page.loadedCollections = page.setCollectionMap(page.loadedCollections, "playlists", true);
            })
            .catch(() => {});
    }

    function createPlaylist() {
        var title = page.newName.trim();
        if (!title || page.creating)
            return;
        page.creating = true;
        Daemon.call("create_playlist", { title: title })
            .then(() => { page.creating = false; page.newName = ""; newDialog.visible = false; page.refreshPlaylists(true); Playback.toast("Playlist created", "success"); })
            .catch((e) => { page.creating = false; Playback.toast((e && e.message) ? e.message : "Could not create", "error"); });
    }

    // import: zenity file picker -> import_playlist_file(path) -> create_playlist + add each
    Process {
        id: importPicker
        command: ["zenity", "--file-selection", "--title=Import a playlist file"]
        stdout: StdioCollector {
            id: importOut
            onStreamFinished: { var p = importOut.text.trim(); if (p) page.doImport(p); }
        }
    }
    function doImport(path) {
        Daemon.call("import_playlist_file", { path: path })
            .then((transfer) => {
                if (!transfer || !transfer.items || !transfer.items.length)
                    return;
                return Daemon.call("create_playlist", { title: transfer.title }).then((id) => {
                    var chain = Promise.resolve();
                    for (var i = 0; i < transfer.items.length; i++) {
                        (function (song) {
                            chain = chain.then(() => Daemon.call("add_to_playlist", { playlistId: id, videoId: song.video_id }).catch(() => {}));
                        })(transfer.items[i]);
                    }
                    return chain.then(() => { page.refreshPlaylists(true); Playback.toast("Imported " + transfer.title, "success"); });
                });
            })
            .catch((e) => Playback.toast((e && e.message) ? e.message : "Could not import", "error"));
    }

    ColumnLayout {
        anchors.fill: parent
        visible: !page.spotifyGate
        spacing: Style.sp(4)

        // title row: display title + active-tab count, then the collection toolbar
        RowLayout {
            Layout.fillWidth: true
            spacing: Style.sp(4)
            ColumnLayout {
                spacing: Style.sp(1)
                Text {
                    text: "Library"
                    color: Tokens.ink
                    font.family: Style.fontDisplay
                    font.pixelSize: Style.fs.title
                }
                Text {
                    visible: text !== ""
                    text: page.tabMeta()
                    color: Tokens.inkMuted
                    font.family: Style.fontUi
                    font.pixelSize: Style.fs.sm
                }
            }
            Item { Layout.fillWidth: true }
            Pill { label: "Import"; icon: "add"; onClicked: importPicker.running = true }
            Pill { label: "New playlist"; icon: "playlist"; primary: true; onClicked: { page.newName = ""; newDialog.visible = true; } }
        }

        // segmented control
        Flickable {
            Layout.fillWidth: true
            implicitHeight: tabRow.implicitHeight
            contentWidth: tabRow.implicitWidth
            contentHeight: tabRow.implicitHeight
            flickableDirection: Flickable.HorizontalFlick
            clip: true
            Row {
                id: tabRow
                spacing: Style.sp(2)
                Repeater {
                    model: page.tabs
                    delegate: Chip {
                        required property var modelData
                        text: modelData.l
                        active: page.tab === modelData.k
                        onClicked: page.selectTab(modelData.k)
                    }
                }
            }
        }

        Hairline { Layout.fillWidth: true }

        // body: card grids for the account collections, lazy loaders for the ported sub-surfaces
        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            CardGrid {
                anchors.fill: parent
                visible: page.gridTab
                loading: page.loading
                model: page.gridModel
                pad: 0
                emptyText: (Playback.auth && Playback.auth.signedIn) ? "Nothing saved yet." : "Sign in to see your library."
            }

            Loader {
                anchors.fill: parent
                active: page.isOpened("songs") && !page.spotifyGate
                visible: page.tab === "songs"
                sourceComponent: LibrarySongs {}
            }
            Loader {
                anchors.fill: parent
                active: page.isOpened("local") && !page.spotifyGate
                visible: page.tab === "local"
                sourceComponent: LocalMusic {}
            }
            Loader {
                anchors.fill: parent
                active: page.isOpened("insights") && !page.spotifyGate
                visible: page.tab === "insights"
                sourceComponent: ListeningInsights {}
            }
        }
    }

    // The Spotify sign-in empty state: shown when Spotify is selected but not signed in, in place of
    // the library. The button starts the OAuth flow; nothing here touches the daemon.
    SpotifyGate {
        anchors.centerIn: parent
        visible: page.spotifyGate
    }

    // new-playlist dialog
    Item {
        id: newDialog
        anchors.fill: parent
        visible: false
        z: 210
        MouseArea { anchors.fill: parent; onClicked: newDialog.visible = false }
        Rectangle { anchors.fill: parent; color: "#000000"; opacity: 0.5 }
        Rectangle {
            anchors.centerIn: parent
            width: Style.sp(90)
            implicitHeight: ndCol.implicitHeight + Style.sp(8)
            height: implicitHeight
            radius: Style.radiusCard
            color: Tokens.paperLift
            border.width: 1
            border.color: Tokens.lineStrong
            MouseArea { anchors.fill: parent }
            ColumnLayout {
                id: ndCol
                anchors.fill: parent
                anchors.margins: Style.sp(4)
                spacing: Style.sp(3)
                Text { text: "New playlist"; color: Tokens.ink; font.family: Style.fontUi; font.pixelSize: Style.fs.lg; font.weight: Font.DemiBold }
                Rectangle {
                    Layout.fillWidth: true
                    implicitHeight: Style.sp(10)
                    radius: Style.radius
                    color: Tokens.paper
                    border.width: 1
                    border.color: ndField.activeFocus ? Tokens.lineStrong : Tokens.line
                    TextInput {
                        id: ndField
                        anchors.fill: parent
                        anchors.leftMargin: Style.sp(2)
                        anchors.rightMargin: Style.sp(2)
                        verticalAlignment: TextInput.AlignVCenter
                        clip: true
                        color: Tokens.ink
                        font.family: Style.fontUi
                        font.pixelSize: Style.fs.md
                        text: page.newName
                        onTextChanged: page.newName = text
                        onAccepted: page.createPlaylist()
                        Component.onCompleted: if (newDialog.visible) ndField.forceActiveFocus()
                        Text {
                            anchors.verticalCenter: parent.verticalCenter
                            visible: ndField.text.length === 0
                            text: "Playlist name"
                            color: Tokens.inkFaint
                            font: ndField.font
                        }
                    }
                }
                RowLayout {
                    Layout.alignment: Qt.AlignRight
                    spacing: Style.sp(2)
                    Pill { label: "Cancel"; onClicked: newDialog.visible = false }
                    Pill { label: "Create"; primary: true; enabled: !page.creating && page.newName.trim().length > 0; onClicked: page.createPlaylist() }
                }
            }
        }
        onVisibleChanged: if (visible) ndField.forceActiveFocus()
    }
}
