/*
 * Client behaviour for watchpost: chart rendering, event-marker overlays, kind
 * filters, sparklines, theme following and the shared error toast.
 *
 * There is no build step. This file is served as written, so it stays plain
 * ES2017+ with no imports — Chart.js is already on the page as a global by the
 * time this runs (both <script>s are `defer`, which keeps them in order). It is
 * one strict-mode IIFE written with `var` throughout.
 *
 * Listeners are delegated on `document`: an htmx swap replaces the elements it
 * targets, and a listener bound to one of them would leave with it.
 *
 * The page hands data over in JSON islands rather than in generated code:
 *   #chart-data   {days, labels:[YYYY-MM-DD…], series:{stars, views_count, …}}
 *   #events-data  [{id, date, kind, title, url}…]
 *   .spark-data   [Option<i64>…]  (one per dashboard card, keyed by class)
 * Every series is dense — one slot per UTC day in the window — which is what
 * lets a category axis double as a date index. A `null` is a genuine "not
 * observed" gap and is never plotted as zero.
 *
 * `#chart-data` always spans the whole history it covers; `days` is only the
 * period to open on. Zooming is therefore a tail slice of arrays already in the
 * page (`setPeriod`), not a request — the server never re-renders for a period
 * change.
 *
 * A page may ship a subset of that island: the analytics page sends `stars`
 * alone, for the portfolio total, on the same canvas id the repo page uses. A
 * series that is absent rolls up to nulls and a canvas that is absent is
 * skipped, so one code path serves both pages.
 */
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
   * The palette is 6-digit hex; anything else (a resolved Pico variable, say)
   * passes through untinted rather than guessing at its channels.
   */
  function hexToRgba(hex, alpha) {
    var m = /^#([0-9a-f]{6})$/i.exec(hex || "");
    if (!m) {
      return hex;
    }
    var n = parseInt(m[1], 16);
    return (
      "rgba(" + ((n >> 16) & 255) + ", " + ((n >> 8) & 255) + ", " +
      (n & 255) + ", " + alpha + ")"
    );
  }

  /*
   * Scriptable backgroundColor for area fills. Reading `borderColor` at draw
   * time is what keeps the wash in the current scheme: `applyTheme` rewrites
   * borderColor and the next resolve rebuilds the gradient from it.
   */
  function areaGradient(context) {
    var chart = context.chart;
    var area = chart.chartArea;
    if (!area) {
      return "rgba(0, 0, 0, 0)";
    }
    var dataset = context.dataset || chart.data.datasets[context.datasetIndex];
    var gradient = chart.ctx.createLinearGradient(0, area.top, 0, area.bottom);
    gradient.addColorStop(0, hexToRgba(dataset.borderColor, 0.18));
    gradient.addColorStop(1, hexToRgba(dataset.borderColor, 0));
    return gradient;
  }

  /*
   * Scriptable backgroundColor for bars. Slightly translucent at rest so the
   * hover state has somewhere to go, and fainter for a bucket only partly
   * observed (`chart.$wp.partial`, from `bucketCoverage`), so a three-day week
   * beside seven-day ones reads as incomplete rather than as a slump. Read off
   * borderColor at draw time for the reason `areaGradient` is: `applyTheme`
   * rewrites borderColor, and the next resolve follows it.
   */
  function barFill(context) {
    var chart = context.chart;
    var dataset = context.dataset || chart.data.datasets[context.datasetIndex];
    var partial = chart.$wp && chart.$wp.partial;
    var faded =
      !!partial &&
      context.dataIndex !== undefined &&
      !!partial[context.dataIndex];
    return hexToRgba(dataset.borderColor, faded ? 0.4 : 0.82);
  }

  var MONTHS = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun",
    "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
  ];

  /*
   * Bucket keys are "YYYY-MM-DD" (day, and a week's Monday) or "YYYY-MM"
   * (month); dispatch on shape, not chart state — ticks render during
   * construction, before `$wp` exists. Week ticks read as their Monday
   * ("May 18"); the tooltip title still says "Week of 2026-05-18".
   */
  function shortTick(label) {
    var day = /^(\d{4})-(\d{2})-(\d{2})$/.exec(label);
    if (day) {
      return MONTHS[Number(day[2]) - 1] + " " + Number(day[3]);
    }
    var month = /^(\d{4})-(\d{2})$/.exec(label);
    if (month) {
      return MONTHS[Number(month[2]) - 1] + " " + month[1];
    }
    return label;
  }

  var COMPACT =
    typeof Intl !== "undefined" && Intl.NumberFormat
      ? new Intl.NumberFormat("en", {
          notation: "compact",
          maximumFractionDigits: 1,
        })
      : null;

  /* Y ticks: "12.3K" past five digits, the plain number below. */
  function compactTick(value) {
    if (typeof value !== "number") {
      return value;
    }
    return COMPACT && Math.abs(value) >= 10000
      ? COMPACT.format(value)
      : String(value);
  }

  function chromeColors() {
    return {
      grid: css("--wp-chart-grid", "#eff1f4"),
      tick: css("--wp-chart-tick", "#6b7280"),
    };
  }

  /*
   * Chart.js resolves these once from options, never from CSS — a scheme flip
   * has to write the re-read values back onto every live repo chart.
   */
  function applyChartChrome(chart) {
    var c = chromeColors();
    chart.options.scales.x.ticks.color = c.tick;
    chart.options.scales.y.ticks.color = c.tick;
    chart.options.scales.y.grid.color = c.grid;
    chart.options.plugins.legend.labels.color = c.tick;
    // The tooltip is an HTML element now (`externalTooltip`); it takes its
    // colours from the stylesheet and needs nothing written here.
  }

  /*
   * Legend boxes draw the dataset backgroundColor; for an area dataset that is
   * a chart-area gradient, which clamps to near-transparent above the plot.
   * Force solid dots in the series colour instead.
   */
  function solidLegendLabels(chart) {
    var items = Chart.defaults.plugins.legend.labels.generateLabels(chart);
    items.forEach(function (item) {
      var dataset = chart.data.datasets[item.datasetIndex];
      if (dataset) {
        item.fillStyle = dataset.borderColor;
        item.strokeStyle = dataset.borderColor;
        item.lineWidth = 0;
      }
    });
    // Declaration order, not paint order: `order` puts the bars behind the
    // uniques line, and without this the legend would read "Unique, Views".
    items.sort(function (a, b) {
      return a.datasetIndex - b.datasetIndex;
    });
    return items;
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
    // `boot()` runs this before any chart exists, so the page font is in the
    // defaults before the first construction.
    Chart.defaults.font.family = css(
      "--pico-font-family",
      Chart.defaults.font.family,
    );
    var card = css("--pico-card-background-color", "#ffffff");
    live.forEach(function (chart) {
      if (!chart.canvas) {
        return;
      }
      // Dataset colours were resolved to literal values at init, so re-reading
      // the variable is the only thing that recolours them. An area fill is
      // the exception: its backgroundColor is a scriptable gradient that
      // re-reads borderColor on every draw, and overwriting it with a literal
      // would freeze it in the old scheme.
      chart.data.datasets.forEach(function (dataset) {
        if (!dataset.$wpVar) {
          return;
        }
        var colour = css(dataset.$wpVar, "#888888");
        dataset.borderColor = colour;
        if (dataset.$wpBar) {
          // A bar's rest fill is scriptable (`barFill`) and re-reads
          // borderColor on every draw, like an area wash; overwriting it with
          // a literal would freeze it and drop the partial-bucket fade. Only
          // the solid hover fill was resolved at build time.
          dataset.hoverBackgroundColor = colour;
          return;
        }
        if (!dataset.$wpArea) {
          dataset.backgroundColor = colour;
        }
        dataset.pointBackgroundColor = colour;
        dataset.pointHoverBackgroundColor = colour;
        dataset.pointHoverBorderColor = card;
      });
      if (chart.$wp) {
        applyChartChrome(chart); // sparklines have no scales/tooltip to write
      }
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
   * How many days a bucket spans on the calendar: 7 for a week, the month's
   * own length for a month, and null at day zoom, where a bucket is one day
   * and cannot be partly observed.
   */
  function calendarSpan(key, kind) {
    if (kind === "week") {
      return 7;
    }
    if (kind === "month") {
      var m = /^(\d{4})-(\d{2})$/.exec(key);
      // Day 0 of the next month is the last day of this one.
      return m
        ? new Date(Date.UTC(Number(m[1]), Number(m[2]), 0)).getUTCDate()
        : null;
    }
    return null;
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

  /*
   * How many of a bucket's days `values` observed. A null is a day watchpost
   * did not see; an observed zero counts, because it is a day of data.
   */
  function countObserved(values, idxs) {
    var n = 0;
    for (var i = 0; i < idxs.length; i++) {
      var v = values[idxs[i]];
      if (v !== null && v !== undefined) {
        n++;
      }
    }
    return n;
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
    // A filter over a list that hides its older rows would hide matches with
    // them, so every chip press shows the whole list first: a filtered view is
    // never silently partial.
    showAllEvents();
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
   * Event markers sit in a lane of their own, `LANE_PX` tall, directly above
   * the plot and under the legend when there is one; the `wpLane` axis in
   * `createChart` is what reserves it. Drawn inside the plot, a dot sat on the
   * top gridline and ran into any series near its maximum. Layout padding was
   * the rejected alternative: Chart.js puts it outside the legend, so on the
   * two-series charts the dots would have landed on "Views ● Unique" rather
   * than above the data.
   *
   * `HIT_PX` is how near the pointer has to be to a dot's column, in pixels,
   * inside that lane. Wider than the dot: a marker is a mark on a canvas with
   * no DOM node behind it, so this slack is the entire hit area, and 5px asked
   * for a precision a trackpad does not have. The lane is the whole hit zone;
   * the plot below it belongs to the data.
   *
   * There is no marker tip. A marker's events are listed in the chart tooltip,
   * under the figures of the column they fall in (`externalTooltip`), so
   * hovering an event day never trades the numbers for the event. A second
   * tip that took over near a marker was the rejected arrangement: on an event
   * day it hid the figures for the whole height of the column.
   *
   * Markers are a mouse enhancement, not a way to reach an event. There is
   * nothing here to focus and nothing to announce — the events table under the
   * charts lists the same events as real rows, with the real links, and that is
   * the accessible equivalent this widget defers to.
   */
  var LANE_PX = 18;
  var HIT_PX = 8;

  /*
   * Place an already-visible tip beside a page-coordinate point, flipped away
   * from the viewport edges.
   *
   * Measurements have to happen after the fill and the unhide: a hidden
   * element reports zero for `offsetWidth`/`offsetHeight`, so a tip measured
   * any earlier would decide it fits everywhere.
   */
  function placeTip(tip, pageX, pageY) {
    var x = pageX + 14;
    var maxX = window.scrollX + document.documentElement.clientWidth - 8;
    if (x + tip.offsetWidth > maxX) {
      x = Math.max(window.scrollX + 8, pageX - tip.offsetWidth - 14);
    }
    // Written before the height is read, not with the `top` below. The tip is
    // absolutely positioned with an automatic width, so the room between its
    // `left` and the page edge is what decides where its text wraps and
    // therefore how tall it is; measured while the previous tip's `left` was
    // still on the element, the height answered for a box of a different width.
    tip.style.left = x + "px";

    // The same flip vertically. Without it a column near the foot of the
    // window opened its tip below the fold — the tip is positioned in page
    // coordinates, so nothing scrolls it back into view. Flipping above the
    // cursor keeps it beside the column it belongs to; the `Math.max` pins a
    // tip taller than the viewport to the top edge, losing its last line rather
    // than its first.
    var y = pageY + 14;
    var maxY = window.scrollY + document.documentElement.clientHeight - 8;
    if (y + tip.offsetHeight > maxY) {
      y = Math.max(window.scrollY + 8, pageY - tip.offsetHeight - 14);
    }
    tip.style.top = y + "px";
  }

  /*
   * Scroll an event's table row into view and flash it, so a click on an
   * event's column answers "which event is this?" without the reader hunting.
   */
  function focusRow(id) {
    var row = document.getElementById("event-row-" + id);
    if (!row) {
      return;
    }
    // An older event sits in the collapsed part of the list (REPO-05); open
    // it first, or the scroll lands on a row that is not drawn.
    revealRow(row);
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

  /*
   * The events whose dots are under the pointer, which has to be in the lane.
   * The old zone ran the column's full height plus 8px either side, so on an
   * event day the bar itself was a marker target, and hovering it traded the
   * figures for the event.
   */
  function hitsAt(chart, x, y) {
    var area = chart.chartArea;
    if (!area || y < area.top - LANE_PX || y >= area.top) {
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

  /*
   * The events showing in column `idx`, in `#events-data` order (newest
   * first). Read through the same `bucketOf` map the dots are placed with, so
   * the tooltip can never list an event the lane does not mark, or miss one.
   */
  function eventsInBucket(chart, idx) {
    var wp = chart.$wp;
    if (!wp || idx === undefined || idx < 0) {
      return [];
    }
    return wp.events.filter(function (ev) {
      return !isHidden(ev.kind) && wp.bucketOf.get(ev.date) === idx;
    });
  }

  /* The column under a pointer inside the plot, or -1 anywhere else. */
  function plotColumnAt(chart, x, y) {
    var area = chart.chartArea;
    var scale = chart.scales.x;
    if (
      !area ||
      !scale ||
      x < area.left ||
      x > area.right ||
      y < area.top ||
      y > area.bottom
    ) {
      return -1;
    }
    var idx = scale.getValueForPixel(x);
    return idx >= 0 && idx < chart.data.labels.length ? idx : -1;
  }

  /*
   * Point the tooltip at the column of the hovered lane marker, or clear it
   * when the pointer is in the lane between markers.
   *
   * Chart.js picks the hovered column only inside the plot. Outside it, it
   * keeps whatever was active last, so a pointer sliding along the lane went on
   * describing the column it left the plot from. Hidden datasets (a legend
   * click hides one) are left out, as Chart.js leaves them out itself.
   */
  function pointTooltipAt(chart, hits, e) {
    var idx = hits.length ? chart.$wp.bucketOf.get(hits[0].date) : undefined;
    var active = [];
    if (idx !== undefined) {
      chart.data.datasets.forEach(function (_dataset, i) {
        if (chart.isDatasetVisible(i)) {
          active.push({ datasetIndex: i, index: idx });
        }
      });
    }
    chart.setActiveElements(active);
    chart.tooltip.setActiveElements(active, { x: e.x, y: e.y });
  }

  /*
   * Remember which marker column the pointer is in, and repaint only when the
   * set of hit events changes — keying on the ids rather than the pixel keeps
   * a pointer sliding along one column from redrawing the chart per event.
   */
  function setHover(chart, x, key) {
    if (chart.$wp.hoverKey === key) {
      return;
    }
    chart.$wp.hoverKey = key;
    chart.$wp.hoverX = x;
    chart.draw();
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
      var ring = css("--pico-card-background-color", "#ffffff");
      var hoverX = chart.$wp ? chart.$wp.hoverX : null;
      ctx.save();
      placed.forEach(function (item) {
        if (!isFinite(item.x)) {
          return;
        }
        var colour = kindColor(item.event.kind);
        // The drop line is on demand: only the column whose dot is under the
        // pointer draws one, so a chart with a busy month rests as a row of
        // dots instead of a fence through the data. Solid — a dash pattern is
        // noise, and the dashes used to be the loudest thing on the plot.
        if (
          hoverX !== null &&
          hoverX !== undefined &&
          Math.abs(item.x - hoverX) <= HIT_PX
        ) {
          ctx.strokeStyle = hexToRgba(colour, 0.5);
          ctx.lineWidth = 1;
          ctx.beginPath();
          ctx.moveTo(item.x, area.top - LANE_PX / 2);
          ctx.lineTo(item.x, area.bottom);
          ctx.stroke();
        }
        // The dot is the marker's whole resting presence and the hover
        // target's advertisement, centred in the lane above the plot. Full
        // colour, ringed in the card surface so it separates from a
        // neighbouring dot.
        ctx.beginPath();
        ctx.arc(item.x, area.top - LANE_PX / 2, 3.5, 0, Math.PI * 2);
        ctx.fillStyle = colour;
        ctx.fill();
        ctx.lineWidth = 2;
        ctx.strokeStyle = ring;
        ctx.stroke();
      });
      ctx.restore();
    },

    /*
     * Runs after Chart.js's own tooltip plugin has handled the same event:
     * registered plugins are notified before a chart's inline ones, which is
     * what lets the lane override the active column below.
     */
    afterEvent: function (chart, args) {
      var e = args.event;
      if (!e || !chart.$wp) {
        return;
      }
      if (e.type === "mouseout") {
        chart.canvas.style.cursor = "";
        setHover(chart, null, "");
        return;
      }
      if (e.type !== "mousemove" && e.type !== "click") {
        return;
      }
      var area = chart.chartArea;
      var inLane = !!area && e.y >= area.top - LANE_PX && e.y < area.top;
      var hits = hitsAt(chart, e.x, e.y);
      // In the plot the whole column is the target: its events are listed
      // under its figures, so a click anywhere in it can jump to them.
      var events = hits.length
        ? hits
        : eventsInBucket(chart, plotColumnAt(chart, e.x, e.y));
      if (e.type === "click") {
        if (events.length) {
          focusRow(events[0].id);
        }
        return;
      }
      if (inLane) {
        pointTooltipAt(chart, hits, e);
        args.changed = true;
      }
      chart.canvas.style.cursor = events.length ? "pointer" : "";
      setHover(
        chart,
        hits.length ? e.x : null,
        hits
          .map(function (ev) {
            return ev.id;
          })
          .join(","),
      );
    },
  };

  // -------------------------------------------------------------------------
  // Chart tooltip and crosshair
  // -------------------------------------------------------------------------

  /*
   * The chart tooltip is an HTML element styled by app.css, not the built-in
   * canvas drawing. Chart.js is told `enabled: false` and still decides what
   * is hovered — it hands this handler the rows and a caret position, and only
   * the rendering is ours. That is what lets the tip wear the page's own
   * card face, which no canvas tooltip option can quite reproduce.
   *
   * It is the chart's only tip. A column's events are listed in it under the
   * column's figures (see `LANE_PX` for the marker tip it replaced).
   */
  var chartTipEl = null;

  function chartTip() {
    if (chartTipEl && chartTipEl.isConnected) {
      return chartTipEl;
    }
    chartTipEl = document.getElementById("chart-tip");
    if (!chartTipEl) {
      chartTipEl = document.createElement("div");
      chartTipEl.id = "chart-tip";
      // On the body rather than inside the chart's box: the box clips, so a
      // tip anchored in it would be cut off. Absolute positioning against the
      // initial containing block means page coordinates place it.
      document.body.appendChild(chartTipEl);
    }
    return chartTipEl;
  }

  function hideChartTip() {
    if (chartTipEl) {
      chartTipEl.classList.remove("wp-visible");
    }
  }

  /*
   * One line for an event in the tooltip: its kind chip and its title, led by
   * its date when the column is wider than that day. Built node by node with
   * `textContent` — event titles and kinds are user input, and this is where
   * they reach the DOM. No link and no host line: the tip is
   * `pointer-events: none`, so a link in it could never be clicked, and the
   * row a click on the column jumps to carries the real one.
   */
  function eventLine(ev, withDate) {
    var line = document.createElement("div");
    line.className = "wp-tip-event";
    if (withDate) {
      var when = document.createElement("span");
      when.className = "wp-muted wp-small";
      when.textContent = shortTick(ev.date);
      line.appendChild(when);
    }
    if (ev.kind) {
      var kind = document.createElement("span");
      kind.className = "wp-chip wp-kind-" + kindSlot(ev.kind);
      kind.textContent = ev.kind;
      line.appendChild(kind);
    }
    var title = document.createElement("span");
    title.className = "wp-tip-event-title";
    title.textContent = ev.title;
    line.appendChild(title);
    return line;
  }

  /*
   * Built node by node with `textContent`: labels here are watchpost's own
   * strings, but event titles are not, and one discipline for everything that
   * reaches the tip is cheaper than remembering which strings are trusted.
   */
  function externalTooltip(context) {
    var model = context.tooltip;
    if (!model || model.opacity === 0 || !(model.dataPoints || []).length) {
      hideChartTip();
      return;
    }

    var chart = context.chart;
    var index = model.dataPoints[0].dataIndex;
    var tip = chartTip();
    tip.textContent = "";

    var title = document.createElement("div");
    title.className = "wp-tip-title";
    // The bucket heading the period change rewrites, read off the chart, not
    // a captured array. It carries the "N of 7 days observed" note for a
    // partly observed bucket (`bucketCoverage`).
    title.textContent = chart.$wp.titles[index] || "";
    tip.appendChild(title);

    model.dataPoints.forEach(function (point) {
      var row = document.createElement("div");
      row.className = "wp-tip-row";

      var swatch = document.createElement("span");
      swatch.className = "wp-tip-swatch";
      // CSSOM assignment, which the CSP allows; a style attribute it would
      // not. The dataset backgroundColor may be a gradient or a scriptable
      // fill — borderColor is the solid series colour.
      swatch.style.backgroundColor = point.dataset.borderColor;
      row.appendChild(swatch);

      // Value before label: the reader hovering a column already knows the
      // series and wants the number. Chart.js formats a null as "0"; the
      // point is correctly absent and the tip must not invent a zero.
      var missing = point.raw === null || point.raw === undefined;
      var value = document.createElement("strong");
      value.className = missing ? "wp-tip-value wp-muted" : "wp-tip-value";
      value.textContent = missing ? "not observed" : point.formattedValue;
      row.appendChild(value);

      if (point.dataset.label) {
        var label = document.createElement("span");
        label.className = "wp-tip-label";
        label.textContent = point.dataset.label;
        row.appendChild(label);
      }

      tip.appendChild(row);
    });

    // What happened in this column, under its figures, in one card. A day
    // column's events are on that day, so only a wider bucket names the date.
    var events = eventsInBucket(chart, index);
    if (events.length) {
      var rule = document.createElement("div");
      rule.className = "wp-tip-sep";
      tip.appendChild(rule);
      var key = chart.data.labels[index];
      events.forEach(function (ev) {
        tip.appendChild(eventLine(ev, ev.date !== key));
      });
    }

    tip.classList.add("wp-visible");
    var rect = chart.canvas.getBoundingClientRect();
    placeTip(
      tip,
      rect.left + window.scrollX + model.caretX,
      rect.top + window.scrollY + model.caretY
    );
  }

  /*
   * One solid hairline at the hovered column, the affordance that tells the
   * reader which bucket the tip is describing. Solid and greyed on purpose:
   * a dash pattern here would read as another event marker.
   */
  var wpCrosshair = {
    id: "wpCrosshair",

    afterDatasetsDraw: function (chart) {
      var active = chart.tooltip && chart.tooltip.getActiveElements();
      if (!active || !active.length) {
        return;
      }
      var area = chart.chartArea;
      var x = active[0].element.x;
      if (!area || !isFinite(x) || x < area.left || x > area.right) {
        return;
      }
      var ctx = chart.ctx;
      ctx.save();
      // `globalAlpha` rather than an rgba rewrite: the tick colour comes from
      // a Pico variable and is not guaranteed to be six-digit hex.
      ctx.globalAlpha = 0.35;
      ctx.strokeStyle = css("--wp-chart-tick", "#6b7280");
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x, area.top);
      ctx.lineTo(x, area.bottom);
      ctx.stroke();
      ctx.restore();
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
   * How big a marker one point gets: visible only where the run of observed
   * points around it is too short to read as a line.
   *
   * `spanGaps: false` strokes a segment between two adjacent observed values
   * and nowhere else, so the first days of a series draw next to nothing while
   * the axis already scales to them. A repo whose release assets were first
   * read two days ago is one observed bucket at weekly zoom — no segment at all
   * — and two neighbouring columns at daily zoom, which is a couple of pixels
   * against the right edge. Both cases read as an empty chart under correct
   * numbers.
   *
   * A run of three or more is a line and needs no help, so every point in one
   * stays at 0: a marker per day turns a line into a caterpillar.
   *
   * `pointStyle: false` cannot do this job — Chart.js reads it as "draw an
   * empty path" whatever the radius says. A radius of 0 is what suppresses a
   * point.
   */
  function strandedPointRadius(ctx) {
    var data = ctx.dataset.data;
    var i = ctx.dataIndex;
    var seen = function (j) {
      return data[j] !== null && data[j] !== undefined;
    };
    if (!seen(i)) {
      return 0;
    }
    // The three ways this point can sit in a run of three: two behind, one
    // either side, or two ahead.
    var inLine =
      (seen(i - 1) && (seen(i - 2) || seen(i + 1))) ||
      (seen(i + 1) && seen(i + 2));
    return inLine ? 0 : 3;
  }

  /*
   * The line shape every dataset wears, copied onto a fresh object per chart
   * by `buildDataset`. Hovering pops a 4px dot ringed in the card surface —
   * the ring is what keeps it legible where it lands on the line itself.
   */
  var LINE_STYLE = {
    borderWidth: 2,
    tension: 0,
    borderJoinStyle: "round",
    borderCapStyle: "round",
    pointRadius: strandedPointRadius,
    pointHoverRadius: 4,
    pointHoverBorderWidth: 2,
  };

  /*
   * The repo charts, as data.
   *
   * Each `source` is a field of `ChartSeries` and each `canvasId` one of its
   * `cards()` (src/routes/html/repo.rs). Rename either side alone and the
   * chart comes up empty with no error.
   *
   * These are descriptors and nothing here is ever written to. Chart.js owns
   * the objects it is handed — it stores the dataset object itself and
   * `applyTheme` writes resolved colours onto it — so `buildDataset` copies
   * what it needs onto a fresh object per chart, and one shared style constant
   * cannot end up wearing every chart's colours in turn.
   *
   * The policies that used to be restated per chart live here once:
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
   *   - `style: "bar"` marks a per-bucket count, which plots as columns:
   *     ninety spiky daily points in a card-width line chart read as
   *     scribble, where the same numbers as bars read as what they are —
   *     discrete daily counts. Cumulative series stay lines.
   *   - `area` marks the chart's primary line series, which carries a soft
   *     gradient wash under it; a secondary series (uniques) stays a bare
   *     line so two washes never muddy each other.
   *   - `order` puts the bars behind the uniques line: Chart.js draws the
   *     higher order first.
   *   - `cssVar` is one accent, `--wp-marker-0`, for every primary series
   *     whatever it measures: the pressed tile already names the metric. A hue
   *     per metric made the page change colour on every tile click, borrowed
   *     seven of the eight slots the event-kind hash colours markers with
   *     (reddit dots and the Downloads line were the same red), and spent red
   *     and green, which mean down and up in the deltas. One borrowed slot is
   *     left: `kindSlot` still hashes over all eight, so a kind that lands on
   *     slot 0 (youtube, twitter, launch) wears the series blue. Its dots
   *     stay apart by place rather than hue, in their own lane above the
   *     plot. Freeing slot 0 needs a ninth token or a seven-slot hash, and
   *     the hash would have to change in `kind_class` too.
   *   - `secondary` marks the companion line (uniques beside its count). It
   *     is muted ink and thinner, so the legend tells the two apart by weight
   *     and tone rather than by a second hue.
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
          cssVar: "--wp-marker-0",
          area: true,
        },
      ],
    },
    {
      canvasId: "chart_views",
      type: "line",
      zeroBased: true,
      datasets: [
        {
          source: "views_count",
          label: "Views",
          mode: "sum",
          cssVar: "--wp-marker-0",
          style: "bar",
          order: 2,
        },
        {
          source: "views_uniques",
          // The uniques series is called something else once a bucket is wider
          // than a day, so its label comes from the view.
          labelKey: "uniquesLabel",
          mode: "max",
          cssVar: "--wp-chart-tick",
          secondary: true,
          order: 1,
        },
      ],
    },
    {
      canvasId: "chart_clones",
      type: "line",
      zeroBased: true,
      datasets: [
        {
          source: "clones_count",
          label: "Clones",
          mode: "sum",
          cssVar: "--wp-marker-0",
          style: "bar",
          order: 2,
        },
        {
          source: "clones_uniques",
          labelKey: "uniquesLabel",
          mode: "max",
          cssVar: "--wp-chart-tick",
          secondary: true,
          order: 1,
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
          cssVar: "--wp-marker-0",
          area: true,
        },
      ],
    },
    {
      canvasId: "chart_pulls",
      type: "line",
      zeroBased: false,
      datasets: [
        {
          source: "pulls_total",
          label: "Container pulls",
          mode: "last",
          cssVar: "--wp-marker-0",
          area: true,
        },
      ],
    },
  ];

  function datasetLabel(descriptor, view) {
    return descriptor.labelKey ? view[descriptor.labelKey] : descriptor.label;
  }

  /*
   * Tooltip headings and fade flags for one chart, from how much of each
   * bucket its counted series observed.
   *
   * A week or month bucket sums (or peaks) only the days it saw. A first week
   * that began on a Sunday, or the week in progress, plots one to three days
   * beside seven-day neighbours and reads as a slump. The values stay exactly
   * as aggregated; this only says so, in the heading ("Week of 2026-07-27 ·
   * 1 of 7 days observed") and through `barFill`. The chart's first sum or
   * max series is the one counted; a 'last' series is a carried level, whole
   * on any day it has. A bucket that observed nothing is left alone, because
   * its rows already read "not observed" and "0 of 7 days" would say it twice.
   * Day zoom has no spans, so nothing there changes.
   */
  function bucketCoverage(spec, view) {
    var counted = null;
    spec.datasets.forEach(function (descriptor) {
      if (!counted && descriptor.mode !== "last") {
        counted = descriptor;
      }
    });
    var seen = counted ? view.observed[counted.source] : null;
    var partial = view.titles.map(function (_title, i) {
      var span = view.spans[i];
      return !!(seen && span && seen[i] > 0 && seen[i] < span);
    });
    return {
      partial: partial,
      titles: view.titles.map(function (title, i) {
        return partial[i]
          ? title + " · " + seen[i] + " of " + view.spans[i] + " days observed"
          : title;
      }),
    };
  }

  function buildDataset(descriptor, view) {
    return descriptor.style === "bar"
      ? buildBarDataset(descriptor, view)
      : buildLineDataset(descriptor, view);
  }

  function buildLineDataset(descriptor, view) {
    var colour = css(descriptor.cssVar, "#888888");
    var dataset = {
      label: datasetLabel(descriptor, view),
      data: view.values[descriptor.source],
      // The variable name travels with the dataset because the colour above is
      // a resolved literal: `applyTheme` re-reads `$wpVar` on a scheme flip,
      // and that is the only thing that recolours a line already drawn.
      $wpVar: descriptor.cssVar,
      // Marks the gradient fill so `applyTheme` leaves the scriptable
      // backgroundColor alone — it re-reads borderColor per draw by itself.
      $wpArea: !!descriptor.area,
      order: descriptor.order,
      borderColor: colour,
      backgroundColor: descriptor.area ? areaGradient : colour,
      // Points would otherwise inherit the gradient as their fill.
      pointBackgroundColor: colour,
      pointHoverBackgroundColor: colour,
      pointHoverBorderColor: css("--pico-card-background-color", "#ffffff"),
      fill: descriptor.area ? "origin" : false,
      // A null is a day watchpost did not observe, not a zero. Bridging it
      // would draw a straight line through missing data and read as a real
      // measurement.
      spanGaps: false,
    };
    Object.assign(dataset, LINE_STYLE);
    if (descriptor.secondary) {
      // After LINE_STYLE, which sets the 2px every primary line wears.
      dataset.borderWidth = 1.5;
    }
    return dataset;
  }

  /*
   * A per-bucket count as columns. A null bucket simply has no bar — the gap
   * discipline costs nothing here — and the external tooltip still answers
   * "not observed" for it.
   */
  function buildBarDataset(descriptor, view) {
    var colour = css(descriptor.cssVar, "#888888");
    return {
      type: "bar",
      label: datasetLabel(descriptor, view),
      data: view.values[descriptor.source],
      $wpVar: descriptor.cssVar,
      // The flag `applyTheme` dispatches on: a bar's hover fill is a literal
      // colour, so a scheme flip must rewrite it, while its rest fill
      // (`barFill`) follows borderColor by itself.
      $wpBar: true,
      order: descriptor.order,
      // Translucent at rest and fainter for a partly observed bucket
      // (`barFill`). `borderColor` stays the solid series colour: the legend,
      // the tooltip swatch and `barFill` read it, and bars draw no stroke of
      // their own.
      backgroundColor: barFill,
      hoverBackgroundColor: colour,
      borderColor: colour,
      borderWidth: 0,
      // Rounded at the data end, square on the baseline.
      borderRadius: { topLeft: 4, topRight: 4 },
      borderSkipped: "bottom",
      // A cap, not a width: one observed bucket in a wide plot must stay a
      // mark, not a block the width of the chart.
      maxBarThickness: 24,
    };
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
            border: { display: false },
            // Horizontal ticks only — a tilted date is harder to read than a
            // sparser axis. The padding is what buys the sparseness: a bare
            // `autoSkip` packs date labels shoulder to shoulder in a
            // card-width chart and they run together.
            ticks: {
              maxRotation: 0,
              autoSkip: true,
              autoSkipPadding: 16,
              font: { size: 11 },
              callback: function (value) {
                // Category axis hands the index; the label is looked up.
                return shortTick(this.getLabelForValue(value));
              },
            },
          },
          y: {
            beginAtZero: spec.zeroBased,
            grid: { drawTicks: false },
            border: { display: false },
            ticks: {
              precision: 0,
              maxTicksLimit: 5,
              font: { size: 11 },
              callback: compactTick,
            },
          },
          // The event markers' lane (see `LANE_PX`): an empty axis box, which
          // Chart.js lays out between the legend and the plot, the one place
          // a box can go there. Nothing on it draws, and no dataset binds to
          // it: datasets take the first x scale, `x`, which is why this one
          // is declared after it.
          wpLane: {
            type: "category",
            position: "top",
            grid: { display: false },
            border: { display: false },
            ticks: { display: false },
            afterFit: function (scale) {
              scale.height = LANE_PX;
            },
          },
        },
        plugins: {
          // A legend earns its space only where there are two series to tell
          // apart.
          legend: {
            display: spec.datasets.length > 1,
            align: "end",
            labels: {
              usePointStyle: true,
              pointStyle: "circle",
              boxWidth: 6,
              boxHeight: 6,
              font: { size: 11 },
              generateLabels: solidLegendLabels,
            },
          },
          tooltip: {
            enabled: false,
            external: externalTooltip,
            // Rows in declaration order, as the legend reads them (see
            // `solidLegendLabels`). Chart.js otherwise orders tooltip items by
            // dataset `order`, the paint order that puts the bars behind the
            // uniques line, and the tip read "Unique" above "Views".
            itemSort: function (a, b) {
              return a.datasetIndex - b.datasetIndex;
            },
          },
        },
      },
      plugins: [eventMarkers, wpCrosshair],
    });

    /*
     * What this file keeps on a chart beyond what Chart.js knows about: the
     * events to mark, the date → column map that places them, the tooltip
     * headings, and which buckets were only partly observed (`barFill`).
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
    var coverage = bucketCoverage(spec, view);
    chart.$wp = {
      events: events,
      bucketOf: view.bucketOf,
      titles: coverage.titles,
      partial: coverage.partial,
    };

    live.add(chart);
    // The constructor's synchronous render ran before `$wp` existed and
    // before the chrome colours below; this update paints markers and puts
    // the CSS-derived greys onto ticks, grid and tooltip in one pass.
    applyChartChrome(chart);
    chart.update("none");
    return chart;
  }

  /*
   * Bring the chart on `spec`'s canvas up to date with `view`.
   *
   * The update path is the point of the whole arrangement: a period change
   * re-labels and re-fills the live charts instead of destroying them, which
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
    // one: a different plot type, a different number of series, a series
    // whose bar/line shape differs from its descriptor's, or a chart this
    // file did not build and therefore holds no `$wp` on.
    if (
      !chart ||
      !chart.$wp ||
      chart.config.type !== spec.type ||
      chart.data.datasets.length !== spec.datasets.length ||
      spec.datasets.some(function (descriptor, i) {
        var built = chart.data.datasets[i].type === "bar";
        return built !== (descriptor.style === "bar");
      })
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
    var coverage = bucketCoverage(spec, view);
    chart.$wp.events = events;
    chart.$wp.bucketOf = view.bucketOf;
    chart.$wp.titles = coverage.titles;
    chart.$wp.partial = coverage.partial;
    chart.update("none");
    return chart;
  }

  /*
   * The `#chart-data` island the current charts were built from. The
   * `htmx:afterSwap` handler compares against it so that a swap which left the
   * island alone — every event mutation does — does not destroy and rebuild
   * charts that are already showing the right data.
   */
  var chartSource = null;

  /* The whole-history payload the charts zoom over, and the period showing. */
  var chartPayload = null;

  var ALL_DAYS = -1;

  /*
   * The period allowlist. Mirrors `PERIODS` in src/routes/html/period.rs, which
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
   * Build the charts from the `#chart-data` island, if there is one.
   *
   * Named for the repo page because that is where it started; the analytics
   * page ships the same island shape on one of the same canvas ids, which is
   * what lets one function serve both. `CHART_SPECS` names seven series across
   * five canvases, and a page that ships fewer of either gets exactly the ones
   * it has: `syncChart` skips a canvas it cannot find, and `computeView` rolls
   * a series that is not in the payload up to nulls.
   *
   * Answers whether it rendered, which also says whether `applyFilter` has
   * already run this pass — `renderCharts` ends with one, and callers use that
   * instead of filtering the page a second time.
   *
   * It is also where the period first reaches the links to other pages
   * (`updatePeriodLinks`). `boot` calls it on every page, with or without a
   * chart, which makes it the one boot hook this section owns.
   */
  function initRepoCharts() {
    // First, and whatever follows: a page with no chart (a repo with nothing
    // observed yet, or no Chart.js) can still have arrived on a `?days=`, and
    // its links should pass that on.
    updatePeriodLinks(currentDays);
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
   * the choice in the address bar and hand it to the sort links and the links
   * to other pages, so a reload, a shared link, a sort click or a hop to
   * Analytics or another repo all stay on the period showing.
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
    updatePeriodLinks(days);
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
   * Carry `days` into the links that open another page with a period: the
   * leaderboard and Recent changes rows on Analytics, the nav's Analytics
   * link, and anything marked `data-period-link` (the repo header's crumb,
   * switcher and previous/next). Without it the period lived only in each
   * page's own address, and every hop between Analytics and a repo opened
   * on All again.
   *
   * The destination decides, not the selector. `/analytics` and
   * `/repos/{id}` take a period; anything else a selector catches — the crumb
   * back to `/repos`, which has none, or Settings in the nav — stays as
   * rendered. The spelling is `applyPeriod`'s, so All is no parameter at
   * all, as in the address bar. Only `href`: none of these is an htmx
   * request, so unlike `updateSortLinks` there is nothing for `htmx.process`
   * to re-read.
   *
   * Remembering the period across visits (localStorage) was left out on
   * purpose: a stored choice would change what a bare URL opens on, which is
   * the owner's call. With JavaScript off these links stay as the server
   * rendered them, and each page opens on its default.
   */
  var PERIOD_LINKS =
    "a[data-period-link], .wp-leaders a[href], .wp-changes a[href], nav a[href]";

  function takesPeriod(url) {
    return (
      url.origin === window.location.origin &&
      (url.pathname === "/analytics" || /^\/repos\/\d+$/.test(url.pathname))
    );
  }

  function updatePeriodLinks(days) {
    var links = document.querySelectorAll(PERIOD_LINKS);
    for (var i = 0; i < links.length; i++) {
      var link = links[i];
      var url = parseUrl(link.getAttribute("href"));
      if (!url || !takesPeriod(url)) {
        continue;
      }
      applyPeriod(url.searchParams, days);
      link.setAttribute("href", url.pathname + url.search + url.hash);
    }
  }

  /*
   * Everything the charts plot at the trailing `days` of `payload`, and
   * nothing about the charts themselves.
   *
   * Returns `{keys, titles, bucketOf, kind, spans, uniquesLabel, values,
   * observed}` — axis labels, tooltip headings, the marker plugin's date →
   * column map, the bucket width the window came out at, each bucket's
   * calendar length (null at day zoom), the name the uniques series goes by
   * at that width, one rolled-up array per series named in `CHART_SPECS`, and
   * per series how many days of each bucket it observed.
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
    var observed = {};
    CHART_SPECS.forEach(function (spec) {
      spec.datasets.forEach(function (descriptor) {
        var series = tail(source[descriptor.source], days);
        values[descriptor.source] = buckets.map(function (bucket) {
          return agg(series, bucket.dayIdxs, descriptor.mode);
        });
        observed[descriptor.source] = buckets.map(function (bucket) {
          return countObserved(series, bucket.dayIdxs);
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
      spans: buckets.map(function (b) {
        return calendarSpan(b.key, kind);
      }),
      // At day zoom the uniques point is that day's unique count; wider
      // buckets cannot sum it (see `agg`), so the label says what the number
      // really is.
      uniquesLabel: kind === "day" ? "Unique" : "Peak daily unique",
      values: values,
      observed: observed,
    };
  }

  /*
   * Show the figure that belongs to the period on screen.
   *
   * The analytics leaderboard renders every period's number and hides all but
   * one, rather than having the client compute them — see `period_cell` in
   * src/routes/html/analytics.rs for why. This is the whole client-side half: a
   * `hidden` flip, no text written and nothing parsed.
   *
   * Called from `renderCharts` so a first render and every later zoom share one
   * call site — put it in `setPeriod` instead and a `?days=30` page load would
   * zoom the chart while leaving the table on "All". A page with no
   * `[data-period-value]` — every page but this one — loops zero times.
   */
  function updatePeriodValues(days) {
    var cells = document.querySelectorAll("[data-period-value]");
    for (var i = 0; i < cells.length; i++) {
      cells[i].hidden =
        normalisePeriod(cells[i].getAttribute("data-period-value")) !== days;
    }
  }

  /*
   * Show the hero panel for `id` and press its tile.
   *
   * An `aria-pressed` and `hidden` flip and nothing else — every panel's
   * chart already holds the full payload, so switching metrics costs no
   * request and no re-render. The one wrinkle is size: a chart built while
   * its panel was `hidden` measured a zero-height box, so the reveal is when
   * it learns its real one.
   */
  function selectKpi(id) {
    var tiles = document.querySelectorAll("[data-kpi-tile]");
    for (var i = 0; i < tiles.length; i++) {
      tiles[i].setAttribute(
        "aria-pressed",
        String(tiles[i].getAttribute("data-kpi-tile") === id),
      );
    }
    var panels = document.querySelectorAll("[data-kpi-panel]");
    for (var j = 0; j < panels.length; j++) {
      panels[j].hidden = panels[j].getAttribute("data-kpi-panel") !== id;
    }
    var canvas = document.getElementById(id);
    var chart = canvas && typeof Chart !== "undefined" && Chart.getChart(canvas);
    if (chart) {
      chart.resize();
      chart.update("none");
    }
  }

  document.addEventListener("click", function (evt) {
    var target = evt.target;
    if (!(target instanceof Element)) {
      return;
    }
    var tile = target.closest("[data-kpi-tile]");
    if (tile) {
      selectKpi(tile.getAttribute("data-kpi-tile"));
    }
  });

  /* Show the trailing `days` of `payload` on the charts. */
  function renderCharts(payload, days) {
    var view = computeView(payload, days);
    // One read for all charts, and the array each of them goes on holding
    // — `refreshMarkers` swaps it out on every chart at once.
    var events = readJson("events-data") || [];
    CHART_SPECS.forEach(function (spec) {
      syncChart(spec, view, events);
    });
    updatePeriodValues(days);
    applyFilter();
  }

  /*
   * Re-read `#events-data` after an event was added, edited or deleted.
   *
   * Deliberately not a re-init: destroying and rebuilding every chart to move
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
              $wpArea: true,
              borderColor: colour,
              backgroundColor: areaGradient,
              fill: "origin",
              borderWidth: 1.5,
              borderJoinStyle: "round",
              borderCapStyle: "round",
              // The big charts' rule: a reading with no neighbours draws a
              // dot, where a radius of 0 left a repo read once as a 1px
              // speck. A run of three or more stays a bare line. Filled with
              // the line colour, not the area gradient, which is nearly
              // transparent at a dot's height; `applyTheme` recolours it.
              pointRadius: strandedPointRadius,
              pointBackgroundColor: colour,
              pointBorderWidth: 0,
              pointHoverRadius: 0,
              tension: 0,
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
   * The dialog's resting copy. A trigger may override the heading and the OK
   * label for one prompt; both are put back on close, so the next plain
   * `hx-confirm` reads exactly as the shell ships it.
   */
  var CONFIRM_DEFAULT = "Confirm";

  /*
   * What the triggering element asks the dialog to say: `data-confirm-title`
   * for the heading, `data-confirm-label` for the OK button, and the bare
   * `data-confirm-danger` for a destructive action. These are read as
   * attributes and written as `textContent`, the same rule the toast keeps:
   * a title that came from a stored event can never become markup. The OK
   * button's own marker is `data-confirm-ok`, which is why the trigger's label
   * attribute is not called that.
   */
  function confirmCopy(elt) {
    function read(name) {
      var value = elt && elt.getAttribute ? elt.getAttribute(name) : null;
      return value && value.trim() ? value : null;
    }
    return {
      title: read("data-confirm-title") || CONFIRM_DEFAULT,
      label: read("data-confirm-label") || CONFIRM_DEFAULT,
      danger: !!(elt && elt.hasAttribute && elt.hasAttribute("data-confirm-danger")),
    };
  }

  /*
   * Fill the shell's `#wp-confirm` with `question` and open it, answering
   * `done(true)` only if the reader pressed OK. Returns false — having
   * changed nothing — if the shell is missing a part, so the caller can leave
   * the request to htmx rather than swallow it.
   *
   * `showModal()` is the reason this is a dialog and not a div: the focus trap,
   * the inert background and Escape all come from the platform.
   */
  function openConfirm(dlg, question, elt, done) {
    var titleEl = dlg.querySelector("#wp-confirm-title");
    var textEl = dlg.querySelector("#wp-confirm-text");
    var okBtn = dlg.querySelector("[data-confirm-ok]");
    var cancelBtn = dlg.querySelector("[data-confirm-cancel]");
    if (!titleEl || !textEl || !okBtn || !cancelBtn) {
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
     * skipped, so the next dialog cannot answer with the last one's callback,
     * and the copy goes back to the shell's own.
     */
    function onClose() {
      okBtn.removeEventListener("click", onOk);
      cancelBtn.removeEventListener("click", onCancel);
      titleEl.textContent = CONFIRM_DEFAULT;
      okBtn.textContent = CONFIRM_DEFAULT;
      okBtn.classList.remove("wp-confirm-danger");
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

    var copy = confirmCopy(elt);
    titleEl.textContent = copy.title;
    okBtn.textContent = copy.label;
    if (copy.danger) {
      okBtn.classList.add("wp-confirm-danger");
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
  // Repo page: row disclosure and switcher
  // -------------------------------------------------------------------------

  /*
   * Long tables open on their first rows. The server renders every row, marks
   * the ones past the cut `wp-more-row`, and gives the table `data-more` plus a
   * toggle that ships `hidden`. Collapsing is a class on the table, set here,
   * so with JavaScript off nothing collapses and nothing offers to. It is a
   * class rather than `hidden` on the rows because the events table's
   * `row.hidden` belongs to `applyFilter`.
   *
   * Which tables the reader opened is kept by id. A sort swaps the whole
   * table, and the fresh one would otherwise close again under the pointer. A
   * Set rather than a flag on the element, because the element is what the
   * swap throws away.
   *
   * The events table is the exception, on purpose. A mutation re-renders the
   * section and the list closes again, unless the row just saved is one of the
   * older ones (closing would hide what the reader just changed) or a kind
   * filter is on (a filtered view must never be silently partial).
   */
  var EVENTS_TABLE = "wp-events-table";
  var openTables = new Set();

  /*
   * The row a Save is about to re-render, by id. Recorded at request start,
   * because the section swap that answers it replaces the button that knew.
   */
  var savedRowId = null;

  function setMore(table, open) {
    table.classList.toggle("wp-collapsed", !open);
    if (open) {
      openTables.add(table.id);
    } else {
      openTables.delete(table.id);
    }
    var toggle = table.querySelector("[data-more-toggle]");
    if (toggle) {
      toggle.hidden = false;
      toggle.setAttribute("aria-expanded", String(open));
      // Attribute text the server wrote; `textContent` keeps it text.
      toggle.textContent = toggle.getAttribute(
        open ? "data-more-hide" : "data-more-show",
      );
    }
  }

  /* Collapse every disclosure table in `root`, or `root` itself, unless the reader opened it. */
  function initMore(root) {
    var tables =
      root.matches && root.matches("table[data-more]")
        ? [root]
        : root.querySelectorAll("table[data-more]");
    for (var i = 0; i < tables.length; i++) {
      setMore(tables[i], openTables.has(tables[i].id));
    }
  }

  /* Open the events list, if it has older rows to show. */
  function showAllEvents() {
    var table = document.getElementById(EVENTS_TABLE);
    if (table && table.hasAttribute("data-more")) {
      setMore(table, true);
    }
  }

  /*
   * Open the table holding `row` when the row is one it collapsed. For code
   * that jumps to a row from outside the table: a chart marker's click has to
   * reveal an older event before it can scroll to it.
   */
  function revealRow(row) {
    var table = row && row.closest ? row.closest("table[data-more]") : null;
    if (
      table &&
      row.classList.contains("wp-more-row") &&
      table.classList.contains("wp-collapsed")
    ) {
      setMore(table, true);
    }
  }

  /*
   * The repo switcher is a Pico `details.dropdown`. Pico closes it on an
   * outside click with a full-screen overlay under the menu, but anything
   * stacked above that overlay still takes the click itself. Closing every
   * open menu the click was not inside covers those.
   */
  function closeMenus(except) {
    var menus = document.querySelectorAll("details.dropdown[open]");
    for (var i = 0; i < menus.length; i++) {
      if (menus[i] !== except) {
        menus[i].open = false;
      }
    }
  }

  document.addEventListener("click", function (evt) {
    var target = evt.target;
    if (!target || !target.closest) {
      return;
    }
    closeMenus(target.closest("details.dropdown"));
    var toggle = target.closest("[data-more-toggle]");
    var table = toggle ? toggle.closest("table[data-more]") : null;
    if (table) {
      setMore(table, table.classList.contains("wp-collapsed"));
    }
  });

  /*
   * Escape closes an open switcher; Pico does nothing with it. Focus goes back
   * to the summary when it was inside the menu, so the keyboard is not left on
   * a link that just disappeared.
   */
  document.addEventListener("keydown", function (evt) {
    if (evt.key !== "Escape") {
      return;
    }
    var menu = document.querySelector("details.dropdown[open]");
    if (!menu) {
      return;
    }
    var hadFocus = menu.contains(document.activeElement);
    menu.open = false;
    var summary = menu.querySelector("summary");
    if (hadFocus && summary) {
      summary.focus();
    }
  });

  document.addEventListener("htmx:beforeRequest", function (evt) {
    var elt = evt.detail ? evt.detail.elt : null;
    var row =
      elt && elt.hasAttribute && elt.hasAttribute("data-save") && elt.closest
        ? elt.closest("tr")
        : null;
    savedRowId = row ? row.id : null;
  });

  /*
   * After settle, not after swap: for the swap htmx dresses the new element in
   * the old one's `class` and puts the server's back at settle, so a class set
   * any earlier would be wiped. The settled element is the event's target.
   * This listener is registered before the focus-continuity one, so a list
   * that has to open is open before focus is put back into it.
   */
  document.addEventListener("htmx:afterSettle", function (evt) {
    var target = evt.target;
    if (!target) {
      return;
    }
    if (target.id === "refs-table" || target.id === "paths-table") {
      initMore(target);
    } else if (target.id === "events-section") {
      var saved = savedRowId ? document.getElementById(savedRowId) : null;
      savedRowId = null;
      openTables.delete(EVENTS_TABLE);
      if (
        (saved && saved.classList.contains("wp-more-row")) ||
        hiddenKinds.size > 0
      ) {
        openTables.add(EVENTS_TABLE);
      }
      initMore(target);
    }
  });

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", function () {
      initMore(document);
    });
  } else {
    initMore(document);
  }

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
    // cannot hold focus where it now is. The nearest container the swap
    // settled into that takes parked focus (`tabindex="-1"`: the events
    // section, a sync status panel) is the closest thing to where the reader
    // was. `#main` carries one too, for the skip link, and is skipped: landing
    // there is the same as being dropped at the top.
    var settled = evt.target;
    var holder =
      settled && settled.closest
        ? settled.closest('[tabindex="-1"]:not(#main)')
        : null;
    if (holder && holder.isConnected) {
      holder.focus();
      if (document.activeElement === holder) {
        return;
      }
    }
    // The events section is the last resort for a swap that settled outside
    // any such container, as a row swap inside it does not.
    var section = document.getElementById("events-section");
    if (section) {
      section.focus();
    }
  });

  // -------------------------------------------------------------------------
  // Announcements
  // -------------------------------------------------------------------------

  /*
   * Speak a confirmation that arrived inside a swap.
   *
   * `#wp-live` (rendered by `base`, outside every swap target) is the page's
   * one polite live region. Text marked `data-announce` in a settled swap is
   * copied into it, because a status node inserted together with its text is
   * the one thing a live region cannot announce reliably.
   *
   * Only a change is written: a poll re-renders "Syncing…" every 2s, and
   * writing it each time would read it out every 2s. A request the reader
   * started clears the region and the memory first, so a second "Event
   * added." in a row is still a change and is still heard. `boot` seeds the
   * memory with the text the page loaded with, so a page load says nothing.
   */
  var lastAnnounced = null;

  function announcement(root) {
    if (!root || !root.querySelector) {
      return "";
    }
    var el =
      root.matches && root.matches("[data-announce]")
        ? root
        : root.querySelector("[data-announce]");
    return el ? el.textContent.replace(/\s+/g, " ").trim() : "";
  }

  document.addEventListener("htmx:beforeRequest", function (evt) {
    if (!readerStarted(evt.detail)) {
      return;
    }
    lastAnnounced = null;
    var live = document.getElementById("wp-live");
    if (live) {
      live.textContent = "";
    }
  });

  document.addEventListener("htmx:afterSettle", function (evt) {
    var live = document.getElementById("wp-live");
    var text = announcement(evt.target);
    if (!live || !text || text === lastAnnounced) {
      return;
    }
    lastAnnounced = text;
    live.textContent = text;
  });

  // -------------------------------------------------------------------------
  // Wiring
  // -------------------------------------------------------------------------

  function boot() {
    lastAnnounced = announcement(document);
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
    selectKpi: selectKpi,
    refreshMarkers: refreshMarkers,
    toggleKind: toggleKind,
    initSparklines: initSparklines,
    applyTheme: applyTheme,
    showToast: showToast,
    hideToast: hideToast,
  };
})();
