import QtQuick
import QtTest
import "../lib/liked-cache.js" as LikedCache

TestCase {
    name: "LikedSongsCache"

    function test_pages_are_cached_by_sort_and_continuation_key() {
        LikedCache.clear();
        var first = { items: [{ video_id: "spotify:one" }] };
        var more = { items: [{ video_id: "spotify:two" }] };
        var key = LikedCache.firstPageKey("default", false);
        LikedCache.put(key, first, 1000);
        LikedCache.put("spotify-liked-token-page-1", more, 1000);
        compare(LikedCache.get(key, 1500), first);
        compare(LikedCache.get("spotify-liked-token-page-1", 1500), more);
        compare(LikedCache.get(LikedCache.firstPageKey("newest", false), 1500), null);
        LikedCache.clear();
    }

    function test_expired_pages_are_refetched() {
        LikedCache.clear();
        var key = LikedCache.firstPageKey("default", false);
        LikedCache.put(key, { items: [] }, 1000);
        compare(LikedCache.get(key, 120999).items.length, 0);
        compare(LikedCache.get(key, 121001), null);
        LikedCache.clear();
    }

    function test_first_page_prefetch_shares_inflight_request_and_warms_navigation() {
        LikedCache.clear();
        var calls = 0;
        var fetch = function() {
            calls++;
            return Promise.resolve({ items: [{ title: "Liked track" }] });
        };
        var first = LikedCache.loadFirstPage(fetch);
        var duplicate = LikedCache.loadFirstPage(fetch);
        verify(first === duplicate);
        return Promise.all([first, duplicate]).then(function(values) {
            compare(values[0].items[0].title, "Liked track");
            compare(values[1].items[0].title, "Liked track");
            return LikedCache.loadFirstPage(function() {
                calls++;
                return Promise.resolve({ items: [] });
            });
        }).then(function(value) {
            compare(value.items[0].title, "Liked track");
            compare(calls, 1);
            LikedCache.clear();
        });
    }

    function test_clear_prevents_old_request_from_repopulating_cache() {
        LikedCache.clear();
        var finishOld;
        var old = LikedCache.loadFirstPage(function() {
            return new Promise(function(resolve) { finishOld = resolve; });
        });
        LikedCache.clear();
        var current = LikedCache.loadFirstPage(function() {
            return Promise.resolve({ items: [{ title: "Current account" }] });
        });
        finishOld({ items: [{ title: "Old account" }] });
        return Promise.all([old, current]).then(function() {
            compare(LikedCache.get(LikedCache.firstPageKey("default", false)).items[0].title, "Current account");
            LikedCache.clear();
        });
    }
}
