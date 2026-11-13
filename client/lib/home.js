.pragma library

var feedCache = ({})
var feedCacheOrder = []
var feedCacheTimes = ({})
// Home is already shown stale-while-revalidate; this only avoids redundant network refreshes on
// quick returns to the page.
var FEED_TTL_MS = 60000
// Keep one launch-time snapshot so a slow first Home request after relaunch can render stale data
// immediately and revalidate in the background. It is a bounded cache, not a durable feed archive.
var PERSISTED_FEED_MAX_AGE_MS = 86400000
var PERSISTED_FEED_MAX_BYTES = 524288

// Home merges YouTube, Spotify, and SoundCloud regardless of the selected catalogue. Tidal is the
// only provider that replaces that feed, so switches among the other three need no reload.
function providerChangeAffectsHome(current, next) {
    return current !== next && (current === "tidal" || next === "tidal");
}

function authEventInvalidatesHome(name, data) {
    if (name === "auth-changed")
        return true;
    if (name !== "spotify-auth" && name !== "soundcloud-auth")
        return false;
    return !!data && (data.state === "signed_in" || data.state === "signed_out");
}

function feedCacheKey(provider, params) {
    return (provider === "tidal" ? "tidal" : "merged") + "\n" + (params || "");
}

function cachedFeed(provider, params) {
    return feedCache[feedCacheKey(provider, params)] || null;
}

function rememberFeed(provider, params, feed, now) {
    var key = feedCacheKey(provider, params);
    feedCache[key] = feed;
    feedCacheTimes[key] = now === undefined ? Date.now() : now;
    feedCacheOrder = feedCacheOrder.filter(function (entry) { return entry !== key; });
    feedCacheOrder.push(key);
    while (feedCacheOrder.length > 8) {
        var expired = feedCacheOrder.shift();
        delete feedCache[expired];
        delete feedCacheTimes[expired];
    }
}

function feedIsFresh(provider, params, now) {
    var key = feedCacheKey(provider, params);
    var cachedAt = feedCacheTimes[key];
    return cachedAt !== undefined && (now === undefined ? Date.now() : now) - cachedAt < FEED_TTL_MS;
}

function clearFeedCache() {
    feedCache = ({});
    feedCacheOrder = [];
    feedCacheTimes = ({});
}

function persistentSnapshot(feed, now) {
    if (!feed || !Array.isArray(feed.sections))
        return "";
    var saved = JSON.stringify({ version: 1, savedAt: now === undefined ? Date.now() : now, feed: feed });
    return saved.length <= PERSISTED_FEED_MAX_BYTES ? saved : "";
}

function restorePersistentSnapshot(raw, now) {
    if (typeof raw !== "string" || !raw || raw.length > PERSISTED_FEED_MAX_BYTES)
        return false;
    var snapshot;
    try { snapshot = JSON.parse(raw); } catch (_) { return false; }
    if (!snapshot || snapshot.version !== 1 || !Number.isFinite(snapshot.savedAt)
            || !snapshot.feed || !Array.isArray(snapshot.feed.sections))
        return false;
    now = now === undefined ? Date.now() : now;
    var age = now - snapshot.savedAt;
    if (age < 0 || age > PERSISTED_FEED_MAX_AGE_MS)
        return false;
    // Continuation tokens are short-lived. Reuse the visible shelves, but wait for the fresh Home
    // response before enabling pagination after a relaunch.
    var feed = Object.assign({}, snapshot.feed, { continuation: null });
    rememberFeed("youtube", "", feed, snapshot.savedAt);
    return true;
}
