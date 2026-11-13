import QtQuick
import QtTest
import "../lib/search.js" as Search

TestCase {
    name: "SearchCache"

    function test_results_are_shared_for_normalized_query_within_provider() {
        Search.clearResultCache();
        var results = { songs: [{ id: "spotify:track:one" }] };
        Search.rememberResults("spotify", " Daft   Punk ", results, 1000);
        compare(Search.cachedResults("spotify", "daft punk", 1500), results);
        compare(Search.cachedResults("soundcloud", "daft punk", 1500), null);
        Search.clearResultCache();
    }

    function test_expired_results_are_not_reused() {
        Search.clearResultCache();
        Search.rememberResults("youtube", "radiohead", { songs: [] }, 1000);
        compare(Search.cachedResults("youtube", "radiohead", 60999).songs.length, 0);
        compare(Search.cachedResults("youtube", "radiohead", 61001), null);
        Search.clearResultCache();
    }

    function test_same_inflight_query_is_requested_once_across_surfaces() {
        Search.clearResultCache();
        var calls = 0;
        var result = { songs: [{ id: "spotify:track:one" }] };
        var fetch = function() {
            calls++;
            return Promise.resolve(result);
        };
        var first = Search.loadResults("spotify", "  Daft   Punk ", fetch);
        var second = Search.loadResults("spotify", "daft punk", fetch);
        verify(first === second);
        return Promise.all([first, second]).then(function(values) {
            compare(calls, 1);
            compare(values[0], result);
            compare(Search.cachedResults("spotify", "Daft Punk"), result);
            Search.clearResultCache();
        });
    }
}
