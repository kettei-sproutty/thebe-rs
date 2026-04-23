import { json } from "@sveltejs/kit";
import { decrementCount } from "$lib/server/counter";

export function POST() {
  return json({ count: decrementCount() });
}
