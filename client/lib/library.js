.pragma library

var collectionCache = ({})
var collectionCacheOrder = []
// Library edits and account/provider changes invalidate the cache explicitly.
var COLLECTION_CACHE_TTL_MS = 120000

function collectionKey(provider, collection) {
    return String(provider || "youtube") + "\n" + String(collection || "");
}

function cachedCollection(provider, collection, now) {
    var key = collectionKey(provider, collection);
    var entry = collectionCache[key];
    if (!entry)
        return null;
    now = now === undefined ? Date.now() : now;
    if (now - entry.savedAt > COLLECTION_CACHE_TTL_MS) {
        delete collectionCache[key];
        collectionCacheOrder = collectionCacheOrder.filter(function (savedKey) { return savedKey !== key; });
        return null;
    }
    return entry.items;
}

function rememberCollection(provider, collection, items, now) {
    var key = collectionKey(provider, collection);
    now = now === undefined ? Date.now() : now;
    collectionCache[key] = { items: items, savedAt: now };
    collectionCacheOrder = collectionCacheOrder.filter(function (savedKey) { return savedKey !== key; });
    collectionCacheOrder.push(key);
    while (collectionCacheOrder.length > 8)
        delete collectionCache[collectionCacheOrder.shift()];
}

function clearCollectionCache() {
    collectionCache = ({});
    collectionCacheOrder = [];
}
