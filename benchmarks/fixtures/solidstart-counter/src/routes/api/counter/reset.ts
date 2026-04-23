import { resetCount } from "../../../lib/counter";

export function POST() {
  return new Response(JSON.stringify({ count: resetCount() }), {
    headers: {
      "content-type": "application/json",
    },
  });
}
