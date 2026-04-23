import { json } from "@sveltejs/kit";
import { getCount } from "$lib/server/counter";

export function GET() {
  return json({ count: getCount() });
}
