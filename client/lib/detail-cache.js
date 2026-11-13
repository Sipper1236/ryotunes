.pragma library

// Browse detail pages are recreated on navigation. Keep their first responses briefly so opening
// the same album, artist, or playlist again is instant, and collapse concurrent requests for the
// same detail into one daemon call.
// Browse details change infrequently, and mutations/auth/provider changes clear this cache.
// A longer window makes revisiting a page instant even after a user pauses to listen or browse.
var TTL_MS = 120000;
var MAX_ENTRIES = 24;
var entries = Object.create(null);
var pending = Object.create(null);
var sequence = 0;
var generation = 0;

function metadataDetailMethod(item, provider, metadataOnly) {
    if (provider !== "spotify" || !metadataOnly || !item || !item.id)
        return "";
    var id = String(item.id);
    if (item.kind === "album" && id.indexOf("spotify:album:") === 0)
        return "get_album";
    if (item.kind === "artist" && id.indexOf("spotify:artist:") === 0)
        return "get_artist";
    if (item.kind === "playlist" && id.indexOf("spotify:playlist:") === 0
            && id !== "spotify:playlist:liked")
        return "get_playlist";
    return "";
}

function prefetchMetadataDetail(item, provider, metadataOnly, fetcher) {
    var method = metadataDetailMethod(item, provider, metadataOnly);
    if (!method || typeof fetcher !== "function")
        return Promise.resolve(null);
    var params = { id: String(item.id) };
    return get(method, params, function () { return fetcher(method, params); });
}

function key(method, params) {
    return method + ":" + JSON.stringify(params || {});
}

function get(method, params, fetcher) {
    var cacheKey = key(method, params);
    var now = Date.now();
    var cached = entries[cacheKey];
    if (cached && now - cached.time < TTL_MS) {
        cached.used = ++sequence;
        return Promise.resolve(cached.value);
    }
    if (pending[cacheKey])
        return pending[cacheKey];

    var requestGeneration = generation;
    var request = fetcher().then(function(value) {
        if (requestGeneration === generation) {
            entries[cacheKey] = { value: value, time: Date.now(), used: ++sequence };
            trim();
        }
        if (pending[cacheKey] === request)
            delete pending[cacheKey];
        return value;
    }).catch(function(error) {
        if (pending[cacheKey] === request)
            delete pending[cacheKey];
        throw error;
    });
    pending[cacheKey] = request;
    return request;
}

function trim() {
    var keys = Object.keys(entries);
    while (keys.length > MAX_ENTRIES) {
        var oldest = keys[0];
        for (var i = 1; i < keys.length; i++)
            if (entries[keys[i]].used < entries[oldest].used)
                oldest = keys[i];
        delete entries[oldest];
        keys = Object.keys(entries);
    }
}

function clear() {
    generation++;
    entries = Object.create(null);
    pending = Object.create(null);
}

function size() {
    return Object.keys(entries).length;
}
