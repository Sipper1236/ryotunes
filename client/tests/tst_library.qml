import QtQuick
import QtTest
import "../lib/library.js" as LibraryData

TestCase {
    name: "LibraryCache"

    function test_collection_cache_is_scoped_by_provider_and_tab() {
        LibraryData.clearCollectionCache();
        var albums = [{ id: "spotify:album:1" }];
        LibraryData.rememberCollection("spotify", "albums", albums, 1000);
        compare(LibraryData.cachedCollection("spotify", "albums", 1500), albums);
        compare(LibraryData.cachedCollection("youtube", "albums", 1500), null);
        compare(LibraryData.cachedCollection("spotify", "artists", 1500), null);
        LibraryData.clearCollectionCache();
    }

    function test_expired_collection_is_fetched_again() {
        LibraryData.clearCollectionCache();
        LibraryData.rememberCollection("spotify", "artists", [], 1000);
        compare(LibraryData.cachedCollection("spotify", "artists", 120999).length, 0);
        compare(LibraryData.cachedCollection("spotify", "artists", 121001), null);
        LibraryData.clearCollectionCache();
    }
}
