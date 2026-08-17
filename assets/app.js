/*
 * Client behaviour for watchpost: chart rendering, event-marker overlays, kind
 * filters, sparklines, theme following and the shared error toast.
 *
 * There is no build step. This file is served as written, so it stays plain
 * ES2017+ with no imports — Chart.js is already on the page as a global by the
 * time this runs (both <script>s are `defer`, which keeps them in order).
 *
 * The page hands data over in JSON islands rather than in generated code:
 *   #chart-data   {days, labels:[YYYY-MM-DD…], series:{stars, views_count, …}}
 *   #events-data  [{id, date, kind, title, url}…]
 *   .spark-data   [Option<i64>…]  (one per dashboard card, keyed by class)
 * Every series is dense — one slot per UTC day in the window — which is what
 * lets a category axis double as a date index. A `null` is a genuine "not
 * observed" gap and is never plotted as zero.
 *
 * `#chart-data` always spans the repo's whole history; `days` is only the
 * period to open on. Zooming is therefore a tail slice of arrays already in the
 * page (`setPeriod`), not a request — the server never re-renders for a period
 * change.
 */
/*
 * htmx injects an inline <style> for `.htmx-indicator` when it boots, and the
 * Content-Security-Policy's `style-src 'self'` blocks it — every spinner would
 * be stuck visible. app.css carries the same rules instead. Set before the rest
 * of this file because htmx reads the flag on DOMContentLoaded, which is after
 * this deferred script has run.
 */
htmx.config.includeIndicatorStyles = false;

