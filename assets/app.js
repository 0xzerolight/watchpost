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
   * mutation of `#events-section` re-renders them from the server pressed —
   * the unfiltered state — so reading `aria-pressed` back off the DOM would
   * silently reset the filter on the next edit. `applyFilter` pushes this set
   * onto the chips instead, and is called after every swap.
   *
   * Semantics, decided here and matched by `applyFilter`:
   *   - a kind chip is a mute toggle; `aria-pressed="true"` means "showing",
   *     which is the state the server renders every chip in;
   *   - the "All" chip (`kind === null`) is a reset, not a toggle: it clears
   *     every mute and is pressed exactly when nothing is muted;
   *   - kind-less events have no chip of their own, so nothing can mute them
   *     individually. They stay visible and only ever return to view with the
   *     rest under "All" — deliberately, since a filter row that cannot name
   *     them must not be able to hide them either.
   */
  var hiddenKinds = new Set();

  var CHIP = "[data-chip-kind],[data-chip-all]";

  /* The kind a chip filters, or null for the "All" reset. */
  function chipKind(chip) {
    return chip.hasAttribute("data-chip-all")
      ? null
      : chip.getAttribute("data-chip-kind");
  }

  function toggleKind(kind) {
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

    // Each chip names its own kind in an attribute, so nothing here depends on
    // the chips' order or on their labels — a kind literally called "All" is
    // just another `data-chip-kind`.
    document.querySelectorAll(CHIP).forEach(function (chip) {
      var kind = chipKind(chip);
      var pressed = kind === null ? hiddenKinds.size === 0 : !isHidden(kind);
      chip.setAttribute("aria-pressed", String(pressed));
    });

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

  /*
   * How near the pointer has to be to a marker's column, in pixels, for its tip
   * to open. Wider than the line it targets: a marker is a stroke on a canvas
   * with no DOM node behind it, so this slack is the entire hit area, and 5px
   * asked for a precision a trackpad does not have.
   *
   * Markers are a mouse enhancement, not a way to reach an event. There is
   * nothing here to focus and nothing to announce — the events table under the
   * charts lists the same events as real rows, with the real links, and that is
   * the accessible equivalent this widget defers to.
   */
  var HIT_PX = 8;
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
   * The host of an event's URL, for the tip's one line about where it points.
   * Every stored URL came through `validate_event_url` and is an absolute
   * http(s) one, so the fallback is for a row that predates that check rather
   * than for anything routine: showing the raw string beats dropping the line.
   */
  function urlHost(url) {
    try {
      return new URL(url).host || url;
    } catch (err) {
      return url;
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
        kind.className = "wp-chip wp-tip-kind wp-kind-" + kindSlot(ev.kind);
        kind.textContent = ev.kind;
        head.appendChild(kind);
      }
      block.appendChild(head);

      var title = document.createElement("div");
      title.textContent = ev.title;
      block.appendChild(title);

      if (ev.url) {
        var where = document.createElement("div");
        // Not an `<a>`: the tip is `pointer-events: none`, so a link in it can
        // never be clicked, and one that looks clickable and is not costs the
        // reader an attempt. The host says where the event points; the row in
        // the events table carries the link that actually works.
        var host = document.createElement("span");
        host.className = "wp-small wp-muted";
        host.textContent = urlHost(ev.url);
        where.appendChild(host);
        block.appendChild(where);
      }

      tip.appendChild(block);
    });
  }

  function showTip(events, native) {
    var tip = markerTip();
    fillTip(tip, events);
    tip.classList.add("wp-visible");

    // Both measurements are taken after the fill and the unhide: a hidden
    // element reports zero for `offsetWidth`/`offsetHeight`, so a tip measured
    // any earlier would decide it fits everywhere.
    var x = native.pageX + 14;
    var y = native.pageY + 14;
    var maxX = window.scrollX + document.documentElement.clientWidth - 8;
    if (x + tip.offsetWidth > maxX) {
      x = Math.max(window.scrollX + 8, native.pageX - tip.offsetWidth - 14);
    }
    // The same flip vertically. Without it a marker near the foot of the window
    // opened its tip below the fold — the tip is positioned in page
    // coordinates, so nothing scrolls it back into view. Flipping above the
    // cursor keeps it beside the marker it belongs to; the `Math.max` pins a
    // tip taller than the viewport to the top edge, losing its last line rather
    // than its first.
    var maxY = window.scrollY + document.documentElement.clientHeight - 8;
    if (y + tip.offsetHeight > maxY) {
      y = Math.max(window.scrollY + 8, native.pageY - tip.offsetHeight - 14);
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
    // Asked at click time rather than cached: the preference can change
    // mid-session, and a reader who has asked for less motion gets the jump —
    // app.css already cancels the flash below for them.
    var reduced =
      typeof window.matchMedia === "function" &&
      window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    row.scrollIntoView({
      block: "center",
      behavior: reduced ? "auto" : "smooth",
    });
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
   * Chart.js refuses to build on a canvas that is already in use, and a
   * discarded instance keeps its resize observer attached; `Chart.getChart` is
   * the supported way to find it. Sparklines rebuild on their own canvases
   * after a swap, and `syncChart` comes through here when a canvas holds a
   * chart it cannot update into the one the spec describes.
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

  /*
   * A cumulative series' line shape. Copied onto each dataset that asks for it
   * rather than handed over as-is — see `CHART_SPECS`.
   */
  var LINE_STYLE = {
    borderWidth: 2,
    tension: 0,
    pointStyle: false,
    fill: false,
  };

  /*
   * The four repo charts, as data.
   *
   * These are descriptors and nothing here is ever written to. Chart.js owns
   * the objects it is handed — it stores the dataset object itself and
   * `applyTheme` writes resolved colours onto it — so `buildDataset` copies
   * what it needs onto a fresh object per chart, and one shared style constant
   * cannot end up wearing four charts' colours in turn.
   *
   * The two policies that used to be restated per chart live here once:
   *
   *   - `zeroBased` follows what the series measures. Stars and total
   *     downloads are running totals, and their axis reads better tight around
   *     the curve than anchored at a zero the data never visits; views and
   *     clones are counts per bucket, where a floating zero would exaggerate
   *     every wobble.
   *   - `mode` is the roll-up `agg` applies once a bucket is wider than a day,
   *     and it is a property of the series rather than of the chart: a
   *     carried-forward total takes its last observation, a count sums, and
   *     uniques can only peak.
   */
  var CHART_SPECS = [
    {
      canvasId: "chart_stars",
      type: "line",
      zeroBased: false,
      datasets: [
        {
          source: "stars",
          label: "Stars",
          mode: "last",
          cssVar: "--wp-marker-3",
          style: LINE_STYLE,
        },
      ],
    },
    {
      canvasId: "chart_views",
      type: "bar",
      zeroBased: true,
      datasets: [
        {
          source: "views_count",
          label: "Views",
          mode: "sum",
          cssVar: "--wp-marker-0",
        },
        {
          source: "views_uniques",
          // The uniques series is called something else once a bucket is wider
          // than a day, so its label comes from the view.
          labelKey: "uniquesLabel",
          mode: "max",
          cssVar: "--wp-marker-5",
        },
      ],
    },
    {
      canvasId: "chart_clones",
      type: "bar",
      zeroBased: true,
      datasets: [
        {
          source: "clones_count",
          label: "Clones",
          mode: "sum",
          cssVar: "--wp-marker-2",
        },
        {
          source: "clones_uniques",
          labelKey: "uniquesLabel",
          mode: "max",
          cssVar: "--wp-marker-4",
        },
      ],
    },
    {
      canvasId: "chart_downloads",
      type: "line",
      zeroBased: false,
      datasets: [
        {
          source: "downloads_total",
          label: "Downloads",
          mode: "last",
          cssVar: "--wp-marker-6",
          style: LINE_STYLE,
        },
      ],
    },
  ];

  function datasetLabel(descriptor, view) {
    return descriptor.labelKey ? view[descriptor.labelKey] : descriptor.label;
  }

  function buildDataset(descriptor, view) {
    var colour = css(descriptor.cssVar, "#888888");
    var dataset = {
      label: datasetLabel(descriptor, view),
      data: view.values[descriptor.source],
      // The variable name travels with the dataset because the colour above is
      // a resolved literal: `applyTheme` re-reads `$wpVar` on a scheme flip,
      // and that is the only thing that recolours a line already drawn.
      $wpVar: descriptor.cssVar,
      borderColor: colour,
      backgroundColor: colour,
      // A null is a day watchpost did not observe, not a zero. Bridging it
      // would draw a straight line through missing data and read as a real
      // measurement.
      spanGaps: false,
    };
    return Object.assign(dataset, descriptor.style);
  }

  function createChart(canvas, spec, view, events) {
    destroyOn(canvas);

    var chart = new Chart(canvas, {
      type: spec.type,
      data: {
        labels: view.keys,
        datasets: spec.datasets.map(function (descriptor) {
          return buildDataset(descriptor, view);
        }),
      },
      options: {
        responsive: true,
        // `.chart-box` supplies the height; without this the canvas grows on
        // every resize.
        maintainAspectRatio: false,
        // Animation off: the markers are painted at the axis' final pixel
        // positions, so a tweening axis would leave every dashed line standing
        // beside the column it belongs to until the animation settled.
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
            beginAtZero: spec.zeroBased,
            ticks: { precision: 0 },
          },
        },
        plugins: {
          // A legend earns its space only where there are two series to tell
          // apart.
          legend: { display: spec.datasets.length > 1 },
          tooltip: {
            callbacks: {
              title: function (items) {
                // Read off the chart, not off a captured array: the titles
                // change under a live chart on every period change.
                return items[0].chart.$wp.titles[items[0].dataIndex];
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

    /*
     * What this file keeps on a chart beyond what Chart.js knows about: the
     * events to mark, the date → column map that places them, and the tooltip
     * headings.
     *
     * Attached after construction — the first render happens inside the
     * constructor, before this exists, which is why the plugin treats a missing
     * `$wp` as "nothing to draw" and why the chart is drawn once more below.
     *
     * Every later render writes to this object's fields and never replaces it.
     * `refreshMarkers` swaps `events` on whichever object each chart is
     * holding, so handing a chart a second `$wp` would strand its markers on
     * the first one.
     */
    chart.$wp = {
      events: events,
      bucketOf: view.bucketOf,
      titles: view.titles,
    };

    live.add(chart);
    chart.draw();
    return chart;
  }

  /*
   * Bring the chart on `spec`'s canvas up to date with `view`.
   *
   * The update path is the point of the whole arrangement: a period change
   * re-labels and re-fills four live charts instead of destroying them, which
   * is what it takes for the cards not to blank for a frame on every zoom.
   * Building from scratch is left for the canvas that has no chart yet — a
   * first render, or an htmx swap that brought new canvas elements with it.
   */
  function syncChart(spec, view, events) {
    var canvas = document.getElementById(spec.canvasId);
    if (!canvas) {
      return null;
    }

    var chart = Chart.getChart(canvas);
    // Anything that is not already this spec's chart cannot be updated into
    // one: a different plot type, a different number of series, or a chart
    // this file did not build and therefore holds no `$wp` on.
    if (
      !chart ||
      !chart.$wp ||
      chart.config.type !== spec.type ||
      chart.data.datasets.length !== spec.datasets.length
    ) {
      return createChart(canvas, spec, view, events);
    }

    chart.data.labels = view.keys;
    spec.datasets.forEach(function (descriptor, i) {
      var dataset = chart.data.datasets[i];
      dataset.data = view.values[descriptor.source];
      dataset.label = datasetLabel(descriptor, view);
    });
    // Fields, never the object — see `createChart`. Colours are deliberately
    // not rewritten here: they are already whatever the current scheme
    // resolved to, and `applyTheme` owns changing them.
    chart.$wp.events = events;
    chart.$wp.bucketOf = view.bucketOf;
    chart.$wp.titles = view.titles;
    chart.update("none");
    return chart;
  }

  /*
   * Past a quarter of history the x-axis stops being one column per day
   * (`getBucketKind`), which changes what a column is — and on the two charts
   * that plot uniques it changes what the number means, since uniques peak
   * rather than sum. The card's note says so, and says nothing at day zoom.
   */
  var BUCKET_NOTES = { week: "Weekly buckets", month: "Monthly buckets" };

  function cardNote(spec, view) {
    var note = BUCKET_NOTES[view.kind];
    if (!note) {
      return "";
    }
    var plotsUniques = spec.datasets.some(function (descriptor) {
      return descriptor.mode === "max";
    });
    return plotsUniques ? note + " — uniques shown as peak daily" : note;
  }

  /* Fill the card heading's note slot, which the server renders empty. */
  function setCardNote(canvasId, text) {
    var canvas = document.getElementById(canvasId);
    var card = canvas ? canvas.closest(".wp-card") : null;
    var note = card ? card.querySelector(".wp-card-note") : null;
    if (note) {
      note.textContent = text;
    }
  }

  /*
   * The `#chart-data` island the current charts were built from. The
   * `htmx:afterSwap` handler compares against it so that a swap which left the
   * island alone — every event mutation does — does not destroy and rebuild
   * four charts that are already showing the right data.
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

  /* `new URL`, without taking a caller down over a href the parser refuses. */
  function parseUrl(href) {
    if (typeof URL !== "function") {
      return null;
    }
    try {
      return new URL(href, window.location.href);
    } catch (err) {
      return null;
    }
  }

  function periodFromUrl() {
    var url = parseUrl(window.location.href);
    return url ? normalisePeriod(url.searchParams.get("days")) : ALL_DAYS;
  }

  /*
   * The period the page is currently showing.
   *
   * Seeded from the address bar rather than from `#chart-data`, because the
   * sort links this drives outlive the charts: a repo with nothing observed
   * renders no island and no selector, but its popular tables and their links
   * are there either way. The allowlist is the same one the server validates
   * `?days=` against, so a hand-edited value lands on the same period here as
   * it did there.
   */
  var currentDays = periodFromUrl();

  /*
   * How a period is spelled in a query string, in one place. The default is
   * spelled as no parameter at all — the same convention `sort_url` follows
   * server-side, so the address only ever names a period someone picked.
   */
  function applyPeriod(params, days) {
    if (days === ALL_DAYS) {
      params.delete("days");
    } else {
      params.set("days", String(days));
    }
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

  /*
   * Build the repo charts from the `#chart-data` island, if there is one.
   *
   * Answers whether it rendered, which also says whether `applyFilter` has
   * already run this pass — `renderCharts` ends with one, and callers use that
   * instead of filtering the page a second time.
   */
  function initRepoCharts() {
    if (typeof Chart === "undefined") {
      return false;
    }
    var el = document.getElementById("chart-data");
    var payload = readJson("chart-data");
    if (!payload || !Array.isArray(payload.labels) || !payload.labels.length) {
      return false;
    }
    chartPayload = payload;
    chartSource = el;
    renderCharts(payload, normalisePeriod(payload.days));
    return true;
  }

  /*
   * Zoom to `value` days: re-render from the payload already in the page, put
   * the choice in the address bar and hand it to the sort links, so a reload, a
   * shared link or a sort click all stay on the period showing.
   * `replaceState` rather than `pushState` — a zoom is not a navigation, and the
   * back button should leave the page rather than step through every period the
   * reader tried.
   */
  function setPeriod(value) {
    if (!chartPayload) {
      return;
    }
    var days = normalisePeriod(value);
    renderCharts(chartPayload, days);
    currentDays = days;
    syncPeriodUrl(days);
    updateSortLinks(days);
  }

  function syncPeriodUrl(days) {
    if (!window.history || !history.replaceState) {
      return;
    }
    var url = parseUrl(window.location.href);
    if (!url) {
      // A location the URL parser refuses is not worth failing a zoom over.
      return;
    }
    applyPeriod(url.searchParams, days);
    history.replaceState(history.state, "", url.toString());
  }

  /*
   * Re-point every sort link at `days`.
   *
   * The links are rendered with the period the page was requested at, and
   * `hx-replace-url` makes a sort rewrite the whole address bar — so without
   * this, sorting after a zoom would put the old period back and a reload would
   * open on it.
   *
   * `href` and `hx-get` both, then `htmx.process`: htmx reads `hx-get` once,
   * when it wires an element up, and keeps the URL in the click handler's
   * closure. Rewriting the attribute alone would fix the fallback link and
   * change nothing about the request; re-processing is what re-reads it.
   */
  function updateSortLinks(days) {
    var links = document.querySelectorAll("[data-sort-link]");
    for (var i = 0; i < links.length; i++) {
      var link = links[i];
      var url = parseUrl(link.getAttribute("href"));
      if (!url) {
        continue;
      }
      applyPeriod(url.searchParams, days);
      var next = url.pathname + url.search;
      link.setAttribute("href", next);
      link.setAttribute("hx-get", next);
      htmx.process(link);
    }
  }

  /*
   * Everything the four charts plot at the trailing `days` of `payload`, and
   * nothing about the charts themselves.
   *
   * Returns `{keys, titles, bucketOf, kind, uniquesLabel, values}` — axis
   * labels, tooltip headings, the marker plugin's date → column map, the bucket
   * width the window came out at, the name the uniques series goes by at that
   * width, and one rolled-up array per series named in `CHART_SPECS`.
   */
  function computeView(payload, days) {
    var labels = tail(payload.labels, days);
    var source = payload.series || {};
    var kind = getBucketKind(labels);
    var buckets = bucketize(labels, kind);

    // date → column, for the marker plugin. Built from the same buckets the
    // values were aggregated over, so a marker cannot drift from its data.
    var bucketOf = new Map();
    buckets.forEach(function (bucket, idx) {
      bucket.dayIdxs.forEach(function (dayIdx) {
        bucketOf.set(labels[dayIdx], idx);
      });
    });

    var values = {};
    CHART_SPECS.forEach(function (spec) {
      spec.datasets.forEach(function (descriptor) {
        var series = tail(source[descriptor.source], days);
        values[descriptor.source] = buckets.map(function (bucket) {
          return agg(series, bucket.dayIdxs, descriptor.mode);
        });
      });
    });

    return {
      keys: buckets.map(function (b) {
        return b.key;
      }),
      titles: buckets.map(function (b) {
        return b.title;
      }),
      bucketOf: bucketOf,
      kind: kind,
      // At day zoom the uniques bar is that day's unique count; wider buckets
      // cannot sum it (see `agg`), so the label says what the number really is.
      uniquesLabel: kind === "day" ? "Unique" : "Peak daily unique",
      values: values,
    };
  }

  /* Show the trailing `days` of `payload` on the four repo charts. */
  function renderCharts(payload, days) {
    var view = computeView(payload, days);
    // One read for all four charts, and the array each of them goes on holding
    // — `refreshMarkers` swaps it out on every chart at once.
    var events = readJson("events-data") || [];
    CHART_SPECS.forEach(function (spec) {
      syncChart(spec, view, events);
      setCardNote(spec.canvasId, cardNote(spec, view));
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

    var colour = css("--wp-marker-0", "#1f5bb5");
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
  // Confirm dialog
  // -------------------------------------------------------------------------

  /*
   * Fill the shell's `#wp-confirm` with `question` and open it, answering
   * `done(true)` only if the reader pressed Confirm. Returns false — having
   * changed nothing — if the shell is missing a part, so the caller can leave
   * the request to htmx rather than swallow it.
   *
   * `showModal()` is the reason this is a dialog and not a div: the focus trap,
   * the inert background and Escape all come from the platform.
   */
  function openConfirm(dlg, question, elt, done) {
    var textEl = dlg.querySelector("#wp-confirm-text");
    var okBtn = dlg.querySelector("[data-confirm-ok]");
    var cancelBtn = dlg.querySelector("[data-confirm-cancel]");
    if (!textEl || !okBtn || !cancelBtn) {
      return false;
    }

    // The answer, kept here rather than in `dlg.returnValue`: a dialog closed
    // by Escape leaves the previous open's return value in place, so reading it
    // back would confirm a second delete the reader had just escaped out of.
    var confirmed = false;

    function onOk() {
      confirmed = true;
      dlg.close();
    }

    function onCancel() {
      dlg.close();
    }

    /*
     * Every way out lands here — both buttons close the dialog, and so does
     * Escape, whose `cancel` event closes it by default. Resolving on `close`
     * instead of wiring the three paths separately is what keeps the teardown
     * whole: the two click handlers are removed on the one event that cannot be
     * skipped, so the next dialog cannot answer with the last one's callback.
     */
    function onClose() {
      okBtn.removeEventListener("click", onOk);
      cancelBtn.removeEventListener("click", onCancel);
      // Focus goes back to the button that asked. The platform restores it by
      // itself only when that button held focus to begin with, and Safari does
      // not focus a button on click — without this a cancel would drop the
      // reader at the top of the document. `elt` is still on the page: the
      // request this is deciding has not been issued yet.
      if (elt && elt.isConnected && typeof elt.focus === "function") {
        elt.focus();
      }
      done(confirmed);
    }

    textEl.textContent = question;
    okBtn.addEventListener("click", onOk);
    cancelBtn.addEventListener("click", onCancel);
    dlg.addEventListener("close", onClose, { once: true });
    dlg.showModal();
    return true;
  }

  /*
   * Answer `hx-confirm` with the dialog instead of `window.confirm`.
   *
   * htmx fires this before it would prompt, and hands over an `issueRequest` to
   * call once the answer is known — so cancelling the event and resolving it
   * from the dialog is a drop-in replacement htmx never notices. `issueRequest`
   * must be called with `true`: that is what tells htmx the question has
   * already been asked, and without it the native prompt appears after all.
   *
   * Doing nothing is always the safe branch. htmx then runs its own
   * `confirm()`, which is worse-looking but still asks — a page that fails this
   * must not end up deleting without a prompt.
   */
  document.addEventListener("htmx:confirm", function (evt) {
    var detail = evt.detail;
    // Fires for every request htmx makes; only an element carrying an
    // `hx-confirm` brings a question, and the rest must proceed untouched.
    if (!detail || !detail.question) {
      return;
    }
    var dlg = document.getElementById("wp-confirm");
    if (!dlg || typeof dlg.showModal !== "function") {
      return;
    }
    var opened = openConfirm(dlg, detail.question, detail.elt, function (ok) {
      if (ok) {
        detail.issueRequest(true);
      }
    });
    // Only once the dialog is actually up — cancelling the event without
    // anything on screen to answer it would strand the request forever.
    if (opened) {
      evt.preventDefault();
    }
  });

  // -------------------------------------------------------------------------
  // Edit-row Enter
  // -------------------------------------------------------------------------

  /*
   * Enter saves the event row being edited.
   *
   * An edit row is a `<tr>` of inputs, not a form — its Save button collects the
   * row with `hx-include` — so the browser has no implicit submission to offer
   * and Enter would otherwise be swallowed. Delegated on `document` because the
   * rows arrive in swaps.
   */
  document.addEventListener("keydown", function (evt) {
    // Shift+Enter is the newline in the notes field, and an Enter that is
    // closing an IME candidate is not a keypress the reader aimed at the row.
    if (evt.key !== "Enter" || evt.shiftKey || evt.isComposing) {
      return;
    }
    var target = evt.target;
    // Text fields only: this stands in for the submission a form would do, and
    // a focused button already has its own answer to Enter — hijacking that
    // would run Save when the reader meant Cancel. `<textarea>` is not an
    // `<input>`, so Enter in the notes stays a newline.
    if (!target || target.tagName !== "INPUT" || !target.closest) {
      return;
    }
    var row = target.closest("tr.wp-edit-row");
    var save = row && row.querySelector("[data-save]");
    if (!save) {
      return;
    }
    evt.preventDefault();
    save.click();
  });

  // -------------------------------------------------------------------------
  // Focus continuity
  // -------------------------------------------------------------------------

  /*
   * Which element started the request in flight, as an id.
   *
   * htmx already restores focus across a swap, but only for an element that
   * still held it when the response arrived — it reads `document.activeElement`
   * at swap time and re-focuses it by id if the swap took it away. Every
   * mutating control on these pages carries `hx-disabled-elt`, and disabling
   * the focused element blurs it, so by swap time the active element is
   * `<body>` and htmx has nothing to restore. The reader presses Save with the
   * keyboard and is dropped at the top of the document.
   *
   * Recording the id at request start is what survives that blur. Elements
   * htmx never disables — sort links, the fields of an edit row and their caret
   * position — are still htmx's to restore, and this leaves them alone.
   */
  var pendingFocusId = null;

  /*
   * The id of the control the reader actually pressed.
   *
   * Usually that is `elt` itself, but not for a form: the add form and the repo
   * picker carry their own `hx-post`, so htmx reports the `<form>` as the
   * requesting element and the submit button — the thing that was pressed and
   * is about to be disabled — is only findable as whatever holds focus inside
   * it. `htmx:beforeRequest` fires before `hx-disabled-elt` is applied, which is
   * what makes that reading possible at all.
   */
  function pressedId(elt) {
    if (!elt || !elt.contains) {
      return null;
    }
    var active = document.activeElement;
    if (active && active.id && elt.contains(active)) {
      return active.id;
    }
    return elt.id || null;
  }

  /*
   * Whether a request is one the reader started.
   *
   * htmx carries the DOM event that triggered a request through to
   * `requestConfig`, and a poller has none to carry — it calls its handler with
   * the element alone. That is the difference this reads. The settings sync
   * poller fires every 2s against `#sync-status`, and without this guard its
   * request would overwrite the id of a Save the reader is still waiting on:
   * `/settings/discover` is a GitHub round trip, so a poll landing inside one
   * is the likely case rather than the unlucky one.
   *
   * The confirm dialog's re-issued Delete keeps its event (htmx threads the
   * original through `issueRequest`), so answering the prompt still counts as
   * something the reader did.
   */
  function readerStarted(detail) {
    var config = detail ? detail.requestConfig : null;
    return !!(config && config.triggeringEvent);
  }

  document.addEventListener("htmx:beforeRequest", function (evt) {
    if (!readerStarted(evt.detail)) {
      return;
    }
    pendingFocusId = pressedId(evt.detail.elt);
  });

  /* The first thing a reader would type into in a freshly swapped edit row. */
  function editRowField(target) {
    if (!target || !target.matches) {
      return null;
    }
    var row = target.matches("tr.wp-edit-row")
      ? target
      : target.querySelector("tr.wp-edit-row");
    return row ? row.querySelector("input, textarea") : null;
  }

  /*
   * Put focus somewhere sensible once the swap has settled.
   *
   * `afterSettle` rather than `afterSwap` for two reasons, and both are timing.
   * htmx re-enables an `hx-disabled-elt` after the swap, and a disabled button
   * cannot take focus. And an element that keeps its id across a swap wears its
   * *old* attributes until settle — a display row turning into an edit row is
   * still classless `tr#event-row-7` at `afterSwap`, so a `tr.wp-edit-row`
   * selector would find nothing there.
   *
   * Nothing here runs unless focus was actually lost, and an edit row is
   * preferred over the recorded id: when Edit brings one, the caret belongs in
   * its date field rather than back on the button it has just replaced.
   */
  document.addEventListener("htmx:afterSettle", function (evt) {
    // Same guard as the recorder, for the same reason from the other side. A
    // poll settling while a save is in flight would otherwise consume that
    // save's id and leave nothing to restore when its own swap arrives — the
    // whole mechanism engages only for requests the reader started.
    if (!readerStarted(evt.detail)) {
      return;
    }
    var id = pendingFocusId;
    // Consumed either way: the request that recorded it is the one settling
    // here, so a later reader-started swap must not inherit a stale id.
    pendingFocusId = null;

    // Anything other than `<body>` means focus is already somewhere deliberate:
    // htmx restored it — caret position and all, which is how a rejected save
    // leaves the reader in the field they were correcting — or the reader moved
    // on while the request was in flight. Stealing it back would interrupt
    // someone typing.
    var active = document.activeElement;
    if (active && active !== document.body) {
      return;
    }
    var field = editRowField(evt.target);
    if (field) {
      field.focus();
      return;
    }
    if (!id) {
      return;
    }
    var elt = document.getElementById(id);
    if (elt && typeof elt.focus === "function") {
      elt.focus();
      // Whether the focus landed is the test, not whether the element exists: a
      // control can survive the swap and still refuse focus, which is what the
      // Add button does when the disclosure holding it closes on a save.
      if (document.activeElement === elt) {
        return;
      }
    }
    // So the control is gone (a deleted row took its Delete button with it) or
    // cannot hold focus where it now is. The section it acted on is the nearest
    // thing to where the reader was, and carries `tabindex="-1"` to take this.
    var section = document.getElementById("events-section");
    if (section) {
      section.focus();
    }
  });

  // -------------------------------------------------------------------------
  // Wiring
  // -------------------------------------------------------------------------

  function boot() {
    applyTheme();
    initSparklines(document);
    // Charts filter the page themselves as the last step of rendering, so the
    // fallback only runs for a page that has none (or has no Chart.js).
    if (!initRepoCharts()) {
      applyFilter();
    }
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

  /*
   * The kind filter chips, delegated for a stronger version of the same
   * reason: every event mutation replaces `#events-section`, chips and all, so
   * a listener bound to a chip would be thrown away on the next save.
   *
   * `closest` rather than a match on the target, because a click can land on
   * something inside the button.
   */
  document.addEventListener("click", function (evt) {
    var target = evt.target;
    if (!target || !target.closest) {
      return;
    }
    var chip = target.closest(CHIP);
    if (chip) {
      toggleKind(chipKind(chip));
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
    // A sorted table arrives as fresh server markup, so its links carry the
    // period the page was requested at all over again — reapply the zoom to
    // them or the next sort undoes it.
    if (target.id === "refs-table" || target.id === "paths-table") {
      updateSortLinks(currentDays);
    }
    // The swapped markup carries data islands, not scripts, so this is what
    // picks them up. A swap that replaced `#chart-data` needs the charts
    // rebuilt; every other one — an event mutation is the usual case — only
    // needs the markers re-read and the filter pushed back onto the fresh
    // chips, which is a redraw rather than a rebuild.
    var chartEl = document.getElementById("chart-data");
    if (chartEl && chartEl !== chartSource) {
      // Same contract `boot` reads: a false answer means nothing rendered and
      // therefore nothing filtered the page, so the fallback still owes it a
      // pass — an island that arrived unparseable or with no labels would
      // otherwise leave the fresh chips showing every kind.
      if (!initRepoCharts()) {
        applyFilter();
      }
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
