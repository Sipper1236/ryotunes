import QtQuick
import QtTest
import "../components"
import "../lib/home.js" as HomeData

TestCase {
    name: "HomeFeed"
    when: windowShown
    width: 600
    visible: true
    height: 500

    Component {
        id: feedComponent
        HomeFeed {
            id: feed
            width: 600
            height: 500
            property int headerHeight: 1600
            model: 20
            header: Rectangle { width: 600; height: feed.headerHeight; color: "gray" }
            delegate: Rectangle { width: 600; height: 200; color: "white" }
        }
    }

    function makeFeed() {
        var feed = createTemporaryObject(feedComponent, this);
        verify(feed !== null);
        waitForRendering(feed);
        tryCompare(feed.headerItem, "height", 1600);
        return feed;
    }

    function test_first_wheel_does_not_skip_tall_header() {
        var feed = makeFeed();
        var top = feed.contentY;
        mouseWheel(feed, 300, 200, 0, -120);
        tryVerify(function () { return feed.contentY > top; }, 1000, "Wheel must actually scroll");
        tryVerify(function () { return !feed.moving; });
        verify(feed.contentY - top < feed.height, "One wheel tick must not skip the whole Home header");
    }

    function test_releasing_pin_preserves_position() {
        var feed = makeFeed();
        var top = feed.contentY;
        feed.stickTop = false;
        compare(feed.contentY, top, "Disabling the pin must not restore pre-layout contentY=0");
    }

    function test_async_header_growth_stays_at_top_until_interaction() {
        var feed = makeFeed();
        feed.headerHeight = 2100;
        tryCompare(feed.headerItem, "height", 2100);
        tryCompare(feed, "contentY", feed.originY);
        feed.stickTop = false;
        feed.contentY += 120;
        var offset = feed.contentY;
        wait(50);
        compare(feed.contentY, offset, "The top pin must not reclaim a user-owned scroll position");
    }

    function test_only_tidal_provider_transitions_replace_home_feed() {
        verify(!HomeData.providerChangeAffectsHome("youtube", "spotify"));
        verify(!HomeData.providerChangeAffectsHome("spotify", "soundcloud"));
        verify(!HomeData.providerChangeAffectsHome("spotify", "spotify"));
        verify(HomeData.providerChangeAffectsHome("youtube", "tidal"));
        verify(HomeData.providerChangeAffectsHome("tidal", "spotify"));
    }

    function test_home_cache_invalidation_tracks_personal_account_events() {
        verify(HomeData.authEventInvalidatesHome("auth-changed", {}));
        verify(HomeData.authEventInvalidatesHome("spotify-auth", { state: "signed_in" }));
        verify(HomeData.authEventInvalidatesHome("spotify-auth", { state: "signed_out" }));
        verify(HomeData.authEventInvalidatesHome("soundcloud-auth", { state: "signed_in" }));
        verify(HomeData.authEventInvalidatesHome("soundcloud-auth", { state: "signed_out" }));
        verify(!HomeData.authEventInvalidatesHome("spotify-auth", { state: "url" }));
        verify(!HomeData.authEventInvalidatesHome("soundcloud-auth", { state: "waiting" }));
        verify(!HomeData.authEventInvalidatesHome("spotify-auth", { state: "error" }));
        verify(!HomeData.authEventInvalidatesHome("provider-changed", {}));
        verify(!HomeData.authEventInvalidatesHome("now-playing", {}));
    }

    function test_home_cache_is_shared_across_merged_providers_but_separate_for_tidal() {
        HomeData.clearFeedCache();
        var feed = { sections: [{ title: "Cached" }] };
        HomeData.rememberFeed("youtube", "", feed, 1000);
        compare(HomeData.cachedFeed("spotify", ""), feed);
        compare(HomeData.cachedFeed("soundcloud", ""), feed);
        compare(HomeData.cachedFeed("tidal", ""), null);
        HomeData.rememberFeed("youtube", "rock", { sections: [{ title: "Rock" }] });
        compare(HomeData.cachedFeed("youtube", "").sections[0].title, "Cached");
        compare(HomeData.cachedFeed("youtube", "rock").sections[0].title, "Rock");
        HomeData.clearFeedCache();
    }

    function test_fresh_home_cache_skips_revalidation_until_expiry() {
        HomeData.clearFeedCache();
        HomeData.rememberFeed("youtube", "", { sections: [] }, 1000);
        verify(HomeData.feedIsFresh("spotify", "", 60999));
        verify(!HomeData.feedIsFresh("soundcloud", "", 61000));
        HomeData.clearFeedCache();
        verify(!HomeData.feedIsFresh("youtube", "", 1000));
    }

    function test_persistent_home_snapshot_restores_stale_feed_for_fast_first_paint() {
        HomeData.clearFeedCache();
        var feed = { chips: [], sections: [{ title: "Saved Home" }], continuation: "old-token" };
        var saved = HomeData.persistentSnapshot(feed, 1000);
        verify(saved.length > 0);
        verify(HomeData.restorePersistentSnapshot(saved, 72000));
        compare(HomeData.cachedFeed("spotify", "").sections[0].title, "Saved Home");
        compare(HomeData.cachedFeed("youtube", "").continuation, null);
        verify(!HomeData.feedIsFresh("youtube", "", 72000), "stale feed should still trigger refresh");
        HomeData.clearFeedCache();
    }

    function test_persistent_home_snapshot_rejects_invalid_and_expired_cache() {
        HomeData.clearFeedCache();
        verify(!HomeData.restorePersistentSnapshot("not json", 1000));
        var saved = HomeData.persistentSnapshot({ sections: [] }, 1000);
        verify(!HomeData.restorePersistentSnapshot(saved, 86401001));
        compare(HomeData.cachedFeed("youtube", ""), null);
        HomeData.clearFeedCache();
    }
}
