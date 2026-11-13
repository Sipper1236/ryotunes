.pragma library

var pageCache = ({})
var pageOrder = []
var pendingPages = ({})
var generation = 0
// Liked pages are invalidated when the library changes or Spotify auth changes.
var PAGE_CACHE_TTL_MS = 120000

function firstPageKey(sort, desc) {
    return "first\n" + (sort || "default") + "\n" + (!!desc);
}

function get(key, now) {
    var entry = pageCache[String(key || "")];
    if (!entry)
        return null;
    now = now === undefined ? Date.now() : now;
    if (now - entry.savedAt > PAGE_CACHE_TTL_MS) {
        delete pageCache[String(key || "")];
        pageOrder = pageOrder.filter(function (savedKey) { return savedKey !== String(key || ""); });
        return null;
    }
    return entry.value;
}

function put(key, value, now) {
    key = String(key || "");
    now = now === undefined ? Date.now() : now;
    pageCache[key] = { value: value, savedAt: now };
    pageOrder = pageOrder.filter(function (savedKey) { return savedKey !== key; });
    pageOrder.push(key);
    while (pageOrder.length > 8)
        delete pageCache[pageOrder.shift()];
}

function load(key, fetcher) {
    key = String(key || "");
    var cached = get(key);
    if (cached !== null)
        return Promise.resolve(cached);
    if (pendingPages[key])
        return pendingPages[key];
    var requestGeneration = generation;
    var fetched;
    try { fetched = fetcher(); } catch (error) { return Promise.reject(error); }
    var request = Promise.resolve(fetched).then(function(value) {
        if (requestGeneration === generation)
            put(key, value);
        if (pendingPages[key] === request)
            delete pendingPages[key];
        return value;
    }).catch(function(error) {
        if (pendingPages[key] === request)
            delete pendingPages[key];
        throw error;
    });
    pendingPages[key] = request;
    return request;
}

function loadFirstPage(fetcher, sort, desc) {
    return load(firstPageKey(sort || "default", desc), fetcher);
}

function clear() {
    generation++;
    pageCache = ({});
    pageOrder = [];
    pendingPages = ({});
}
