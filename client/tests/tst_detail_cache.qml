import QtQuick
import QtTest
import "../lib/detail-cache.js" as DetailCache

TestCase {
    name: "BrowseDetailCache"

    function test_reuses_detail_responses_and_collapses_inflight_requests() {
        compare(DetailCache.TTL_MS, 120000);
        DetailCache.clear();
        var calls = 0;
        var fetch = function() {
            calls++;
            return Promise.resolve({ title: "Album" });
        };
        var first = DetailCache.get("get_album", { id: "spotify:album:one" }, fetch);
        var duplicate = DetailCache.get("get_album", { id: "spotify:album:one" }, fetch);
        verify(first === duplicate);
        return first.then(function(value) {
            compare(value.title, "Album");
            compare(calls, 1);
            return DetailCache.get("get_album", { id: "spotify:album:one" }, fetch);
        }).then(function(value) {
            compare(value.title, "Album");
            compare(calls, 1);
            compare(DetailCache.size(), 1);
            DetailCache.clear();
        });
    }

    function test_metadata_prefetch_is_scoped_to_free_spotify_detail_pages() {
        compare(DetailCache.metadataDetailMethod({ kind: "album", id: "spotify:album:one" }, "spotify", true), "get_album");
        compare(DetailCache.metadataDetailMethod({ kind: "artist", id: "spotify:artist:one" }, "spotify", true), "get_artist");
        compare(DetailCache.metadataDetailMethod({ kind: "playlist", id: "spotify:playlist:one" }, "spotify", true), "get_playlist");
        compare(DetailCache.metadataDetailMethod({ kind: "playlist", id: "spotify:playlist:liked" }, "spotify", true), "");
        compare(DetailCache.metadataDetailMethod({ kind: "song", id: "spotify:track:one" }, "spotify", true), "");
        compare(DetailCache.metadataDetailMethod({ kind: "album", id: "spotify:album:one" }, "spotify", false), "");
        compare(DetailCache.metadataDetailMethod({ kind: "album", id: "youtube:album:one" }, "youtube", true), "");
    }

    function test_metadata_prefetch_warms_the_page_cache_without_duplicate_fetches() {
        DetailCache.clear();
        var calls = 0;
        return DetailCache.prefetchMetadataDetail(
            { kind: "artist", id: "spotify:artist:one" }, "spotify", true,
            function(method, params) {
                calls++;
                compare(method, "get_artist");
                compare(params.id, "spotify:artist:one");
                return Promise.resolve({ name: "Prefetched" });
            }).then(function(value) {
                compare(value.name, "Prefetched");
                return DetailCache.get("get_artist", { id: "spotify:artist:one" }, function() {
                    calls++;
                    return Promise.resolve({ name: "Fetched again" });
                });
            }).then(function(value) {
                compare(value.name, "Prefetched");
                compare(calls, 1);
                DetailCache.clear();
            });
    }

    function test_metadata_playlist_prefetch_reuses_the_first_page_on_navigation() {
        DetailCache.clear();
        var calls = 0;
        var item = { kind: "playlist", id: "spotify:playlist:one" };
        return DetailCache.prefetchMetadataDetail(item, "spotify", true, function(method) {
            calls++;
            compare(method, "get_playlist");
            return Promise.resolve({ items: [{ title: "First page" }], continuation: "next" });
        }).then(function(value) {
            compare(value.items[0].title, "First page");
            return DetailCache.get("get_playlist", { id: item.id }, function() {
                calls++;
                return Promise.resolve({ items: [] });
            });
        }).then(function(value) {
            compare(value.items[0].title, "First page");
            compare(calls, 1);
            DetailCache.clear();
        });
    }

    function test_clear_invalidates_cached_details() {
        DetailCache.clear();
        var calls = 0;
        var fetch = function() {
            calls++;
            return Promise.resolve({ title: "Playlist" });
        };
        return DetailCache.get("get_playlist", { id: "spotify:playlist:one" }, fetch)
            .then(function() {
                DetailCache.clear();
                return DetailCache.get("get_playlist", { id: "spotify:playlist:one" }, fetch);
            }).then(function() {
                compare(calls, 2);
                DetailCache.clear();
            });
    }

    function test_old_inflight_reply_cannot_replace_or_hide_new_request() {
        DetailCache.clear();
        var finishOld;
        var old = DetailCache.get("get_artist", { id: "spotify:artist:one" }, function() {
            return new Promise(function(resolve) { finishOld = resolve; });
        });
        DetailCache.clear();
        var current = DetailCache.get("get_artist", { id: "spotify:artist:one" }, function() {
            return Promise.resolve({ name: "Current" });
        });
        finishOld({ name: "Old" });
        return Promise.all([old, current]).then(function(values) {
            compare(values[0].name, "Old");
            compare(values[1].name, "Current");
            return DetailCache.get("get_artist", { id: "spotify:artist:one" }, function() {
                fail("expected the current response to remain cached");
            });
        }).then(function(value) {
            compare(value.name, "Current");
            DetailCache.clear();
        });
    }
}