(function () {
  "use strict";

  // -------------------------------------------------------------------------
  // Theme
  // -------------------------------------------------------------------------

  /*
   * Every colour on every chart comes from a CSS custom property, so light and
   * dark are one palette definition in app.css rather than two in two
   * languages. Reads are cached because `getComputedStyle` forces a style
   * recalculation and the marker plugin asks for colours on every frame;
   * `applyTheme` clears the cache, which is the only moment the answers can
   * change.
   */
  var varCache = new Map();

  function css(name, fallback) {
    if (varCache.has(name)) {
      return varCache.get(name);
    }
    var value = "";
    try {
      value = getComputedStyle(document.documentElement)
        .getPropertyValue(name)
        .trim();
    } catch (err) {
      value = "";
    }
    var resolved = value || fallback;
    varCache.set(name, resolved);
    return resolved;
  }

  /*
   * Charts this file owns and has not destroyed. Needed because a theme change
   * has to reach every live chart, and because a marker redraw must not walk
   * canvases belonging to a page that has since been swapped away.
   */
  var live = new Set();

  function applyTheme() {
    varCache.clear();
    if (typeof Chart === "undefined") {
      return;
    }
    Chart.defaults.color = css("--pico-color", "#373c44");
    Chart.defaults.borderColor = css("--pico-muted-border-color", "#dfe3eb");
    live.forEach(function (chart) {
      if (!chart.canvas) {
        return;
      }
      // Dataset colours were resolved to literal values at init, so re-reading
      // the variable is the only thing that recolours them; `Chart.defaults`
      // alone would leave the lines and bars in the old scheme.
      chart.data.datasets.forEach(function (dataset) {
        if (dataset.$wpVar) {
          var colour = css(dataset.$wpVar, "#888888");
          dataset.borderColor = colour;
          dataset.backgroundColor = colour;
        }
      });
      chart.update("none");
    });
  }

  // -------------------------------------------------------------------------
  // Kind colours
  // -------------------------------------------------------------------------

  var utf8 = new TextEncoder();

  /*
   * djb2 over the kind's bytes, modulo the eight marker slots.
   *
   * This MUST stay byte-for-byte equivalent to `kind_class` in
   * src/routes/html/mod.rs — the server picks a badge's colour with that one
   * and the client picks the matching marker's colour with this one, so a kind
   * that hashed differently here would wear two colours on the same page.
   * Change one, change both. Pinned by a test on the Rust side:
   * "reddit" → slot 7 → `--wp-marker-7`.
   *
   * Two details carry the equivalence:
   *   - iterate UTF-8 *bytes* (Rust's `.bytes()`), not UTF-16 code units, so a
   *     non-ASCII kind hashes the same on both sides;
   *   - `>>> 0` after each step. JS bitwise operators produce a signed int32,
   *     and a negative hash would take `%` negative with it (`-3 % 8` is -3),
   *     landing outside the eight slots. Rust's `u32` wrapping arithmetic has
   *     no such trapdoor.
   */
  function kindSlot(kind) {
    var bytes = utf8.encode(kind);
    var hash = 5381;
    for (var i = 0; i < bytes.length; i++) {
      hash = (Math.imul(hash, 33) ^ bytes[i]) >>> 0;
    }
    return hash % 8;
  }

  function kindColor(kind) {
    if (kind === null || kind === undefined || kind === "") {
      return css("--pico-muted-color", "#6b7280");
    }
    return css("--wp-marker-" + kindSlot(kind), "#888888");
  }

  // -------------------------------------------------------------------------
  // Dates and bucketing
  // -------------------------------------------------------------------------

  /*
   * Parse a `YYYY-MM-DD` label to a UTC timestamp.
   *
   * Deliberately explicit rather than `new Date(label)`: the bare constructor
   * is only UTC for that exact format by spec, and every subsequent read has to
   * remember to use a `getUTC*` accessor. West of Greenwich a single slip makes
   * the label render as the previous day, which is exactly the off-by-one that
   * would put an event marker on the wrong column. Parsing the parts by hand
   * keeps every date in this file an instant, never a wall clock.
   */
  function parseUtc(label) {
    var m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(label);
    if (!m) {
      return NaN;
    }
    return Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  }

  function pad2(n) {
    return n < 10 ? "0" + n : String(n);
  }

  function fmtUtc(ms) {
    var d = new Date(ms);
    return (
      d.getUTCFullYear() +
      "-" +
      pad2(d.getUTCMonth() + 1) +
      "-" +
      pad2(d.getUTCDate())
    );
  }

  var DAY_MS = 86400000;

  /*
   * How wide a bucket has to be for the x-axis to stay readable: a day each up
   * to a quarter, a week each up to a year, a month each beyond that. The span
   * comes from the first and last label of the zoomed window rather than from
   * the selected period, because "All" arrives as -1 and a shorter period can
   * still be longer than the history — only the labels know what is on screen.
   */
  function getBucketKind(labels) {
    if (!labels || labels.length < 2) {
      return "day";
    }
    var first = parseUtc(labels[0]);
    var last = parseUtc(labels[labels.length - 1]);
    if (!isFinite(first) || !isFinite(last)) {
      return "day";
    }
    var days = Math.round((last - first) / DAY_MS) + 1;
    if (days <= 90) {
      return "day";
    }
    if (days <= 365) {
      return "week";
    }
    return "month";
  }

  /*
   * The ISO week's Monday, as a `YYYY-MM-DD` string.
   *
   * A week bucket is keyed by its Monday rather than by an ISO week number:
   * both are stable, but the Monday is a real date, so it sorts as a string,
   * reads as an axis tick without a legend, and needs no year-boundary special
   * case (ISO week 1 can start in December).
   */
  function isoMonday(label) {
    var ms = parseUtc(label);
    if (!isFinite(ms)) {
      return label;
    }
    var offset = (new Date(ms).getUTCDay() + 6) % 7; // Sunday (0) is day 7.
    return fmtUtc(ms - offset * DAY_MS);
  }

  function bucketKeyFor(label, kind) {
    if (kind === "week") {
      return isoMonday(label);
    }
    if (kind === "month") {
      return label.slice(0, 7);
    }
    return label;
  }

  function bucketTitleFor(key, kind) {
    if (kind === "week") {
      return "Week of " + key;
    }
    return key;
  }

  /*
   * Group dense day labels into plot columns.
   *
   * Returns `[{key, title, dayIdxs}]` in ascending order — `key` is the axis
   * label, `title` the tooltip heading, and `dayIdxs` the indices into the
   * original dense arrays that fall in this bucket. Everything downstream
   * (aggregation, and the date → column mapping the markers need) is derived
   * from `dayIdxs`, so the grouping rule lives in exactly one place.
   */
  function bucketize(labels, kind) {
    var buckets = [];
    var byKey = new Map();
    for (var i = 0; i < labels.length; i++) {
      var key = bucketKeyFor(labels[i], kind);
      var bucket = byKey.get(key);
      if (!bucket) {
        bucket = { key: key, title: bucketTitleFor(key, kind), dayIdxs: [] };
        byKey.set(key, bucket);
        buckets.push(bucket);
      }
      bucket.dayIdxs.push(i);
    }
    return buckets;
  }

  /*
   * Roll `values` up over one bucket's days. Nulls are skipped rather than
   * treated as zero, and a bucket with nothing observed in it stays null so the
   * gap survives aggregation.
   */
  function agg(values, idxs, mode) {
    var out = null;
    for (var i = 0; i < idxs.length; i++) {
      var v = values[idxs[i]];
      if (v === null || v === undefined) {
        continue;
      }
      if (mode === "sum") {
        out = out === null ? v : out + v;
      } else if (mode === "max") {
        // Uniques are never summed. A visitor who came on three days of a week
        // is one unique, not three, and GitHub reports no way to deduplicate
        // across days — so the honest weekly figure is the peak daily count,
        // which is why the axis label changes to "Peak daily unique" whenever
        // a bucket is wider than a day.
        out = out === null || v > out ? v : out;
      } else {
        // 'last': stars and total downloads are cumulative and already carried
        // forward server-side, so a bucket's value is its latest observation.
        out = v;
      }
    }
    return out;
  }

  // -------------------------------------------------------------------------
  // Kind filtering
  // -------------------------------------------------------------------------

  /*
   * Kinds the reader has muted. Empty means everything is showing, which is
   * also the "All" state.
   *
   * The chips are the view of this set, not the source of truth: every
   * mutation of `#events-section` re-renders them from the server with their
   * default pressed states, so reading `aria-pressed` back off the DOM would
   * silently reset the filter on the next edit. `applyFilter` pushes this set
   * onto the chips instead, and is called after every swap.
   *
   * Semantics, decided here and matched by `applyFilter`:
   *   - a kind chip is a mute toggle; `aria-pressed="true"` means "showing",
   *     which is the state the client normalises every chip to on load;
   *   - the "All" chip (`kind === null`) is a reset, not a toggle: it clears
   *     every mute and is pressed exactly when nothing is muted;
   *   - kind-less events have no chip of their own, so nothing can mute them
   *     individually. They stay visible and only ever return to view with the
   *     rest under "All" — deliberately, since a filter row that cannot name
   *     them must not be able to hide them either.
   */
  var hiddenKinds = new Set();

  var CHIP_GROUP = '[aria-label="Filter events by kind"]';

  /*
   * `btn` is accepted because the markup passes `this`, but the state lives in
   * `hiddenKinds` — see above for why the DOM cannot be trusted to hold it.
   */
  function toggleKind(kind, btn) {
    void btn;
    if (kind === null || kind === undefined) {
      hiddenKinds.clear();
    } else if (hiddenKinds.has(kind)) {
      hiddenKinds.delete(kind);
    } else {
      hiddenKinds.add(kind);
    }
    applyFilter();
  }

  function isHidden(kind) {
    return (
      kind !== null && kind !== undefined && kind !== "" && hiddenKinds.has(kind)
    );
  }

  function applyFilter() {
    document
      .querySelectorAll("#events-section tr[data-kind]")
      .forEach(function (row) {
        row.hidden = isHidden(row.dataset.kind);
      });

    var group = document.querySelector(CHIP_GROUP);
    if (group) {
      // The chips render in one fixed order: "All" first, then one per kind
      // labelled with the kind itself. Matching on position keeps a kind
      // literally called "All" from colliding with the reset chip.
      var chips = group.querySelectorAll(":scope > button");
      chips.forEach(function (chip, i) {
        var pressed =
          i === 0 ? hiddenKinds.size === 0 : !isHidden(chip.textContent);
        chip.setAttribute("aria-pressed", String(pressed));
      });
    }

    redrawMarkers();
  }

  function redrawMarkers() {
    live.forEach(function (chart) {
      if (chart.$wp && chart.canvas) {
        chart.draw();
      }
    });
  }

  // -------------------------------------------------------------------------
  // The event-marker plugin
  // -------------------------------------------------------------------------

  var HIT_PX = 5;
  var tipEl = null;

  function markerTip() {
    if (tipEl && tipEl.isConnected) {
      return tipEl;
    }
    tipEl = document.getElementById("marker-tip");
    if (!tipEl) {
      tipEl = document.createElement("div");
      tipEl.id = "marker-tip";
      // On the body rather than inside `.chart-box`: the box is
      // `overflow`-clipped and only 220px tall, so a tip anchored in it would
      // be cut off. Absolute positioning against the initial containing block
      // means page coordinates place it, which is exactly what a mouse event
      // reports.
      document.body.appendChild(tipEl);
    }
    return tipEl;
  }

  function hideTip() {
    if (tipEl) {
      tipEl.classList.remove("wp-visible");
    }
  }

  /*
   * Fill the tip from `events`. Built node by node with `textContent` — event
   * titles and kinds are user input, and this is the one place in the client
   * where they reach the DOM.
   */
  function fillTip(tip, events) {
    tip.textContent = "";
    events.forEach(function (ev) {
      var block = document.createElement("div");

      var head = document.createElement("div");
      var when = document.createElement("strong");
      when.textContent = ev.date;
      head.appendChild(when);
      if (ev.kind) {
        var kind = document.createElement("span");
        kind.className = "wp-chip wp-kind-" + kindSlot(ev.kind);
        kind.style.marginLeft = "0.4rem";
        kind.textContent = ev.kind;
        head.appendChild(kind);
      }
      block.appendChild(head);

      var title = document.createElement("div");
      title.textContent = ev.title;
      block.appendChild(title);

      if (ev.url) {
        var link = document.createElement("div");
        var anchor = document.createElement("a");
        // The server already scheme-allowlisted this on the write path
        // (`validate_event_url`); the tip is `pointer-events: none` besides, so
        // the anchor is a visual affordance pointing at the row's real link.
        anchor.href = ev.url;
        anchor.rel = "noopener noreferrer";
        anchor.className = "wp-small";
        anchor.textContent = ev.url;
        link.appendChild(anchor);
        block.appendChild(link);
      }

      tip.appendChild(block);
    });
  }

  function showTip(events, native) {
    var tip = markerTip();
    fillTip(tip, events);
    tip.classList.add("wp-visible");

    var x = native.pageX + 14;
    var y = native.pageY + 14;
    var maxX = window.scrollX + document.documentElement.clientWidth - 8;
    if (x + tip.offsetWidth > maxX) {
      x = Math.max(window.scrollX + 8, native.pageX - tip.offsetWidth - 14);
    }
    tip.style.left = x + "px";
    tip.style.top = y + "px";
  }

  /*
   * Scroll an event's table row into view and flash it, so a marker click on
   * the chart answers "which event is this?" without the reader hunting.
   */
  function focusRow(id) {
    var row = document.getElementById("event-row-" + id);
    if (!row) {
      return;
    }
    row.scrollIntoView({ block: "center", behavior: "smooth" });
    // Removing and forcing a reflow before re-adding restarts the animation on
    // a second click of the same marker; without it the class is already there
    // and nothing visible happens.
    row.classList.remove("wp-flash");
    void row.offsetWidth;
    row.classList.add("wp-flash");
    // A timer rather than `animationend`, because `prefers-reduced-motion`
    // turns the animation off entirely and the event would never fire.
    setTimeout(function () {
      row.classList.remove("wp-flash");
    }, 1600);
  }

  /*
   * Every event that is showing, is on a date this window covers, and knows
   * which column it belongs to — paired with that column's pixel.
   *
   * The two-step date → day → bucket mapping is the point. At week or month
   * zoom the chart's labels are bucket keys, so `labels.indexOf(ev.date)` finds
   * nothing and the marker silently disappears; `bucketOf` is built from the
   * same `dayIdxs` the aggregation used, so a date always resolves to the
   * column its data was rolled into.
   */
  function placedEvents(chart) {
    var wp = chart.$wp;
    var scale = chart.scales.x;
    var out = [];
    if (!wp || !scale) {
      return out;
    }
    wp.events.forEach(function (ev) {
      if (isHidden(ev.kind)) {
        return;
      }
      var idx = wp.bucketOf.get(ev.date);
      if (idx === undefined) {
        return;
      }
      out.push({ event: ev, x: scale.getPixelForValue(idx) });
    });
    return out;
  }

  function hitsAt(chart, x, y) {
    var area = chart.chartArea;
    if (!area || y < area.top - HIT_PX || y > area.bottom + HIT_PX) {
      return [];
    }
    return placedEvents(chart)
      .filter(function (placed) {
        return Math.abs(placed.x - x) <= HIT_PX;
      })
      .map(function (placed) {
        return placed.event;
      });
  }

  var eventMarkers = {
    id: "eventMarkers",

    afterDraw: function (chart) {
      var area = chart.chartArea;
      if (!area) {
        return;
      }
      var placed = placedEvents(chart);
      if (!placed.length) {
        return;
      }
      var ctx = chart.ctx;
      ctx.save();
      ctx.lineWidth = 1.5;
      placed.forEach(function (item) {
        if (!isFinite(item.x)) {
          return;
        }
        var colour = kindColor(item.event.kind);
        ctx.strokeStyle = colour;
        ctx.fillStyle = colour;
        ctx.setLineDash([4, 3]);
        ctx.beginPath();
        ctx.moveTo(item.x, area.top);
        ctx.lineTo(item.x, area.bottom);
        ctx.stroke();
        // The dot is the hover target's advertisement — a dashed hairline alone
        // reads as grid decoration.
        ctx.setLineDash([]);
        ctx.beginPath();
        ctx.arc(item.x, area.top + 3, 3, 0, Math.PI * 2);
        ctx.fill();
      });
      ctx.restore();
    },

    afterEvent: function (chart, args) {
      var e = args.event;
      if (!e || !chart.$wp) {
        return;
      }
      if (e.type === "mouseout") {
        hideTip();
        return;
      }
      if (e.type !== "mousemove" && e.type !== "click") {
        return;
      }
      var hits = hitsAt(chart, e.x, e.y);
      if (e.type === "click") {
        if (hits.length) {
          focusRow(hits[0].id);
        }
        return;
      }
      if (hits.length && e.native) {
        // Several events on one day (or in one week) share a column, so the
        // tip lists all of them rather than picking one arbitrarily.
        showTip(hits, e.native);
        chart.canvas.style.cursor = "pointer";
      } else {
        hideTip();
        chart.canvas.style.cursor = "";
      }
    },
  };

  // -------------------------------------------------------------------------
  // Chart construction
  // -------------------------------------------------------------------------

  function readJson(id) {
    var el = document.getElementById(id);
    if (!el) {
      return null;
    }
    try {
      return JSON.parse(el.textContent);
    } catch (err) {
      return null;
    }
  }

  /*
   * Tear down whatever chart already owns this canvas.
   *
   * A period change re-creates all four charts on the canvases they are already
   * drawn on. Re-creating without this leaks the old instance and its resize
   * listener; `Chart.getChart` is the supported way to find it.
   */
  function destroyOn(canvas) {
    var old = Chart.getChart(canvas);
    if (old) {
      live.delete(old);
      old.destroy();
    }
  }

  /*
   * Destroy every chart whose canvas has left the document.
   *
   * `destroyOn` cannot catch these. An `outerHTML` swap brings *new* canvas
   * elements — `Chart.getChart(newCanvas)` finds nothing, and the charts bound
   * to the discarded ones stay registered with their resize observers attached,
   * after which `applyTheme` spends its time updating charts drawing into
   * detached canvases.
   *
   * Called only from `htmx:afterSwap`, and that timing is the whole trick:
   * htmx inserts the new fragment (running any inline init it carries) *before*
   * it removes the old one, so at the moment charts are rebuilt the outgoing
   * canvases are still connected and nothing here would match. By `afterSwap`
   * they are gone.
   */
  function pruneDetached() {
    live.forEach(function (chart) {
      if (!chart.canvas || !chart.canvas.isConnected) {
        live.delete(chart);
        chart.destroy();
      }
    });
  }

  function makeChart(canvasId, spec) {
    var canvas = document.getElementById(canvasId);
    if (!canvas) {
      return null;
    }
    destroyOn(canvas);

    spec.datasets.forEach(function (dataset) {
      var colour = css(dataset.$wpVar, "#888888");
      dataset.borderColor = colour;
      dataset.backgroundColor = colour;
      // A null is a day watchpost did not observe, not a zero. Bridging it
      // would draw a straight line through missing data and read as a real
      // measurement.
      dataset.spanGaps = false;
    });

    var chart = new Chart(canvas, {
      type: spec.type,
      data: { labels: spec.labels, datasets: spec.datasets },
      options: {
        responsive: true,
        // `.chart-box` supplies the height; without this the canvas grows on
        // every resize.
        maintainAspectRatio: false,
        // Animation off: a period change re-creates these charts from scratch,
        // and a growth animation on every swap reads as a glitch rather than a
        // transition. It also keeps the marker overlay in step with the axis.
        animation: false,
        // Hovering anywhere in a column reports every series in it, which is
        // what a reader comparing count against uniques wants.
        interaction: { mode: "index", intersect: false },
        scales: {
          x: {
            type: "category",
            grid: { display: false },
            // Horizontal ticks only — a tilted date is harder to read than a
            // sparser axis. The padding is what buys the sparseness: a bare
            // `autoSkip` packs `YYYY-MM-DD` labels shoulder to shoulder in a
            // card-width chart and they run together.
            ticks: { maxRotation: 0, autoSkip: true, autoSkipPadding: 16 },
          },
          y: {
            beginAtZero: spec.zeroBased !== false,
            ticks: { precision: 0 },
          },
        },
        plugins: {
          legend: { display: spec.datasets.length > 1 },
          tooltip: {
            callbacks: {
              title: function (items) {
                return spec.titles[items[0].dataIndex];
              },
              label: function (item) {
                var name = item.dataset.label ? item.dataset.label + ": " : "";
                // Chart.js formats a null as "0". Leaving that alone would
                // undo the whole point of plotting gaps as gaps: the bar is
                // correctly absent, and then the tooltip tells the reader the
                // repo got zero views that day.
                if (item.raw === null || item.raw === undefined) {
                  return name + "not observed";
                }
                return name + item.formattedValue;
              },
            },
          },
        },
      },
      plugins: [eventMarkers],
    });

    live.add(chart);
    return chart;
  }

  /*
   * The `#chart-data` island the current charts were built from. Only the
   * htmx belt-and-braces re-init consults it: the swapped-in fragment carries
   * its own `watchpost.initRepoCharts()` call, so by the time `htmx:afterSwap`
   * fires the charts are usually already up, and rebuilding them a second time
   * would be visible work for no change.
   */
  var chartSource = null;

  /* The whole-history payload the charts zoom over, and the period showing. */
  var chartPayload = null;

  var ALL_DAYS = -1;

  /*
   * The period allowlist. Mirrors `PERIODS` in src/routes/html/repo.rs, which
   * is what renders the options and what validates a `?days=` on the way in —
   * this copy only guards against a value arriving from somewhere else.
   */
  var PERIODS = [7, 30, 90, 365, ALL_DAYS];

  function normalisePeriod(value) {
    var days = Number(value);
    return PERIODS.indexOf(days) === -1 ? ALL_DAYS : days;
  }

  /*
   * The trailing `days` of a dense array, or all of it for "All" (and for a
   * window longer than the history, which `slice` already handles).
   *
   * Slicing the tail of a carried-forward series is safe: the values were
   * materialized server-side, so a window opening mid-carry opens on the level
   * that was carried into it rather than on a null.
   */
  function tail(values, days) {
    if (!Array.isArray(values)) {
      return [];
    }
    return days > 0 ? values.slice(-days) : values.slice();
  }

  function initRepoCharts() {
    if (typeof Chart === "undefined") {
      return;
    }
    var el = document.getElementById("chart-data");
    var payload = readJson("chart-data");
    if (!payload || !Array.isArray(payload.labels) || !payload.labels.length) {
      return;
    }
    chartPayload = payload;
    chartSource = el;
    renderCharts(payload, normalisePeriod(payload.days));
  }

  /*
   * Zoom to `value` days: re-render from the payload already in the page and
   * put the choice in the address bar, so a reload or a shared link opens on
   * the same period. `replaceState` rather than `pushState` — a zoom is not a
   * navigation, and the back button should leave the page rather than step
   * through every period the reader tried.
   */
  function setPeriod(value) {
    if (!chartPayload) {
      return;
    }
    var days = normalisePeriod(value);
    renderCharts(chartPayload, days);
    syncPeriodUrl(days);
  }

  function syncPeriodUrl(days) {
    if (!window.history || !history.replaceState || typeof URL !== "function") {
      return;
    }
    try {
      var url = new URL(window.location.href);
      url.searchParams.set("days", String(days));
      history.replaceState(history.state, "", url.toString());
    } catch (err) {
      // A location the URL parser refuses is not worth failing a zoom over.
    }
  }

  /* Build the four repo charts from the trailing `days` of `payload`. */
  function renderCharts(payload, days) {
    var labels = tail(payload.labels, days);
    var source = payload.series || {};
    var kind = getBucketKind(labels);
    var buckets = bucketize(labels, kind);
    var keys = buckets.map(function (b) {
      return b.key;
    });
    var titles = buckets.map(function (b) {
      return b.title;
    });

    // date → column, for the marker plugin. Built from the same buckets the
    // values were aggregated over, so a marker cannot drift from its data.
    var bucketOf = new Map();
    buckets.forEach(function (bucket, idx) {
      bucket.dayIdxs.forEach(function (dayIdx) {
        bucketOf.set(labels[dayIdx], idx);
      });
    });

    function rollup(name, mode) {
      var values = tail(source[name], days);
      return buckets.map(function (bucket) {
        return agg(values, bucket.dayIdxs, mode);
      });
    }

    // At day zoom the uniques bar is that day's unique count; wider buckets
    // cannot sum it (see `agg`), so the label says what the number really is.
    var uniquesLabel = kind === "day" ? "Unique" : "Peak daily unique";

    var charts = [
      makeChart("chart_stars", {
        type: "line",
        labels: keys,
        titles: titles,
        // Stars are a running total, so the axis reads better tight around the
        // curve than anchored at zero.
        zeroBased: false,
        datasets: [
          {
            label: "Stars",
            data: rollup("stars", "last"),
            $wpVar: "--wp-marker-3",
            borderWidth: 2,
            tension: 0,
            pointStyle: false,
            fill: false,
          },
        ],
      }),
      makeChart("chart_views", {
        type: "bar",
        labels: keys,
        titles: titles,
        datasets: [
          {
            label: "Views",
            data: rollup("views_count", "sum"),
            $wpVar: "--wp-marker-0",
          },
          {
            label: uniquesLabel,
            data: rollup("views_uniques", "max"),
            $wpVar: "--wp-marker-5",
          },
        ],
      }),
      makeChart("chart_clones", {
        type: "bar",
        labels: keys,
        titles: titles,
        datasets: [
          {
            label: "Clones",
            data: rollup("clones_count", "sum"),
            $wpVar: "--wp-marker-2",
          },
          {
            label: uniquesLabel,
            data: rollup("clones_uniques", "max"),
            $wpVar: "--wp-marker-4",
          },
        ],
      }),
      makeChart("chart_downloads", {
        type: "line",
        labels: keys,
        titles: titles,
        zeroBased: false,
        datasets: [
          {
            label: "Downloads",
            data: rollup("downloads_total", "last"),
            $wpVar: "--wp-marker-6",
            borderWidth: 2,
            tension: 0,
            pointStyle: false,
            fill: false,
          },
        ],
      }),
    ];

    var events = readJson("events-data") || [];
    charts.forEach(function (chart) {
      if (!chart) {
        return;
      }
      // Attached after construction — the first render happens inside the
      // constructor, before this exists, which is why the plugin treats a
      // missing `$wp` as "nothing to draw" and why each chart is drawn once
      // more here.
      chart.$wp = { events: events, bucketOf: bucketOf };
      chart.draw();
    });

    applyFilter();
  }

  /*
   * Re-read `#events-data` after an event was added, edited or deleted.
   *
   * Deliberately not a re-init: destroying and rebuilding four charts to move
   * one dashed line makes the whole section blink on every save. The markers
   * are drawn from `chart.$wp.events` on each frame, so swapping that array and
   * asking for a redraw is the entire update.
   */
  function refreshMarkers() {
    var events = readJson("events-data") || [];
    live.forEach(function (chart) {
      if (chart.$wp) {
        chart.$wp.events = events;
      }
    });
    // The swap also re-rendered the rows and chips from the server's defaults,
    // so the active filter has to be pushed back onto them. This redraws the
    // markers too.
    applyFilter();
  }

  // -------------------------------------------------------------------------
  // Dashboard sparklines
  // -------------------------------------------------------------------------

  function sparkData(canvas) {
    var sibling = canvas.nextElementSibling;
    var holder =
      sibling && sibling.classList.contains("spark-data")
        ? sibling
        : canvas.parentElement &&
          canvas.parentElement.querySelector(".spark-data");
    if (!holder) {
      return null;
    }
    try {
      var parsed = JSON.parse(holder.textContent);
      return Array.isArray(parsed) ? parsed : null;
    } catch (err) {
      return null;
    }
  }

  function initSparklines(root) {
    if (typeof Chart === "undefined") {
      return;
    }
    var scope = root && root.querySelectorAll ? root : document;
    var canvases = Array.prototype.slice.call(
      scope.querySelectorAll("canvas.spark"),
    );
    // An htmx swap can deliver the canvas as the swapped element itself, which
    // `querySelectorAll` on that element would not find.
    if (scope.matches && scope.matches("canvas.spark")) {
      canvases.push(scope);
    }

    var colour = css("--wp-marker-0", "#2f6fd0");
    canvases.forEach(function (canvas) {
      var values = sparkData(canvas);
      if (!values) {
        return;
      }
      destroyOn(canvas);
      var chart = new Chart(canvas, {
        type: "line",
        data: {
          // Positional labels: nothing displays them, they only give the
          // category axis one slot per day.
          labels: values.map(function (_, i) {
            return i;
          }),
          datasets: [
            {
              data: values,
              // Named variable, not just the literal: `applyTheme()` re-reads
              // `$wpVar` on a scheme flip, so sparklines recolour with the
              // rest of the charts instead of keeping the old theme's line.
              $wpVar: "--wp-marker-0",
              borderColor: colour,
              borderWidth: 1.5,
              pointRadius: 0,
              tension: 0,
              fill: false,
              // Same rule as the big charts: a day with no observation is a
              // break, not a dip to zero.
              spanGaps: false,
            },
          ],
        },
        options: {
          responsive: true,
          maintainAspectRatio: false,
          animation: false,
          // A sparkline is a shape, not a readout — no axes, no legend, and no
          // tooltip to chase with a pointer.
          scales: { x: { display: false }, y: { display: false } },
          plugins: { legend: { display: false }, tooltip: { enabled: false } },
          elements: { point: { radius: 0 } },
        },
      });
      live.add(chart);
    });
  }

  // -------------------------------------------------------------------------
  // Toast
  // -------------------------------------------------------------------------

  /*
   * The shell ships one hidden toast (`#wp-toast`) that this section fills in.
   * It is the only report a failed request gets: the shell's
   * `responseHandling` never swaps a 4xx/5xx, so the server's error page is
   * parsed and thrown away, and without this a 403 from an expired CSRF cookie
   * is indistinguishable from a dead button.
   *
   * Text is written with `textContent`, never `innerHTML` — same rule as the
   * chart tooltip. The strings here are literals, and keeping the rule absolute
   * means a later caller cannot turn a server-supplied message into markup.
   */
  var TOAST_MS = 8000;

  /*
   * One timer handle for the whole page: a new toast replaces the message, so
   * it has to replace the countdown too or the second message inherits what was
   * left of the first one's.
   */
  var toastTimer = null;

  /*
   * What the action button runs, cleared on hide so the button can never fire a
   * closure belonging to a message that is no longer on screen.
   */
  var toastAction = null;

  var RELOAD = {
    label: "Reload",
    fn: function () {
      window.location.reload();
    },
  };

  function showToast(text, opts) {
    var toast = document.getElementById("wp-toast");
    if (!toast) {
      return;
    }
    var options = opts || {};
    var textEl = toast.querySelector(".wp-toast-text");
    var actionEl = toast.querySelector(".wp-toast-action");
    if (textEl) {
      textEl.textContent = text;
    }
    toastAction = options.action ? options.action.fn : null;
    if (actionEl) {
      actionEl.textContent = options.action ? options.action.label : "";
      actionEl.hidden = !options.action;
    }
    toast.hidden = false;
    if (toastTimer !== null) {
      clearTimeout(toastTimer);
      toastTimer = null;
    }
    // A sticky message reports something the page cannot recover from on its
    // own, so it waits for the reader instead of timing out unread.
    if (!options.sticky) {
      toastTimer = setTimeout(hideToast, TOAST_MS);
    }
  }

  function hideToast() {
    if (toastTimer !== null) {
      clearTimeout(toastTimer);
      toastTimer = null;
    }
    toastAction = null;
    var toast = document.getElementById("wp-toast");
    if (toast) {
      toast.hidden = true;
    }
  }

  /*
   * What the status means to the person who pressed the button, not what it
   * means to HTTP. A 403 is the CSRF cookie having expired: no retry fixes it
   * and a reload does, so that message sticks and carries the reload. A 404 is
   * a row that is gone from the database but still on screen, which is the same
   * cure without the urgency.
   */
  function messageForStatus(status) {
    if (status === 403) {
      return {
        text: "Your session expired. Reload the page and try again.",
        sticky: true,
        action: RELOAD,
      };
    }
    if (status === 404) {
      return { text: "That item no longer exists.", action: RELOAD };
    }
    if (status >= 500) {
      return { text: "Server error — your change was not saved." };
    }
    return { text: "Request failed (" + status + ")." };
  }

  document.addEventListener("htmx:responseError", function (evt) {
    var xhr = evt.detail ? evt.detail.xhr : null;
    var status = xhr ? xhr.status : 0;
    // 422 is the one error status the shell swaps: the response body is the
    // form with its field errors, which says more than a corner toast could.
    if (status === 422) {
      return;
    }
    var message = messageForStatus(status);
    showToast(message.text, message);
  });

  // No response at all — offline, DNS, a server that is not up yet. Sticky
  // because there is no follow-up event to correct it once connectivity is
  // back; the next request that succeeds clears it.
  document.addEventListener("htmx:sendError", function () {
    showToast("Network error — the server could not be reached.", {
      sticky: true,
    });
  });

  document.addEventListener("htmx:timeout", function () {
    showToast("The request timed out. Try again.");
  });

  /*
   * A polling element requests on a timer, so its success reports nothing about
   * the failure on screen and nobody is waiting on it.
   */
  function isPolling(elt) {
    if (!elt || !elt.closest) {
      return false;
    }
    // `closest` starts at the element itself, which is where the attribute sits
    // on the settings poller.
    var source = elt.closest("[hx-trigger]");
    if (!source) {
      return false;
    }
    return (source.getAttribute("hx-trigger") || "").indexOf("every") !== -1;
  }

  /*
   * A request that worked answers whatever the last one failed at, and a stale
   * error next to fresh content is worse than no error. htmx fires this after
   * `htmx:responseError` and only sets `successful` on a non-error response, so
   * a failure cannot clear the toast it just raised.
   */
  document.addEventListener("htmx:afterRequest", function (evt) {
    if (!evt.detail || !evt.detail.successful) {
      return;
    }
    // Settings polls `#sync-status` every 2s while a sync runs; left alone those
    // successes would wipe a sticky 403 two seconds after it appeared.
    if (isPolling(evt.detail.elt)) {
      return;
    }
    hideToast();
  });

  document.addEventListener("click", function (evt) {
    var target = evt.target;
    if (!target || !target.closest) {
      return;
    }
    if (target.closest(".wp-toast-close")) {
      hideToast();
      return;
    }
    if (target.closest(".wp-toast-action")) {
      // Read the handler before hiding: `hideToast` clears it.
      var run = toastAction;
      hideToast();
      if (run) {
        run();
      }
    }
  });

  document.addEventListener("keydown", function (evt) {
    if (evt.key !== "Escape") {
      return;
    }
    var toast = document.getElementById("wp-toast");
    if (!toast || toast.hidden) {
      return;
    }
    // An open modal owns Escape. Dismissing the toast behind it would swallow
    // the keypress the user meant for the dialog.
    var dialog = document.getElementById("wp-confirm");
    if (dialog && dialog.open) {
      return;
    }
    hideToast();
  });

  // -------------------------------------------------------------------------
  // Wiring
  // -------------------------------------------------------------------------

  function boot() {
    applyTheme();
    initSparklines(document);
    // The fragment's own `initRepoCharts()` call runs before this file is
    // parsed on a full page load (both scripts are deferred), so it is a no-op
    // there and this is the call that actually builds the charts.
    initRepoCharts();
    applyFilter();
  }

  /*
   * The chart period selector. Delegated on `document` rather than bound to the
   * element, because the charts section is rendered by the server (and can
   * arrive in a swap): a listener bound at boot would either miss it or hold on
   * to a select that has since been replaced.
   *
   * The raw value goes to `setPeriod`, which allowlists it — the same thing the
   * inline `onchange` this replaced used to do.
   */
  document.addEventListener("change", function (evt) {
    var target = evt.target;
    if (target && target.matches && target.matches("[data-period-select]")) {
      setPeriod(target.value);
    }
  });

  document.addEventListener("htmx:afterSwap", function (evt) {
    var target = evt.target;
    if (!target || !target.querySelectorAll) {
      return;
    }
    pruneDetached();
    if (
      target.querySelector("canvas.spark") ||
      (target.matches && target.matches("canvas.spark"))
    ) {
      initSparklines(target);
    }
    // Belt to the fragment's inline calls, for a swap that lands the islands
    // without them (an out-of-band swap, say). Both are guarded against doing
    // the work twice: the charts only rebuild if the payload element is a new
    // one, and a marker refresh is a redraw rather than a rebuild.
    var chartEl = document.getElementById("chart-data");
    if (chartEl && chartEl !== chartSource) {
      initRepoCharts();
    } else if (document.getElementById("events-data")) {
      refreshMarkers();
    }
  });

  var scheme =
    typeof window.matchMedia === "function"
      ? window.matchMedia("(prefers-color-scheme: dark)")
      : null;
  if (scheme) {
    if (scheme.addEventListener) {
      scheme.addEventListener("change", applyTheme);
    } else if (scheme.addListener) {
      scheme.addListener(applyTheme);
    }
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", boot);
  } else {
    boot();
  }

  window.watchpost = {
    initRepoCharts: initRepoCharts,
    setPeriod: setPeriod,
    refreshMarkers: refreshMarkers,
    toggleKind: toggleKind,
    initSparklines: initSparklines,
    applyTheme: applyTheme,
    showToast: showToast,
    hideToast: hideToast,
  };
})();
