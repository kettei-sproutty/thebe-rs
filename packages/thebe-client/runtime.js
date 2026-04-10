/**
 * thebe-client runtime — Milestone 4
 *
 * Responsibilities:
 *  1. `getProps()` — reads the SSR props JSON and wraps it in a deep reactive
 *     Proxy.  Mutations trigger immediate DOM patches via hydration anchors.
 *  2. `__thebe_register(name, fn)` — registers event-handler functions so the
 *     onclick wiring below can call them by name.
 *  3. Auto-wires `onclick="fnName"` attributes after the DOM is ready.
 *  4. `_updateDOM(key, value)` — finds all anchors for `key` and patches
 *     the bound text nodes or data-attribute elements.
 */
/* __thebe_runtime */
(function (win) {
  "use strict";

  /** Registry filled by `__thebe_register` calls in the user script. */
  var _handlers = {};

  /**
   * Register a named event handler.
   * Called by the synthesised registration code injected into the user script.
   */
  function __thebe_register(name, fn) {
    _handlers[name] = fn;
  }

  /**
   * Patch all DOM nodes bound to `key` with the stringified `value`.
   *
   * Two anchor strategies are supported — the codegen picks the right one
   * based on the DOM context detected at build time:
   *
   *  1. **Comment markers** (safe contexts — phrasing content, divs, …):
   *     `<!--thebe:key-->TEXT<!--/thebe:key-->`
   *     The text node between the pair is updated in place.
   *
   *  2. **Data-attribute spans** (unsafe contexts — table cells, select, …
   *     where browsers hoist comment nodes out of the structure):
   *     `<span data-thebe-bind="key">TEXT</span>`
   *     The element's `textContent` is replaced.
   */
  function _updateDOM(key, value) {
    var text = String(value);

    // Strategy 1 — comment markers (safe contexts).
    var startMarker = "thebe:" + key;
    var walker = win.document.createTreeWalker(
      win.document.body,
      NodeFilter.SHOW_COMMENT,
      null
    );
    var node;
    while ((node = walker.nextNode())) {
      if (node.nodeValue.trim() === startMarker) {
        var sibling = node.nextSibling;
        if (sibling && sibling.nodeType === Node.TEXT_NODE) {
          sibling.data = text;
        } else {
          // No text node between markers — insert one.
          var textNode = win.document.createTextNode(text);
          node.parentNode.insertBefore(textNode, sibling);
        }
      }
    }

    // Strategy 2 — data-thebe-bind (unsafe / table contexts).
    // Escape `key` for safe use in an attribute selector.
    var safeKey = key.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
    var bound = win.document.querySelectorAll(
      '[data-thebe-bind="' + safeKey + '"]'
    );
    for (var i = 0; i < bound.length; i++) {
      bound[i].textContent = text;
    }
  }

  /**
   * Read the SSR-injected Props JSON and return a deep reactive Proxy.
   *
   * Every mutation on the returned object (or any nested object) immediately
   * patches the corresponding DOM hydration anchors via `_updateDOM`.
   *
   * The `path` parameter is used internally for nested Proxies to reconstruct
   * the full dot-separated key (e.g. `"user.name"`).
   */
  function getProps() {
    var el = win.document.getElementById("__thebe_props");
    if (!el) return {};
    var raw;
    try {
      raw = JSON.parse(el.textContent || "{}");
    } catch (_) {
      raw = {};
    }
    return _makeReactive(raw, "");
  }

  function _makeReactive(obj, path) {
    if (typeof obj !== "object" || obj === null) return obj;
    // Recurse into nested objects, threading the key path for notifications.
    for (var k in obj) {
      if (Object.prototype.hasOwnProperty.call(obj, k) &&
          typeof obj[k] === "object" && obj[k] !== null) {
        obj[k] = _makeReactive(obj[k], path ? path + "." + k : k);
      }
    }
    return new Proxy(obj, {
      set: function (target, key, value) {
        var fullKey = path ? path + "." + key : key;
        target[key] = typeof value === "object" && value !== null
          ? _makeReactive(value, fullKey)
          : value;
        _updateDOM(fullKey, value);
        return true;
      }
    });
  }

  /** Wire `onclick="fnName"` attributes to the handlers registry. */
  function _wireEvents() {
    var els = win.document.querySelectorAll("[onclick]");
    for (var i = 0; i < els.length; i++) {
      /* jshint loopfunc: true */
      (function (el) {
        var fn = el.getAttribute("onclick");
        el.removeAttribute("onclick");
        el.addEventListener("click", function () {
          if (typeof _handlers[fn] === "function") {
            _handlers[fn]();
          }
        });
      })(els[i]);
    }
  }

  /**
   * Re-evaluate all inline `<script>` elements found in a parsed document
   * body.  Scripts injected via `innerHTML` are inert; this clones them into
   * live `<script>` nodes so the browser evaluates them.
   *
   * The thebe runtime script itself is identified by the sentinel comment
   * `/* __thebe_runtime *\/` at the top of its content and is skipped — it is
   * already running and must not be re-initialised.
   */
  function _evalScripts(parsedBody) {
    var scripts = parsedBody.querySelectorAll("script");
    for (var i = 0; i < scripts.length; i++) {
      var src = scripts[i];
      // Skip the runtime bootstrap — already in scope.
      if (src.textContent.indexOf("__thebe_runtime") !== -1) {
        continue;
      }
      var live = win.document.createElement("script");
      if (src.type) {
        live.type = src.type;
      }
      // Wrap in an IIFE so each navigation gets a fresh scope and `let`/`const`
      // declarations from the previous page cannot conflict with the new page.
      live.textContent = "(function(){\n" + src.textContent + "\n})();";
      win.document.body.appendChild(live);
    }
  }

  /**
   * Perform a client-side navigation to `href`.
   *
   * Fetches the full server-rendered HTML, swaps the document body, pushes a
   * history entry (unless `push` is false, e.g. on popstate), scrolls to the
   * top, and re-runs the new page's inline scripts + event wiring.
   */
  function _navigate(href, push) {
    win
      .fetch(href, { headers: { Accept: "text/html" } })
      .then(function (r) {
        return r.text();
      })
      .then(function (html) {
        var parser = new win.DOMParser();
        var doc = parser.parseFromString(html, "text/html");

        // Swap body content.
        win.document.body.innerHTML = doc.body.innerHTML;

        // Update history and title.
        if (push !== false) {
          win.history.pushState({}, doc.title || "", href);
        }
        if (doc.title) {
          win.document.title = doc.title;
        }

        win.scrollTo(0, 0);

        // Reset handler registry for the new page.
        _handlers = {};

        // Re-evaluate the new page's inline scripts (user script + props).
        _evalScripts(doc.body);

        // Re-wire onclick attributes emitted by codegen.
        _wireEvents();
      })
      .catch(function () {
        // On network error fall back to a full navigation.
        win.location.href = href;
      });
  }

  /**
   * Attach the client-side router.
   *
   * Uses event delegation on `document` — a single listener handles all
   * current and future anchor elements.  Links can opt out of client routing
   * with `data-thebe-reload`.
   */
  function _initRouter() {
    win.document.addEventListener("click", function (e) {
      var a = e.target.closest("a[href]");
      if (!a) {
        return;
      }
      var href = a.getAttribute("href");
      // Skip: empty, external, protocol-relative, hash-only, or opted-out.
      if (
        !href ||
        href.indexOf("://") !== -1 ||
        href.startsWith("//") ||
        href.startsWith("#") ||
        a.hasAttribute("data-thebe-reload")
      ) {
        return;
      }
      e.preventDefault();
      _navigate(href, true);
    });

    // Handle back/forward buttons.
    win.addEventListener("popstate", function () {
      _navigate(win.location.pathname + win.location.search, false);
    });
  }

  // Attach event wirer after DOM is ready (safe even if already parsed).
  if (win.document.readyState === "loading") {
    win.document.addEventListener("DOMContentLoaded", function () {
      _wireEvents();
      _initRouter();
    });
  } else {
    _wireEvents();
    _initRouter();
  }

  // Expose public API on `window`.
  win.getProps = getProps;
  win.__thebe_register = __thebe_register;
})(window);
