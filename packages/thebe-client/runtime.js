/**
 * thebe-client runtime — Milestone 4
 *
 * Responsibilities:
 *  1. `getProps()` — reads the SSR props JSON and wraps it in a deep reactive
 *     Proxy.  Mutations trigger immediate DOM patches via hydration anchors.
 *  2. `__thebe_register(name, fn)` — registers event-handler functions so the
 *     DOM event wiring below can call them by name.
 *  3. Auto-wires `on*="fnName"` and `on*="fnName(this.value)"` attributes
 *     after the DOM is ready.
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

  function _readMemberPath(root, path) {
    var current = root;
    var parts = path.split(".");
    var i;

    for (i = 0; i < parts.length; i++) {
      if (current == null) {
        return undefined;
      }
      current = current[parts[i]];
    }

    return current;
  }

  function _splitHandlerArgs(source) {
    var args = [];
    var current = "";
    var quote = null;
    var escaped = false;
    var i;
    var ch;

    for (i = 0; i < source.length; i++) {
      ch = source.charAt(i);

      if (quote) {
        current += ch;
        if (escaped) {
          escaped = false;
        } else if (ch === "\\") {
          escaped = true;
        } else if (ch === quote) {
          quote = null;
        }
        continue;
      }

      if (ch === '"' || ch === "'" || ch === "`") {
        quote = ch;
        current += ch;
        continue;
      }

      if (ch === ",") {
        args.push(current.trim());
        current = "";
        continue;
      }

      current += ch;
    }

    if (current.trim()) {
      args.push(current.trim());
    }

    return args;
  }

  function _parseHandlerExpression(expression) {
    var trimmed = (expression || "").trim();
    var openParen;
    var closeParen;
    var name;

    if (!trimmed) {
      return null;
    }

    openParen = trimmed.indexOf("(");
    if (openParen === -1) {
      if (!/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(trimmed)) {
        return null;
      }
      return { name: trimmed, args: null };
    }

    closeParen = trimmed.lastIndexOf(")");
    if (closeParen !== trimmed.length - 1) {
      return null;
    }

    name = trimmed.slice(0, openParen).trim();
    if (!/^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name)) {
      return null;
    }

    return {
      name: name,
      args: _splitHandlerArgs(trimmed.slice(openParen + 1, closeParen))
    };
  }

  function _resolveHandlerArg(expression, event, el) {
    var trimmed = expression.trim();
    var lastChar;

    if (!trimmed) {
      return undefined;
    }

    if (trimmed === "event") {
      return event;
    }

    if (trimmed === "this") {
      return el;
    }

    if (trimmed.indexOf("this.") === 0) {
      return _readMemberPath(el, trimmed.slice(5));
    }

    if (trimmed.indexOf("event.") === 0) {
      return _readMemberPath(event, trimmed.slice(6));
    }

    if (trimmed === "true") {
      return true;
    }

    if (trimmed === "false") {
      return false;
    }

    if (trimmed === "null") {
      return null;
    }

    if (trimmed === "undefined") {
      return undefined;
    }

    if (/^-?\d+(\.\d+)?$/.test(trimmed)) {
      return Number(trimmed);
    }

    lastChar = trimmed.charAt(trimmed.length - 1);
    if (
      trimmed.length >= 2 &&
      ((trimmed.charAt(0) === '"' && lastChar === '"') ||
        (trimmed.charAt(0) === "'" && lastChar === "'"))
    ) {
      return trimmed.slice(1, -1);
    }

    return undefined;
  }

  function _invokeHandler(spec, event, el) {
    var fn = _handlers[spec.name];
    var args;
    var i;

    if (typeof fn !== "function") {
      return;
    }

    if (spec.args === null) {
      fn.call(el, event);
      return;
    }

    args = [];
    for (i = 0; i < spec.args.length; i++) {
      args.push(_resolveHandlerArg(spec.args[i], event, el));
    }
    fn.apply(el, args);
  }

  /** Wire `on*="fnName"` attributes to the handlers registry. */
  function _wireEvents() {
    var els = win.document.querySelectorAll("*");
    for (var i = 0; i < els.length; i++) {
      /* jshint loopfunc: true */
      (function (el) {
        var attrs = [];
        var j;
        var attr;
        var spec;
        var eventName;

        for (j = 0; j < el.attributes.length; j++) {
          attrs.push({
            name: el.attributes[j].name,
            value: el.attributes[j].value
          });
        }

        for (j = 0; j < attrs.length; j++) {
          attr = attrs[j];
          if (attr.name.slice(0, 2).toLowerCase() !== "on" || attr.name.length <= 2) {
            continue;
          }

          spec = _parseHandlerExpression(attr.value);
          if (!spec) {
            continue;
          }

          eventName = attr.name.slice(2).toLowerCase();
          (function (boundAttrName, boundEventName, boundSpec) {
            el.removeAttribute(boundAttrName);
            el.addEventListener(boundEventName, function (event) {
              _invokeHandler(boundSpec, event, el);
            });
          })(attr.name, eventName, spec);
        }
      })(els[i]);
    }
  }

  function _isExecutableScript(script) {
    var type = (script.getAttribute("type") || "").trim().toLowerCase();
    return (
      !type ||
      type === "text/javascript" ||
      type === "application/javascript" ||
      type === "module"
    );
  }

  function _syncManagedHead(parsedDoc) {
    var selector = "[data-thebe-head]";
    var current = win.document.head.querySelectorAll(selector);
    var incoming = parsedDoc.head.querySelectorAll(selector);
    var i;

    for (i = 0; i < current.length; i++) {
      current[i].remove();
    }

    for (i = 0; i < incoming.length; i++) {
      win.document.head.appendChild(incoming[i].cloneNode(true));
    }
  }

  function _scrollToNavigationTarget(url) {
    if (!url.hash) {
      win.scrollTo(0, 0);
      return;
    }

    var target = win.document.getElementById(
      decodeURIComponent(url.hash.slice(1))
    );
    if (target) {
      target.scrollIntoView();
      return;
    }

    win.scrollTo(0, 0);
  }

  function _resolveUrl(href) {
    try {
      return new win.URL(href, win.location.href);
    } catch (_) {
      return null;
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
      var type;
      // Skip the runtime bootstrap — already in scope.
      if (
        src.id === "__thebe_props" ||
        !_isExecutableScript(src) ||
        src.textContent.indexOf("__thebe_runtime") !== -1
      ) {
        continue;
      }
      var live = win.document.createElement("script");
      for (var j = 0; j < src.attributes.length; j++) {
        live.setAttribute(src.attributes[j].name, src.attributes[j].value);
      }
      if (src.src) {
        live.src = src.src;
        win.document.body.appendChild(live);
        continue;
      }
      type = (live.type || "").trim().toLowerCase();
      // Wrap in an IIFE so each navigation gets a fresh scope and `let`/`const`
      // declarations from the previous page cannot conflict with the new page.
      live.textContent =
        type === "module"
          ? src.textContent
          : "(function(){\n" + src.textContent + "\n})();";
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
  function _navigate(url, push) {
    var requestPath = url.pathname + url.search;
    var historyPath = requestPath + url.hash;

    win
      .fetch(requestPath, { headers: { Accept: "text/html" } })
      .then(function (r) {
        return r.text();
      })
      .then(function (html) {
        var parser = new win.DOMParser();
        var doc = parser.parseFromString(html, "text/html");

        _syncManagedHead(doc);

        // Swap body content.
        win.document.body.innerHTML = doc.body.innerHTML;

        // Update history and title.
        if (push !== false) {
          win.history.pushState({}, doc.title || "", historyPath);
        }
        win.document.title = doc.title;

        _scrollToNavigationTarget(url);

        // Reset handler registry for the new page.
        _handlers = {};

        // Re-evaluate the new page's inline scripts (user script + props).
        _evalScripts(doc.body);

        // Re-wire managed DOM event attributes emitted by codegen.
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
      var href;
      var url;

      if (
        e.defaultPrevented ||
        e.button !== 0 ||
        e.metaKey ||
        e.ctrlKey ||
        e.shiftKey ||
        e.altKey ||
        !e.target ||
        typeof e.target.closest !== "function"
      ) {
        return;
      }

      var a = e.target.closest("a[href]");
      if (!a) {
        return;
      }

      href = a.getAttribute("href");
      url = _resolveUrl(href);

      // Skip: invalid, cross-origin, non-http(s), same-document hash, or opted-out.
      if (
        !url ||
        url.origin !== win.location.origin ||
        (url.protocol !== "http:" && url.protocol !== "https:") ||
        a.hasAttribute("data-thebe-reload") ||
        a.hasAttribute("download") ||
        (a.target && a.target.toLowerCase() !== "_self") ||
        (url.pathname === win.location.pathname &&
          url.search === win.location.search &&
          url.hash)
      ) {
        return;
      }

      e.preventDefault();
      _navigate(url, true);
    });

    // Handle back/forward buttons.
    win.addEventListener("popstate", function () {
      _navigate(new win.URL(win.location.href), false);
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
