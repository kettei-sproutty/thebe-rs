import { json } from "@sveltejs/kit";
import { resetCount } from "$lib/server/counter";

export function POST() {
  return json({ count: resetCount() });
}
