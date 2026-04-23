import { json } from "@sveltejs/kit";
import { incrementCount } from "$lib/server/counter";

export function POST() {
  return json({ count: incrementCount() });
}
