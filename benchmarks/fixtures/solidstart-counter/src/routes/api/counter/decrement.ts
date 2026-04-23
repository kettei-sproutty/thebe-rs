import { decrementCount } from "../../../lib/counter";

export function POST() {
  return new Response(JSON.stringify({ count: decrementCount() }), {
    headers: {
      "content-type": "application/json",
    },
  });
}
