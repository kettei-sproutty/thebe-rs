"use client";

import { useEffect, useRef, useState } from "react";

if (
  typeof performance !== "undefined" &&
  performance.getEntriesByName("framework-bench:bootstrap:start").length === 0
) {
  performance.mark("framework-bench:bootstrap:start");
}

export default function CounterClient({ initialCount, title }) {
  const [count, setCount] = useState(initialCount);
  const pendingWrite = useRef(null);

  useEffect(() => {
    if (!pendingWrite.current) {
      return;
    }

    const resolve = pendingWrite.current;
    pendingWrite.current = null;
    resolve();
  }, [count]);

  useEffect(() => {
    function readRenderedCount() {
      return (
        document
          .querySelector("[data-framework-bench-counter]")
          ?.textContent?.trim() ?? null
      );
    }

    window.__frameworkBench = {
      framework: "nextjs",
      writeCount(value) {
        return new Promise((resolve) => {
          pendingWrite.current = resolve;
          setCount(value);
        });
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
  }, []);

  async function requestCount(path) {
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
    <main className="page-shell">
      <section className="panel">
        <h1 className="title">{title}</h1>
        <p className="subtitle">
          The count lives in server state. Buttons post to the server and hydrate
          the returned value.
        </p>

        <div className="card">
          <p data-framework-bench-counter className="count">
            {count}
          </p>

          <div className="actions">
            <button onClick={() => void requestCount("/api/counter/decrement")}>-</button>
            <button onClick={() => void requestCount("/api/counter/reset")}>Reset</button>
            <button onClick={() => void requestCount("/api/counter/increment")}>+</button>
          </div>
        </div>
      </section>
    </main>
  );
}
