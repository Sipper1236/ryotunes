pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Layouts
import QtQuick.Shapes
import Ryoku.Ui.Singletons
import "../"
import "../components"
import "../chrome"
import "../lib/browse.js" as Browse
import "../lib/detail-cache.js" as DetailCache
import "../lib/home.js" as HomeData
import "../lib/ids.js" as Ids
import "../lib/recommendations.js" as Recommendations

// Home (spec section 5), ported from ui/src/routes/+page.svelte and reset onto the visual system.
// One vertical reused ListView of the feed's shelves; the header carries the greeting hero (with
// the listening deck at its right on a wide page), the mood-chip rail, the Pinned tile grid and the
// personal shelves — Listen again, Familiar artists and Forgotten favourites. The footer carries the
// loading skeletons, the empty / error states and the progressive get_home_more pagination
// (it fires when the tail comes within 400 px of the viewport bottom). The page fills the content
// rect the frame already pads (32 sides / 24 top / 32 bottom); it never insets itself.
Item {
    id: page

    property var home: null
    property var chips: []
    property var forgotten: null
    property var listenAgain: null
    // The hero's search field, for the rig's `suggest` hook.
    property var heroSearch: null
    property string selected: ""
    property bool loading: true
    property string errorMsg: ""
    property bool loadingMore: false
    property bool moreError: false
    ListModel { id: blocks }
    property int requestId: 0
    property var recommendations: ({ items: [], explanation: "" })

    // Home is the merged feed: YouTube Music's own shelves (always available, signed in or
    // not) plus the shelves of every signed-in provider. There is no sign-in gate here —
    // Library and Search still gate on the selected catalogue.

    // Personal shelves, live off the shared store.
    readonly property var recents: Personal.recent(100).filter((item) => page.providerFor(item.id) === Playback.provider).slice(0, 6)

    // TIDAL has no guest catalogue, so a signed-out TIDAL reaches this page with an empty feed
    // rather than an error. Without this the empty state offers Google's sign-in, which signs the
    // user into the wrong service entirely and leaves the selected provider exactly as dead.
    readonly property bool tidalGate: Playback.provider === "tidal"
        && !(Playback.tidal && Playback.tidal.signedIn)
    readonly property var forgottenList: page.forgottenSongs()

    // The shelf displays at most five artists; resolve only those pages.
    property var famIds: Personal.topArtistIds(5)
    property var famArtists: []
    property Item familiarShelfItem: null
    property bool famLoaded: false
    property string homeProvider: "youtube"
    property bool providerSnapshotSeen: false
    onFamIdsChanged: page.maybeLoadFamiliar()

    Component.onCompleted: {
        page.homeProvider = Playback.provider;
        HomeStore.restore();
        page.load("");
    }

    // Only TIDAL replaces the merged Home feed; the other selected providers share it.
    Connections {
        target: Daemon
        function onSnapshot(snap): void {
            if (!snap || !snap.provider)
                return;
            var previous = page.homeProvider;
            page.homeProvider = snap.provider;
            if (!page.providerSnapshotSeen) {
                page.providerSnapshotSeen = true;
                // The first request already uses the daemon's selected provider. Retry here only
                // if it failed before the socket connected and the page has no result yet.
                if (!page.home && !page.loading)
                    page.load(page.selected);
                return;
            }
            if (HomeData.providerChangeAffectsHome(previous, snap.provider))
                page.load(page.selected);
        }
        function onEvent(name: string, data: var): void {
            if (HomeData.authEventInvalidatesHome(name, data)) {
                HomeData.clearFeedCache();
                HomeStore.clear();
                if (page.homeProvider !== "tidal")
                    page.load(page.selected);
            }
            if (name !== "provider-changed" || !data || !data.provider)
                return;
            var previous = page.homeProvider;
            page.homeProvider = data.provider;
            if (HomeData.providerChangeAffectsHome(previous, data.provider))
                page.load(page.selected);
        }
    }

    Connections {
        target: Personal
        function onBlobChanged(): void { Qt.callLater(page.refreshRecommendations); }
    }

    function providerFor(id) {
        return Ids.providerOf(id);
    }
    function refreshRecommendations() {
        page.recommendations = Recommendations.build(
            page.home && page.home.sections ? page.home.sections : [],
            Personal.blob, Playback.provider, Date.now());
    }

    function greeting() {
        var h = new Date().getHours();
        return h < 5 ? "Still up" : h < 12 ? "Good morning" : h < 17 ? "Good afternoon" : h < 22 ? "Good evening" : "Good night";
    }

    function isForgotten(s) {
        if (!/forgotten/i.test(s.title))
            return false;
        for (var i = 0; i < s.items.length; i++)
            if (s.items[i].kind === "song")
                return true;
        return false;
    }

    function appendSections(secs) {
        // Keep the model alive across continuation responses. Replacing a JS-array model
        // resets ListView to the header, even after the user has started scrolling.
        for (var i = 0; i < secs.length; i++) {
            if (page.isForgotten(secs[i])) {
                if (!page.forgotten)
                    page.forgotten = secs[i];
            } else if (!page.listenAgain && /listen again/i.test(secs[i].title)) {
                page.listenAgain = secs[i];
            } else {
                blocks.append({ sectionData: secs[i] });
            }
        }
        page.refreshRecommendations();
    }

    function showHome(h) {
        blocks.clear();
        page.forgotten = null;
        page.listenAgain = null;
        page.home = h;
        page.chips = (h.chips || []).filter((c) => c.title !== "Podcasts");
        page.appendSections(h.sections || []);
    }

    function forgottenSongs() {
        if (!page.forgotten)
            return [];
        return page.forgotten.items.filter((i) => i.kind === "song").slice(0, 15);
    }

    function loadFamiliar() {
        if (page.famLoaded || page.famIds.length < 3)
            return;
        page.famLoaded = true;
        Promise.all(page.famIds.map((id) => DetailCache.get("get_artist", { id: id },
            () => Daemon.call("get_artist", { id: id })).catch(() => null)))
            .then((pages) => { page.famArtists = pages.filter((p) => !!p); });
    }

    // Artist pages are a secondary Home shelf. Fetch them when the shelf is visible or within
    // roughly one viewport of it, after the main feed request has finished.
    function maybeLoadFamiliar() {
        if (page.famLoaded || page.loading || page.famIds.length < 3 || page.selected !== "")
            return;
        Qt.callLater(() => {
            if (page.famLoaded || page.loading || page.selected !== "" || !page.familiarShelfItem
                    || !page.familiarShelfItem.visible || list.height <= 0)
                return;
            var shelfY = page.familiarShelfItem.mapToItem(list.contentItem, 0, 0).y;
            if (shelfY <= list.contentY + list.height * 1.25)
                page.loadFamiliar();
        });
    }

    function load(params) {
        page.selected = params;
        var request = ++page.requestId;
        var cacheProvider = Playback.provider;
        var cached = HomeData.cachedFeed(cacheProvider, params);
        page.loadingMore = false;
        list.cancelFlick();
        list.stickTop = true;
        page.moreError = false;
        page.errorMsg = "";
        if (cached) {
            page.showHome(cached);
            page.loading = false;
            if (HomeData.feedIsFresh(cacheProvider, params)) {
                page.maybeLoadFamiliar();
                return;
            }
        } else {
            blocks.clear();
            page.forgotten = null;
            page.listenAgain = null;
            page.loading = true;
        }
        Daemon.call("get_home", { params: params ? params : null })
            .then((h) => {
                if (page.requestId !== request)
                    return;
                HomeData.rememberFeed(cacheProvider, params, h);
                if (cacheProvider !== "tidal" && !params)
                    HomeStore.save(h);
                page.showHome(h);
                page.loading = false;
                page.maybeLoadFamiliar();
            })
            .catch((e) => {
                if (page.requestId !== request)
                    return;
                page.errorMsg = (e && e.message) ? e.message : String(e);
                page.loading = false;
                page.maybeLoadFamiliar();
            });
    }

    function loadMore() {
        if (!page.home || !page.home.continuation || page.loadingMore || page.moreError)
            return;
        page.loadingMore = true;
        var token = page.home.continuation;
        var request = page.requestId;
        Daemon.call("get_home_more", { token: token })
            .then((more) => {
                if (page.requestId !== request || !page.home || page.home.continuation !== token)
                    return;
                page.home = {
                    chips: page.home.chips,
                    sections: page.home.sections.concat(more.sections),
                    continuation: more.sections.length ? more.continuation : undefined
                };
                page.appendSections(more.sections);
                page.loadingMore = false;
            })
            .catch(() => {
                if (page.requestId !== request)
                    return;
                page.moreError = true;
                page.loadingMore = false;
                Playback.toast("Could not load more", "error");
            });
    }

    function maybeLoadMore() {
        if (page.loading || !page.home || !page.home.continuation || page.loadingMore || page.moreError)
            return;
        if (list.contentHeight <= 0)
            return;
        if (!list.stickTop && list.contentY - list.originY + list.height > list.contentHeight - 400)
            page.loadMore();
    }

    HomeFeed {
        id: list
        anchors.fill: parent
        clip: true
        reuseItems: true
        cacheBuffer: Math.max(0, Math.round(height * 1.5))
        boundsBehavior: Flickable.StopAtBounds
        model: blocks
        spacing: Style.sp(10)

        // HomeFeed releases its top pin without restoring a stale pre-layout offset.
        onContentYChanged: { page.maybeLoadMore(); page.maybeLoadFamiliar(); }
        onContentHeightChanged: { page.maybeLoadMore(); page.maybeLoadFamiliar(); }

        delegate: Item {
            required property var sectionData
            width: list.width
            implicitHeight: shelf.implicitHeight
            Shelf {
                id: shelf
                width: parent.width
                section: parent.sectionData
                mark: Style.decorRich ? "章" : ""
            }
        }

        header: Item {
            // The hero's search panel hangs below its own row: keep the header above the delegates
            // (the feed shelves) so the panel is never painted under them.
            z: 2
            width: list.width
            // The list's spacing runs between delegates only; the header carries its own section
            // gap so the first shelf never rides up against the last header block.
            implicitHeight: headerCol.implicitHeight + Style.sp(10)

            ColumnLayout {
                id: headerCol
                width: parent.width
                spacing: Style.sp(10)

                // hero: the greeting, search and key hints on the left (inset 48 px); the Now
                // Playing card on the right when the page is wide enough that it never crowds the
                // greeting column (>= 1180 px) and something is playing.
                RowLayout {
                    id: hero
                    z: 2
                    Layout.fillWidth: true
                    readonly property bool wide: width >= Style.sp(295)
                    spacing: 0

                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.maximumWidth: Style.sp(120)
                        Layout.minimumWidth: Style.sp(70)
                        Layout.leftMargin: Style.sp(4)
                        Layout.alignment: Qt.AlignTop
                        spacing: Style.sp(2)

                        // — 力 HOME / LISTEN ··· 01
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: Style.sp(2)
                            Rectangle { Layout.preferredWidth: Style.sp(4); Layout.preferredHeight: 1; Layout.alignment: Qt.AlignVCenter; color: Tokens.ink }
                            Text { text: "力"; color: Tokens.ink; font.family: Tokens.jp; font.pixelSize: Style.fs.sm }
                            Text {
                                text: "HOME / LISTEN"
                                color: Tokens.inkFaint
                                font.family: Style.fontMono
                                font.pixelSize: Style.fs.micro
                                font.letterSpacing: Style.trackMicro
                            }
                            Rectangle { Layout.fillWidth: true; Layout.preferredHeight: 1; Layout.alignment: Qt.AlignVCenter; color: Tokens.lineSoft }
                            Text {
                                text: "01"
                                color: Tokens.inkFaint
                                font.family: Style.fontMono
                                font.pixelSize: Style.fs.micro
                                font.letterSpacing: Style.trackMicro
                            }
                        }
                        Text {
                            Layout.fillWidth: true
                            text: page.greeting() + ((Playback.auth && Playback.auth.signedIn && Playback.auth.name) ? (", " + Playback.auth.name) : "")
                            color: Tokens.ink
                            font.family: Style.fontDisplay
                            font.pixelSize: Style.fs.hero
                            elide: Text.ElideRight
                        }
                        Text {
                            Layout.fillWidth: true
                            text: Playback.provider === "soundcloud" && !(Playback.soundcloud && Playback.soundcloud.signedIn)
                                ? "SoundCloud \u00b7 listening as a guest"
                                : "Pick up where you left off, or find the next thing worth hearing."
                            color: Tokens.inkMuted
                            font.family: Style.fontUi
                            font.pixelSize: Style.fs.sm
                            wrapMode: Text.WordWrap
                        }

                        // the search field, with a Ctrl+K kbd chip inset at its right edge
                        Item {
                            Layout.fillWidth: true
                            Layout.topMargin: Style.sp(2)
                            implicitHeight: heroSearch.implicitHeight
                            z: 40
                            SearchSuggest {
                                id: heroSearch
                                Component.onCompleted: page.heroSearch = heroSearch
                                width: parent.width
                                placeholder: "Search tracks, albums, artists…"
                                onSubmitted: if (value.trim() !== "") Router.push("search", { q: value.trim() })
                                onPicked: if (value.trim() !== "") Router.push("search", { q: value.trim() })
                                z: 40
                            }
                            Rectangle {
                                id: kbd
                                z: 50
                                visible: heroSearch.value === ""
                                anchors.right: parent.right
                                anchors.rightMargin: Style.sp(2)
                                anchors.top: parent.top
                                anchors.topMargin: (Style.sp(11) - height) / 2
                                implicitHeight: Style.sp(6)
                                implicitWidth: kbdText.implicitWidth + Style.sp(3)
                                radius: Style.radius
                                color: Tokens.tint5
                                border.width: 1
                                border.color: Tokens.line
                                Text {
                                    id: kbdText
                                    anchors.centerIn: parent
                                    text: "Ctrl+K"
                                    color: Tokens.inkMuted
                                    font.family: Style.fontMono
                                    font.pixelSize: Style.fs.micro
                                    font.letterSpacing: Style.trackMicro
                                }
                            }
                        }

                        // key hints
                        RowLayout {
                            Layout.topMargin: Style.sp(1)
                            spacing: Style.sp(2)
                            Text { text: "Ctrl+K"; color: Tokens.inkMuted; font.family: Style.fontMono; font.pixelSize: Style.fs.micro; font.letterSpacing: Style.trackMicro }
                            Text { text: "command search"; color: Tokens.inkFaint; font.family: Style.fontUi; font.pixelSize: Style.fs.xs }
                            Rectangle { Layout.preferredWidth: Style.sp(4); Layout.preferredHeight: 1; Layout.alignment: Qt.AlignVCenter; color: Tokens.lineSoft }
                            Text { text: "Space"; color: Tokens.inkMuted; font.family: Style.fontMono; font.pixelSize: Style.fs.micro; font.letterSpacing: Style.trackMicro }
                            Text { text: "play / pause"; color: Tokens.inkFaint; font.family: Style.fontUi; font.pixelSize: Style.fs.xs }
                        }
                    }

                    Item { Layout.fillWidth: true; Layout.minimumWidth: hero.wide ? Style.sp(10) : 0 }
                    NowPlayingCard {
                        visible: hero.wide && !!Playback.now
                        Layout.preferredWidth: Style.sp(138)
                        Layout.maximumWidth: Style.sp(140)
                        Layout.alignment: Qt.AlignTop
                        onOpenQueue: Playback.nowPlayingRequested("queue")
                    }
                }

                // The restored listening session must remain reachable on a narrow Home too.
                RowLayout {
                    Layout.fillWidth: true
                    visible: !!Playback.now
                    spacing: Style.sp(3)
                    Artwork {
                        visible: !hero.wide
                        url: Playback.now && Playback.now.thumbnail ? Playback.now.thumbnail : ""
                        px: Style.sp(14)
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Style.sp(1)
                        Text {
                            Layout.fillWidth: true
                            text: Playback.paused ? "Continue listening" : "Your listening session"
                            color: Tokens.ink
                            font.family: Style.fontUi
                            font.pixelSize: Style.fs.md
                            font.weight: Font.Medium
                        }
                        Text {
                            Layout.fillWidth: true
                            text: Playback.now ? Playback.now.title + " — " + Playback.now.artists : ""
                            color: Tokens.inkMuted
                            font.family: Style.fontUi
                            font.pixelSize: Style.fs.sm
                            elide: Text.ElideRight
                            textFormat: Text.PlainText
                        }
                    }
                    Btn {
                        text: Playback.paused ? "Resume" : "Pause"
                        icon: Playback.paused ? "play" : "pause"
                        primary: Playback.paused
                        onClicked: Playback.togglePause().catch((e) => Playback.toast(String(e), "error"))
                    }
                    IconButton {
                        icon: "queue"
                        tip: "Open your queue"
                        onClicked: Playback.nowPlayingRequested("queue")
                    }
                }

                HomePersonal {
                    id: familiarShelf
                    Component.onCompleted: page.familiarShelfItem = familiarShelf
                    Layout.fillWidth: true
                    visible: page.selected === ""
                    recents: page.recents
                }

                // chip rail — a horizontal Flickable whose right edge fades into the paper (never a
                // hard cut on the last chip) via a gradient overlay rather than an OpacityMask.
                Item {
                    Layout.fillWidth: true
                    implicitHeight: chipRow.implicitHeight
                    visible: page.chips.length > 0
                    Flickable {
                        id: chipsFlick
                        anchors.fill: parent
                        contentWidth: chipRow.implicitWidth
                        contentHeight: chipRow.implicitHeight
                        flickableDirection: Flickable.HorizontalFlick
                        boundsBehavior: Flickable.StopAtBounds
                        clip: true
                        Row {
                            id: chipRow
                            spacing: Style.sp(2)
                            Chip {
                                text: "All"
                                active: page.selected === ""
                                onClicked: page.load("")
                            }
                            Repeater {
                                model: page.chips
                                delegate: Chip {
                                    required property var modelData
                                    text: modelData.title
                                    active: page.selected === modelData.params
                                    onClicked: page.load(page.selected === modelData.params ? "" : modelData.params)
                                }
                            }
                        }
                    }
                    Rectangle {
                        visible: chipsFlick.contentWidth > chipsFlick.width
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.bottom: parent.bottom
                        width: Style.sp(12)
                        gradient: Gradient {
                            orientation: Gradient.Horizontal
                            GradientStop { position: 0.0; color: "transparent" }
                            GradientStop { position: 1.0; color: Tokens.paper }
                        }
                    }
                }

                // shortcuts (pinned, unfiltered only) — bordered 56 px cards, then a dashed
                // "Add shortcut" card of the same height
                ColumnLayout {
                    Layout.fillWidth: true
                    visible: page.selected === ""
                    spacing: Style.sp(4)
                    RowLayout {
                        Layout.fillWidth: true
                        spacing: Style.sp(2)
                        Icon { Layout.alignment: Qt.AlignVCenter; name: "dashboard"; size: Style.fs.md; color: Tokens.inkMuted }
                        SectionHeading { Layout.fillWidth: true; title: "// Shortcuts"; mark: Style.decorRich ? "選" : "" }
                    }
                    Flow {
                        id: pinFlow
                        Layout.fillWidth: true
                        spacing: Style.sp(4)

                        Repeater {
                            model: Personal.picks
                            delegate: Rectangle {
                                id: pin
                                required property var modelData
                                readonly property bool round: pin.modelData && pin.modelData.kind === "artist"
                                width: Style.sp(80)
                                height: Style.sp(14)
                                radius: Style.radiusCard
                                color: pinHover.hovered ? Tokens.tint5 : "transparent"
                                border.width: 1
                                border.color: pinHover.hovered ? Tokens.lineStrong : Tokens.line
                                Behavior on color { ColorAnimation { duration: Style.motion.snap } }
                                Behavior on border.color { ColorAnimation { duration: Style.motion.snap } }
                                RowLayout {
                                    anchors.fill: parent
                                    anchors.leftMargin: Style.sp(2)
                                    anchors.rightMargin: Style.sp(2)
                                    spacing: Style.sp(3)
                                    Artwork {
                                        Layout.alignment: Qt.AlignVCenter
                                        url: pin.modelData && pin.modelData.thumbnail ? pin.modelData.thumbnail : ""
                                        px: Style.sp(10)
                                        round: pin.round
                                        placeholderIcon: pin.round ? "user"
                                            : (pin.modelData && Ids.isOnRepeatId(pin.modelData.id)) ? "on-repeat" : "music"
                                    }
                                    ColumnLayout {
                                        Layout.fillWidth: true
                                        spacing: Style.sp(0.5)
                                        Text {
                                            Layout.fillWidth: true
                                            text: pin.modelData ? pin.modelData.title : ""
                                            color: Tokens.ink
                                            font.family: Style.fontUi
                                            font.pixelSize: Style.fs.md
                                            font.weight: Font.Medium
                                            elide: Text.ElideRight
                                        }
                                        Text {
                                            Layout.fillWidth: true
                                            text: (pin.modelData && pin.modelData.subtitle) ? pin.modelData.subtitle
                                                : (pin.modelData ? pin.modelData.kind : "")
                                            color: Tokens.inkMuted
                                            font.family: Style.fontUi
                                            font.pixelSize: Style.fs.sm
                                            elide: Text.ElideRight
                                            textFormat: Text.PlainText
                                        }
                                    }
                                    IconButton {
                                        visible: pinHover.hovered
                                        icon: "close"
                                        iconSize: Style.fs.sm
                                        diameter: Style.sp(7)
                                        tip: "Remove"
                                        onClicked: Personal.removePick(pin.modelData.id)
                                    }
                                }
                                HoverHandler { id: pinHover }
                                TapHandler {
                                    onTapped: {
                                        if (pin.modelData.kind === "song")
                                            Playback.play(Browse.asSong(pin.modelData));
                                        else
                                            Router.push(pin.modelData.kind, { id: pin.modelData.id, title: pin.modelData.title });
                                    }
                                    acceptedButtons: Qt.LeftButton
                                }
                            }
                        }

                        // dashed "Add shortcut" card
                        Item {
                            id: addTile
                            width: Style.sp(80)
                            height: Style.sp(14)
                            Shape {
                                anchors.fill: parent
                                preferredRendererType: Shape.CurveRenderer
                                ShapePath {
                                    strokeColor: addHover.hovered ? Tokens.lineStrong : Tokens.line
                                    strokeWidth: 1
                                    fillColor: addHover.hovered ? Tokens.tint5 : "transparent"
                                    strokeStyle: ShapePath.DashLine
                                    dashPattern: [4, 4]
                                    joinStyle: ShapePath.RoundJoin
                                    PathSvg {
                                        path: {
                                            var o = 0.5;
                                            var w = addTile.width - o * 2;
                                            var h = addTile.height - o * 2;
                                            var r = Style.radiusCard;
                                            return "M " + (o + r) + " " + o
                                                + " H " + (o + w - r)
                                                + " A " + r + " " + r + " 0 0 1 " + (o + w) + " " + (o + r)
                                                + " V " + (o + h - r)
                                                + " A " + r + " " + r + " 0 0 1 " + (o + w - r) + " " + (o + h)
                                                + " H " + (o + r)
                                                + " A " + r + " " + r + " 0 0 1 " + o + " " + (o + h - r)
                                                + " V " + (o + r)
                                                + " A " + r + " " + r + " 0 0 1 " + (o + r) + " " + o
                                                + " Z";
                                        }
                                    }
                                }
                            }
                            RowLayout {
                                anchors.centerIn: parent
                                spacing: Style.sp(2)
                                Icon { name: "add"; size: Style.fs.md; color: Tokens.inkMuted }
                                ColumnLayout {
                                    spacing: Style.sp(0.5)
                                    Text { text: "Add shortcut"; color: Tokens.inkMuted; font.family: Style.fontUi; font.pixelSize: Style.fs.md; font.weight: Font.Medium }
                                    Text { text: "Recent, library or YouTube link"; color: Tokens.inkFaint; font.family: Style.fontUi; font.pixelSize: Style.fs.xs }
                                }
                            }
                            HoverHandler { id: addHover }
                            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: shortcutPicker.open() }
                        }
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    visible: page.selected === "" && page.recommendations.items.length > 0
                    spacing: Style.sp(2)
                    Shelf {
                        Layout.fillWidth: true
                        title: "Picked for you"
                        items: page.recommendations.items
                    }
                    Text {
                        Layout.fillWidth: true
                        text: page.recommendations.explanation
                        color: Tokens.inkMuted
                        font.family: Style.fontUi
                        font.pixelSize: Style.fs.sm
                        wrapMode: Text.WordWrap
                    }
                }

                HomePersonal {
                    Layout.fillWidth: true
                    // The recents / familiar artists are YouTube Music history. The merged
                    // home always carries the YouTube spine, so they always belong; a mood
                    // chip filter narrows the YouTube feed alone, so they step aside then.
                    visible: page.selected === ""
                    artists: page.famArtists
                    listenAgain: page.listenAgain
                }

                // forgotten favourites (from the feed)
                Shelf {
                    Layout.fillWidth: true
                    visible: !!page.forgotten && page.forgottenList.length > 0
                    title: page.forgotten ? page.forgotten.title : "Forgotten favourites"
                    mark: Style.decorRich ? "忘" : ""
                    items: page.forgottenList
                }
            }
        }

        footer: Item {
            width: list.width
            implicitHeight: footerCol.implicitHeight + Style.sp(20)

            ColumnLayout {
                id: footerCol
                width: parent.width
                spacing: Style.sp(9)

                // loading skeletons
                Repeater {
                    model: page.loading ? 3 : 0
                    delegate: ColumnLayout {
                        Layout.fillWidth: true
                        spacing: Style.sp(3)
                        Skeleton { Layout.preferredWidth: Style.sp(40); Layout.preferredHeight: Style.sp(4) }
                        RowLayout {
                            Layout.fillWidth: true
                            spacing: Style.sp(4)
                            Repeater {
                                model: 6
                                delegate: Skeleton {
                                    required property int index
                                    Layout.preferredWidth: Style.cardW
                                    Layout.preferredHeight: Style.cardW
                                    corner: Style.radiusCard
                                }
                            }
                        }
                    }
                }

                // error
                ColumnLayout {
                    Layout.alignment: Qt.AlignHCenter
                    visible: !page.loading && page.errorMsg !== ""
                    spacing: Style.sp(2)
                    Text {
                        Layout.alignment: Qt.AlignHCenter
                        text: page.errorMsg
                        color: Tokens.inkMuted
                        font.family: Style.fontUi
                        font.pixelSize: Style.fs.md
                    }
                    Chip {
                        Layout.alignment: Qt.AlignHCenter
                        text: "Try again"
                        onClicked: page.load(page.selected)
                    }
                }

                // empty
                ColumnLayout {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.topMargin: Style.sp(16)
                    visible: !page.loading && page.errorMsg === "" && blocks.count === 0 && !page.forgotten
                    spacing: Style.sp(3)
                    Icon { Layout.alignment: Qt.AlignHCenter; name: "music"; size: Style.fs.hero; color: Tokens.inkFaint }
                    Text {
                        Layout.alignment: Qt.AlignHCenter
                        Layout.maximumWidth: Style.sp(90)
                        horizontalAlignment: Text.AlignHCenter
                        wrapMode: Text.WordWrap
                        text: page.tidalGate
                            ? "Sign in to TIDAL and home fills up with your playlists, albums and tracks."
                            : (Playback.auth && Playback.auth.signedIn)
                            ? "Your home feed came back empty this time."
                            : "Sign in and home fills up with mixes and playlists built from what you listen to."
                        color: Tokens.inkMuted
                        font.family: Style.fontUi
                        font.pixelSize: Style.fs.md
                    }
                    Chip {
                        Layout.alignment: Qt.AlignHCenter
                        text: page.tidalGate ? "Sign in to TIDAL"
                            : (Playback.auth && Playback.auth.signedIn) ? "Try again" : "Sign in with Google"
                        active: page.tidalGate || !(Playback.auth && Playback.auth.signedIn)
                        onClicked: page.tidalGate
                            ? Playback.tidalSignIn().catch(() => {})
                            : (Playback.auth && Playback.auth.signedIn)
                            ? page.load(page.selected)
                            : Daemon.call("sign_in").catch(() => {})
                    }
                }

                // load-more affordance
                Chip {
                    Layout.alignment: Qt.AlignHCenter
                    visible: !page.loading && !!(page.home && page.home.continuation) && page.moreError
                    text: page.loadingMore ? "Loading…" : "Try again"
                    onClicked: { page.moreError = false; page.loadMore(); }
                }
            }
        }
    }

    ShortcutPicker {
        id: shortcutPicker
        anchors.fill: parent
        z: 100
    }
}
