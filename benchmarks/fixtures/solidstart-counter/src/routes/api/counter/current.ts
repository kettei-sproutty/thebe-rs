import { getCount } from "../../../lib/counter";

export function GET() {
  return new Response(JSON.stringify({ count: getCount() }), {
    headers: {
      "content-type": "application/json",
    },
  });
}
