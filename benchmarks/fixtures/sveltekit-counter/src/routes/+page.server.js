import { getCount } from "$lib/server/counter";

export function load() {
  return {
    initialCount: getCount(),
  };
}
