<script setup lang="ts">
if (
  import.meta.client &&
  performance.getEntriesByName("framework-bench:bootstrap:start").length === 0
) {
  performance.mark("framework-bench:bootstrap:start");
}

type CounterResponse = {
  count: number;
};

type FrameworkBench = {
  framework: string;
  writeCount(value: number): void | Promise<void>;
  readCount(): string | null | Promise<string | null>;
};

const { data } = await useFetch<CounterResponse>("/api/counter/current", {
  key: "counter-current",
});

const count = ref(data.value?.count ?? 0);
let pendingWrite: null | (() => void) = null;

watch(count, async () => {
  if (!pendingWrite) {
    return;
  }

  await nextTick();
  const resolve = pendingWrite;
  pendingWrite = null;
  resolve();
});

function readRenderedCount() {
  return document
    .querySelector("[data-framework-bench-counter]")
    ?.textContent?.trim() ?? null;
}

onMounted(() => {
  const benchWindow = window as Window & { __frameworkBench?: FrameworkBench };

  benchWindow.__frameworkBench = {
    framework: "nuxt",
    writeCount(value) {
      return new Promise<void>((resolve) => {
        pendingWrite = resolve;
        count.value = value;
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
});

onBeforeUnmount(() => {
  const benchWindow = window as Window & { __frameworkBench?: FrameworkBench };
  delete benchWindow.__frameworkBench;
});

async function requestCount(path: string) {
  try {
    const payload = await $fetch<CounterResponse>(path, { method: "POST" });
    count.value = payload.count;
  } catch (error) {
    console.error(error);
  }
}
</script>

<template>
  <main class="page-shell">
    <section class="panel">
      <h1 class="title">Nuxt Counter</h1>
      <p class="subtitle">
        The count lives in server state. Buttons post to the server and hydrate
        the returned value.
      </p>

      <div class="card">
        <p data-framework-bench-counter class="count">{{ count }}</p>

        <div class="actions">
          <button @click="requestCount('/api/counter/decrement')">-</button>
          <button @click="requestCount('/api/counter/reset')">Reset</button>
          <button @click="requestCount('/api/counter/increment')">+</button>
        </div>
      </div>
    </section>
  </main>
</template>

<style scoped>
html,
body {
  margin: 0;
  min-height: 100%;
  font-family: "Helvetica Neue", Helvetica, Arial, sans-serif;
  background: #f3f4f6;
  color: #0f172a;
}

body {
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
