import QtQuick
import QtTest
import "../lib/browse.js" as Browse

TestCase {
    name: "Browse"

    function test_mixed_search_supplies_songs_without_a_separate_page() {
        var rows = Browse.searchSongRows([], [
            { id: "spotify:track:one", title: "One", isVideo: false },
            { id: "spotify:track:video", title: "Video", isVideo: true }
        ]);
        compare(rows.length, 1);
        compare(rows[0].video_id, "spotify:track:one");
        compare(rows[0].title, "One");
    }

    function test_dedicated_page_wins_when_available() {
        var rows = Browse.searchSongRows(
            [{ video_id: "page-track", title: "Paged", is_video: false }],
            [{ id: "spotify:track:mixed", title: "Mixed" }]
        );
        compare(rows.length, 1);
        compare(rows[0].video_id, "page-track");
    }

    function test_search_rows_filter_videos_for_both_shapes() {
        var paged = Browse.searchSongRows([
            { video_id: "song", is_video: false },
            { video_id: "video", is_video: true }
        ], []);
        var mixed = Browse.searchSongRows([], [
            { id: "song", title: "Song", isVideo: false },
            { id: "video", title: "Video", isVideo: true }
        ]);
        compare(paged.length, 1);
        compare(mixed.length, 1);
    }

    function test_metadata_providers_use_complete_mixed_search() {
        verify(Browse.hasCompleteMixedSearch("spotify"));
        verify(Browse.hasCompleteMixedSearch("soundcloud"));
        verify(Browse.hasCompleteMixedSearch("tidal"));
        verify(!Browse.hasCompleteMixedSearch("youtube"));
    }

    function test_soundcloud_song_cards_cover_search_row_fields() {
        var rows = Browse.searchSongRows([], [{
            id: "sc:track:42", title: "Song", subtitle: "Artist", duration: "3:20",
            playCount: "12K", thumbnail: "cover.jpg", artistRuns: [{ text: "Artist", id: "sc:user:7" }]
        }]);
        compare(rows.length, 1);
        compare(rows[0].video_id, "sc:track:42");
        compare(rows[0].artists, "Artist");
        compare(rows[0].artist_id, "sc:user:7");
        compare(rows[0].duration, "3:20");
        compare(rows[0].play_count, "12K");
        compare(rows[0].thumbnail, "cover.jpg");
    }

    function test_mixed_song_album_fields_survive_song_mapping() {
        var song = Browse.asSong({
            id: "tidal:track:1", title: "Song", subtitle: "Artist",
            album: "Album", albumId: "tidal:album:2"
        });
        compare(song.album, "Album");
        compare(song.album_id, "tidal:album:2");
    }

    function test_expanded_categories_reuse_mixed_search_results() {
        var results = {
            songs: [{ id: "sc:track:1", title: "Song", isVideo: false }],
            albums: [{ id: "sc:playlist:2", title: "Album" }],
            artists: [{ id: "sc:user:3", title: "Artist" }],
            playlists: [{ id: "sc:playlist:4", title: "Playlist" }]
        };
        compare(Browse.expandedMixedSearchRows(results, "songs")[0].video_id, "sc:track:1");
        compare(Browse.expandedMixedSearchRows(results, "albums")[0].title, "Album");
        compare(Browse.expandedMixedSearchRows(results, "artists")[0].title, "Artist");
        compare(Browse.expandedMixedSearchRows(results, "playlists")[0].title, "Playlist");
    }

    function test_song_expansion_keeps_richer_paged_metadata_when_present() {
        var results = { songs: [{ id: "tidal:track:1", title: "Song" }] };
        var pageItems = [{ video_id: "tidal:track:1", title: "Song", album: "Album" }];
        var rows = Browse.expandedMixedSearchRows(results, "songs", pageItems);
        compare(rows[0].album, "Album");
    }
}
