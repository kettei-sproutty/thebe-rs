import { createAsync, query } from "@solidjs/router";
import { createEffect, createSignal, onMount } from "solid-js";

import { getCount } from "../lib/counter";

const getCounter = query(async () => {
  "use server";
  return getCount();
}, "counter");

export const route = {
  preload: () => getCounter(),
};

if (
  typeof performance !== "undefined" &&
  performance.getEntriesByName("framework-bench:bootstrap:start").length === 0
) {
  performance.mark("framework-bench:bootstrap:start");
}

export default function Home() {
  const initialCount = createAsync(() => getCounter());
  const [count, setCount] = createSignal(0);

  createEffect(() => {
    const value = initialCount();
    if (typeof value === "number") {
      setCount(value);
    }
  });

  function readRenderedCount() {
    return (
      document
        .querySelector("[data-framework-bench-counter]")
        ?.textContent?.trim() ?? null
    );
  }

  onMount(() => {
    window.__frameworkBench = {
      framework: "solidstart",
      async writeCount(value: number) {
        setCount(value);
        await new Promise((resolve) => queueMicrotask(resolve));
      },
      readCount() {
        return readRenderedCount();
      },
    };

    performance.mark("framework-bench:bootstrap:ready");
    performance.measure(
      "framework-bench:bootstrap",
      "framework-bench:bootstrap:start",
      "framework-bench:bootstrap:ready"
    );

    return () => {
      delete window.__frameworkBench;
    };
  });

  async function requestCount(path: string) {
    try {
      const response = await fetch(path, { method: "POST" });
      if (!response.ok) {
        throw new Error(`Counter request failed: ${response.status}`);
      }

      const payload = await response.json();
      setCount(payload.count);
    } catch (error) {
      console.error(error);
    }
  }

  return (
    <main class="page-shell">
      <section class="panel">
        <h1 class="title">SolidStart Counter</h1>
        <p class="subtitle">
          The count lives in server state. Buttons post to the server and hydrate
          the returned value.
        </p>

        <div class="card">
          <p data-framework-bench-counter class="count">
            {count()}
          </p>

          <div class="actions">
            <button onClick={() => void requestCount("/api/counter/decrement")}>-</button>
            <button onClick={() => void requestCount("/api/counter/reset")}>Reset</button>
            <button onClick={() => void requestCount("/api/counter/increment")}>+</button>
          </div>
        </div>
      </section>
    </main>
  );
}
