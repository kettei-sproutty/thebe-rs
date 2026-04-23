import { incrementCount } from "../../../lib/counter";

export function POST() {
  return new Response(JSON.stringify({ count: incrementCount() }), {
    headers: {
      "content-type": "application/json",
    },
  });
}
