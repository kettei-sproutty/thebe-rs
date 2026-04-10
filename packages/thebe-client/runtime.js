/**
 * thebe-client runtime — Milestone 3
 *
 * Responsibilities:
 *  1. `getProps()` — reads the SSR props JSON and wraps it in a deep reactive
 *     Proxy.  Mutations are local-only in M3; DOM patching via hydration
 *     markers arrives in M4.
 *  2. `__thebe_register(name, fn)` — registers event-handler functions so the
 *     onclick wiring below can call them by name.
 *  3. Auto-wires `onclick="fnName"` attributes after the DOM is ready.
 */
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
   * Read the SSR-injected Props JSON and return a deep reactive Proxy.
   *
   * In M3, mutations update the in-memory object only.
   * M4 will wire Proxy `set` traps to DOM hydration markers.
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
    return _makeReactive(raw);
  }

  function _makeReactive(obj) {
    if (typeof obj !== "object" || obj === null) return obj;
    // Recurse into nested objects.
    for (var k in obj) {
      if (Object.prototype.hasOwnProperty.call(obj, k) &&
          typeof obj[k] === "object" && obj[k] !== null) {
        obj[k] = _makeReactive(obj[k]);
      }
    }
    return new Proxy(obj, {
      set: function (target, key, value) {
        target[key] = typeof value === "object" && value !== null
          ? _makeReactive(value)
          : value;
        // TODO(M4): notify hydration markers bound to `key`.
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

  // Attach event wirer after DOM is ready (safe even if already parsed).
  if (win.document.readyState === "loading") {
    win.document.addEventListener("DOMContentLoaded", _wireEvents);
  } else {
    _wireEvents();
  }

  // Expose public API on `window`.
  win.getProps = getProps;
  win.__thebe_register = __thebe_register;
})(window);
