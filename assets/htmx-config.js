/*
 * htmx configuration, and nothing else.
 *
 * Served as a file rather than inlined into the shell so the policy can be
 * `script-src 'self'` with no `'unsafe-inline'`. The URL carries a hash of
 * these bytes, so an edit here moves the URL and the year of `immutable`
 * caching on the old one stops mattering — without that, a stale copy of this
 * file is a silently broken 422 path.
 *
 * Loaded as a plain (non-deferred) <script src> immediately after htmx itself,
 * which puts it in <head> with the parser still ahead of <body>: `htmx` is
 * already a global by then, and no element exists yet that could trigger a
 * swap. htmx does not read any of these at its own load time — it defers boot
 * to DOMContentLoaded (`readyState === "complete"` ? run now : listen for it)
 * and consults `responseHandling` later still, once a response comes back — so
 * setting them here is early with room to spare.
 */

/*
 * htmx's history cache restores a serialized DOM snapshot, which brings back
 * <canvas> elements with no Chart.js instance behind them — dead charts on
 * every back button. Disabling the cache costs a re-request and keeps pages
 * live.
 */
htmx.config.historyCacheSize = 0;

/*
 * htmx 2's default responseHandling never swaps a 4xx, so the 422 bodies the
 * event forms answer with would be discarded — the user would press Save and
 * watch nothing happen. This override keeps the defaults and adds one rule
 * that swaps 422 (still flagged as an error). Rules match first-wins, so the
 * 422 entry MUST sit before the `[45]..` catch-all.
 */
htmx.config.responseHandling = [{code:"204",swap:false},{code:"[23]..",swap:true},{code:"422",swap:true,error:true},{code:"[45]..",swap:false,error:true}];

/*
 * htmx injects an inline <style> for `.htmx-indicator` when it boots, and the
 * Content-Security-Policy's `style-src 'self'` blocks it — every spinner would
 * be stuck visible. app.css carries the same rules instead.
 */
htmx.config.includeIndicatorStyles = false;
