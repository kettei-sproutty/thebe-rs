<script>
  import { onMount, tick } from "svelte";

  let { data } = $props();
  let count = $state(0);

  $effect(() => {
    count = data.initialCount;
  });

  if (
    typeof window !== "undefined" &&
    performance.getEntriesByName("framework-bench:bootstrap:start").length === 0
  ) {
    performance.mark("framework-bench:bootstrap:start");
  }

  function readRenderedCount() {
    return (
      document
        .querySelector("[data-framework-bench-counter]")
        ?.textContent?.trim() ?? null
    );
  }

  onMount(() => {
    window.__frameworkBench = {
      framework: "sveltekit",
      async writeCount(value) {
        count = value;
        await tick();
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

  async function requestCount(path) {
    try {
      const response = await fetch(path, { method: "POST" });
      if (!response.ok) {
        throw new Error(`Counter request failed: ${response.status}`);
      }

      const payload = await response.json();
      count = payload.count;
    } catch (error) {
      console.error(error);
    }
  }
</script>

<svelte:head>
  <title>SvelteKit Counter</title>
</svelte:head>

<main class="page-shell">
  <section class="panel">
    <h1 class="title">SvelteKit Counter</h1>
    <p class="subtitle">
      The count lives in server state. Buttons post to the server and hydrate the returned value.
    </p>

    <div class="card">
      <p data-framework-bench-counter class="count">{count}</p>

      <div class="actions">
        <button onclick={() => void requestCount("/api/counter/decrement")}>-</button>
        <button onclick={() => void requestCount("/api/counter/reset")}>Reset</button>
        <button onclick={() => void requestCount("/api/counter/increment")}>+</button>
      </div>
    </div>
  </section>
</main>

<style>
  :global(html),
  :global(body) {
    margin: 0;
    min-height: 100%;
    font-family: "Helvetica Neue", Helvetica, Arial, sans-serif;
    background: #f3f4f6;
    color: #0f172a;
  }

  :global(body) {
    min-height: 100vh;
  }

  .page-shell {
    min-height: 100vh;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 3rem 1.5rem;
  }

  .panel {
    width: min(32rem, 100%);
    text-align: center;
  }

  .title {
    margin: 0 0 1rem;
    font-size: 2.5rem;
  }

  .subtitle {
    margin: 0 0 2rem;
    color: #475569;
    line-height: 1.6;
  }

  .card {
    background: white;
    border-radius: 1.25rem;
    border: 1px solid #cbd5e1;
    box-shadow: 0 10px 30px rgba(15, 23, 42, 0.06);
    padding: 2rem;
  }

  .count {
    margin: 0 0 1.5rem;
    font-size: 4.5rem;
    font-weight: 800;
    color: #4338ca;
  }

  .actions {
    display: flex;
    justify-content: center;
    gap: 1rem;
  }

  .actions button {
    border: none;
    border-radius: 0.85rem;
    background: #e2e8f0;
    color: #0f172a;
    padding: 0.85rem 1.1rem;
    font-size: 1rem;
    font-weight: 700;
    cursor: pointer;
  }

  .actions button:hover {
    background: #cbd5e1;
  }
</style>
