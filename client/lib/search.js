.pragma library

var resultCache = ({})
var resultCacheOrder = []
var resultRequests = ({})
// Search results are public catalogue data. Keep them warm across normal navigation pauses while
// retaining the explicit bounded cache and in-flight deduplication below.
var RESULT_CACHE_TTL_MS = 60000
var resultGeneration = 0

function resultCacheKey(provider, query) {
    return String(provider || "youtube") + "\n" + String(query || "").trim().replace(/\s+/g, " ").toLowerCase();
}

function cachedResults(provider, query, now) {
    var key = resultCacheKey(provider, query);
    var entry = resultCache[key];
    if (!entry)
        return null;
    now = now === undefined ? Date.now() : now;
    if (now - entry.savedAt > RESULT_CACHE_TTL_MS) {
        delete resultCache[key];
        resultCacheOrder = resultCacheOrder.filter(function (savedKey) { return savedKey !== key; });
        return null;
    }
    return entry.results;
}

function rememberResults(provider, query, results, now) {
    var key = resultCacheKey(provider, query);
    now = now === undefined ? Date.now() : now;
    resultCache[key] = { results: results, savedAt: now };
    resultCacheOrder = resultCacheOrder.filter(function (savedKey) { return savedKey !== key; });
    resultCacheOrder.push(key);
    while (resultCacheOrder.length > 12)
        delete resultCache[resultCacheOrder.shift()];
}

// Share pending searches across SearchSuggest, CommandPalette, and SearchPage so identical queries
// do not each consume a slow provider round-trip.
function loadResults(provider, query, fetcher) {
    var key = resultCacheKey(provider, query);
    var cached = cachedResults(provider, query);
    if (cached !== null)
        return Promise.resolve(cached);
    if (resultRequests[key])
        return resultRequests[key];

    var generation = resultGeneration;
    var request = fetcher().then(function(results) {
        if (generation === resultGeneration)
            rememberResults(provider, query, results);
        if (resultRequests[key] === request)
            delete resultRequests[key];
        return results;
    }).catch(function(error) {
        if (resultRequests[key] === request)
            delete resultRequests[key];
        throw error;
    });
    resultRequests[key] = request;
    return request;
}

function clearResultCache() {
    resultGeneration++;
    resultCache = ({});
    resultCacheOrder = [];
    resultRequests = ({});
}

// The small ranked result set the typeahead surfaces (SearchSuggest, the Ctrl+K palette), ported
// from browse.ts previewMix: the top hit first, then a round-robin across songs / artists / albums
// / playlists rather than six songs, deduped by kind:id. Kept pure so both surfaces show the same
// rows for the same query.
function previewMix(res, limit) {
    limit = limit || 16;
    var out = [];
    var seen = {};
    function push(item) {
        if (!item)
            return;
        var key = item.kind + ":" + item.id;
        if (seen[key])
            return;
        seen[key] = true;
        out.push(item);
    }
    var top = res.top || [];
    for (var i = 0; i < top.length; i++)
        push(top[i]);
    var groups = [res.songs || [], res.artists || [], res.albums || [], res.playlists || []];
    for (var row = 0; out.length < limit; row++) {
        var added = false;
        for (var g = 0; g < groups.length; g++) {
            if (groups[g][row]) { push(groups[g][row]); added = true; }
            if (out.length >= limit)
                break;
        }
        if (!added)
            break;
    }
    return out.slice(0, limit);
}
